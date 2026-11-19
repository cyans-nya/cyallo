//! Assign executable PCs and resolve source and statement locations.

use super::{
    Instruction, InstructionId, LowerError, StatementArrival, StatementBoundary,
    cfg::{Block, BlockId, Exit, Located},
    lower::lower_index,
};
use crate::Span;

pub(super) struct Body {
    pub instructions: Vec<Instruction>,
    pub spans: Box<[Span]>,
    pub statements: Box<[StatementBoundary]>,
}

fn width(block: &Block) -> usize {
    block.operations.len() + usize::from(!matches!(block.exit.value, Exit::Fallthrough(_)))
}

fn statement(
    out: &mut Vec<StatementBoundary>,
    instruction: InstructionId,
    span: Span,
    arrival: Option<StatementArrival>,
) {
    out.push(StatementBoundary {
        instruction,
        span,
        arrival,
    });
}

/// Follow implicitly forwarded labels until reaching a block with code.
fn edge_statements(
    out: &mut Vec<StatementBoundary>,
    blocks: &[Block],
    offsets: &[InstructionId],
    mut target: BlockId,
    arrival: StatementArrival,
) {
    while width(&blocks[target.0]) == 0 {
        let block = &blocks[target.0];
        for marker in &block.statements {
            statement(out, offsets[target.0], marker.span, Some(arrival));
        }
        let Exit::Fallthrough(next) = block.exit.value else {
            // Every explicit terminator emits an instruction.
            unreachable!("empty block must have an implicit successor");
        };
        target = next;
    }
}

pub(super) fn assign(
    prologue: Vec<Located<Instruction>>,
    blocks: Vec<Block>,
    routine: usize,
) -> Result<Body, LowerError> {
    let pc = |index| lower_index(routine, index).map(InstructionId);
    let mut next = prologue.len();
    let mut offsets = Vec::with_capacity(blocks.len());
    for block in &blocks {
        offsets.push(pc(next)?);
        next += width(block);
    }
    let mut instructions = Vec::with_capacity(next);
    let mut spans = Vec::with_capacity(next);
    let mut statements = Vec::new();
    let arrival = match prologue.len().checked_sub(1) {
        Some(index) => StatementArrival::Next(pc(index)?),
        None => StatementArrival::Entry,
    };
    edge_statements(&mut statements, &blocks, &offsets, BlockId(0), arrival);
    for operation in prologue {
        instructions.push(operation.value);
        spans.push(operation.span);
    }

    for block in &blocks {
        let width = width(block);
        if width == 0 {
            // Empty blocks share their successor's first PC.
            continue;
        }
        let start = instructions.len();
        for marker in &block.statements {
            if marker.before < width {
                statement(
                    &mut statements,
                    pc(start + marker.before)?,
                    marker.span,
                    None,
                );
            }
        }
        for operation in &block.operations {
            instructions.push(operation.value);
            spans.push(operation.span);
        }
        let end = pc(start + width - 1)?;
        let terminator = match block.exit.value {
            Exit::Fallthrough(to) => {
                let arrival = StatementArrival::Next(end);
                for marker in block
                    .statements
                    .iter()
                    .filter(|marker| marker.before == width)
                {
                    statement(&mut statements, offsets[to.0], marker.span, Some(arrival));
                }
                edge_statements(&mut statements, &blocks, &offsets, to, arrival);
                continue;
            }
            Exit::Jump(to) => {
                edge_statements(
                    &mut statements,
                    &blocks,
                    &offsets,
                    to,
                    StatementArrival::Jump(end),
                );
                Instruction::Jump(offsets[to.0])
            }
            Exit::Branch(condition, yes, no) => {
                for (taken, to) in [(true, yes), (false, no)] {
                    let arrival = StatementArrival::Branch { from: end, taken };
                    edge_statements(&mut statements, &blocks, &offsets, to, arrival);
                }
                Instruction::Branch(condition, offsets[yes.0], offsets[no.0])
            }
            Exit::Return(value) => Instruction::Return(value),
            Exit::Trap => Instruction::Trap,
        };
        instructions.push(terminator);
        spans.push(block.exit.span);
    }
    // Incoming-edge markers precede markers in the instruction's own block.
    // Stable sorting preserves the source order within one incoming edge.
    statements.sort_by_key(StatementBoundary::order);
    Ok(Body {
        instructions,
        spans: spans.into_boxed_slice(),
        statements: statements.into_boxed_slice(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::verified::{
        ExecutionLimits, ExecutionStatus, FORMAT_VERSION, FunctionId, FunctionImage, ModuleImage,
        Register, Scalar, ScalarType, cfg::Statement, encode, lifetimes,
    };

    #[test]
    fn forwarded_labels_keep_statements_on_the_taken_edge() {
        let condition = Register(0);
        let only_true = Span::new(1, 2);
        let shared = Span::new(2, 3);
        let mut blocks = vec![
            Block {
                operations: vec![],
                statements: vec![],
                exit: Located {
                    value: Exit::Branch(condition, BlockId(1), BlockId(2)),
                    span: Span::default(),
                },
            },
            Block {
                operations: vec![],
                statements: vec![Statement {
                    before: 0,
                    span: only_true,
                }],
                exit: Located {
                    value: Exit::Fallthrough(BlockId(2)),
                    span: only_true,
                },
            },
            Block {
                operations: vec![],
                statements: vec![Statement {
                    before: 0,
                    span: shared,
                }],
                exit: Located {
                    value: Exit::Return(Some(condition)),
                    span: shared,
                },
            },
        ];
        let registers = [ScalarType::Bool.into()];
        let prologue = lifetimes::insert(&mut blocks, &[condition], &mut [], &[], &registers);
        let body = assign(prologue, blocks, 0).unwrap();
        assert_eq!(
            body.instructions,
            [
                Instruction::Branch(condition, InstructionId(1), InstructionId(1)),
                Instruction::Return(Some(condition)),
            ],
        );
        let module = ModuleImage {
            version: FORMAT_VERSION,
            entry: FunctionId(0),
            shapes: Box::new([]),
            signatures: Box::new([]),
            globals: Box::new([]),
            managed: Box::new([]),
            functions: Box::new([FunctionImage {
                registers: Box::new(registers),
                parameters: Box::new([condition]),
                result: Some(ScalarType::Bool.into()),
                calls: Box::new([]),
                lists: Box::new([]),
                texts: Box::new([]),
                outputs: Box::new([]),
                words: encode(body.instructions),
                spans: body.spans,
                statements: body.statements,
            }]),
        }
        .verify()
        .unwrap();
        for (value, expected) in [(true, vec![only_true, shared]), (false, vec![shared])] {
            let mut activation = module
                .start(
                    &[Scalar::Bool(value)],
                    ExecutionLimits { fuel: 2, frames: 1 },
                )
                .unwrap();
            assert!(matches!(activation.resume(1), ExecutionStatus::Paused(_)));
            let spans: Vec<_> = activation
                .statement_boundaries()
                .map(|marker| marker.span)
                .collect();
            assert_eq!(spans, expected);
            assert_eq!(activation.instructions_executed(), 1);
            assert_eq!(
                activation.resume(1),
                ExecutionStatus::Returned(Some(Scalar::Bool(value)))
            );
        }
    }
}
