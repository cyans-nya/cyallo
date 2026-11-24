//! Control flow used between checked IR and executable bytecode layout.

use super::{Instruction, InstructionId, LowerError, Register, lower::lower_index};
use crate::{
    Span,
    ir::{Instruction as CheckedInstruction, Op},
};
use std::ops::Range;

#[derive(Clone, Copy, Debug)]
pub(super) struct BlockId(pub usize);

#[derive(Clone, Copy)]
pub(super) enum Exit<R> {
    Fallthrough(BlockId),
    Jump(BlockId),
    Branch(R, BlockId, BlockId),
    Return(Option<R>),
    Trap,
}

impl<R: Copy> Exit<R> {
    pub fn successors(self) -> [Option<BlockId>; 2] {
        match self {
            Self::Fallthrough(to) | Self::Jump(to) => [Some(to), None],
            Self::Branch(_, yes, no) => [Some(yes), Some(no)],
            Self::Return(_) | Self::Trap => [None, None],
        }
    }

    pub fn read(self) -> Option<R> {
        match self {
            Self::Branch(condition, ..) => Some(condition),
            Self::Return(value) => value,
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Located<T> {
    pub value: T,
    pub span: Span,
}

pub(super) struct Statement {
    pub before: usize,
    pub span: Span,
}

pub(super) struct Block {
    pub operations: Vec<Located<Instruction>>,
    pub statements: Vec<Statement>,
    pub exit: Located<Exit<Register>>,
}

pub(super) struct CheckedBlock<'a> {
    pub start: usize,
    pub operations: &'a [CheckedInstruction],
    pub exit: Located<Exit<usize>>,
}

fn terminates(op: &Op) -> bool {
    matches!(
        op,
        Op::Jump(_) | Op::Branch(..) | Op::Return(_) | Op::Unreachable
    )
}

fn starts(code: &[CheckedInstruction], routine: usize) -> Result<Vec<usize>, LowerError> {
    let mut starts = vec![false; code.len()];
    let Some(entry) = starts.first_mut() else {
        return Err(LowerError::InvalidFallthrough { routine });
    };
    *entry = true;
    for (instruction, record) in code.iter().enumerate() {
        let mut mark = |target: usize| -> Result<(), LowerError> {
            let target_id = InstructionId(lower_index(routine, target)?);
            let start = starts.get_mut(target).ok_or(LowerError::InvalidTarget {
                routine,
                instruction,
                target: target_id,
            })?;
            *start = true;
            Ok(())
        };
        match record.op {
            Op::Jump(target) => mark(target)?,
            Op::Branch(_, yes, no) => {
                mark(yes)?;
                mark(no)?;
            }
            _ => {}
        }
        if terminates(&record.op)
            && let Some(next) = starts.get_mut(instruction + 1)
        {
            *next = true;
        }
    }
    Ok(starts
        .into_iter()
        .enumerate()
        .filter_map(|(index, start)| start.then_some(index))
        .collect())
}

fn source_exit(
    op: &Op,
    target: impl Fn(usize) -> BlockId,
    next: Option<BlockId>,
    return_sentinel: bool,
) -> Option<Exit<usize>> {
    Some(match *op {
        Op::Jump(to) => Exit::Jump(target(to)),
        Op::Branch(condition, yes, no) => Exit::Branch(condition, target(yes), target(no)),
        Op::Return(None) if return_sentinel => Exit::Trap,
        Op::Return(value) => Exit::Return(value),
        Op::Unreachable => Exit::Trap,
        _ => Exit::Fallthrough(next?),
    })
}

/// Keep reachable blocks in source order so implicit edges stay adjacent.
pub(super) fn reachable(
    code: &[CheckedInstruction],
    routine: usize,
    has_result: bool,
) -> Result<Vec<CheckedBlock<'_>>, LowerError> {
    let starts = starts(code, routine)?;
    let range = |block: BlockId| -> Range<usize> {
        starts[block.0]..starts.get(block.0 + 1).copied().unwrap_or(code.len())
    };
    let target = |pc| {
        let Ok(index) = starts.binary_search(&pc) else {
            // starts() validates and registers each branch target.
            unreachable!("checked target is missing its block");
        };
        BlockId(index)
    };
    let mut exits = vec![None; starts.len()];
    let mut work = vec![BlockId(0)];
    while let Some(block) = work.pop() {
        if exits[block.0].is_some() {
            continue;
        }
        let end = range(block).end;
        let last = &code[end - 1];
        let next = starts.get(block.0 + 1).map(|_| BlockId(block.0 + 1));
        // The current checked IR may append a void return sentinel after all
        // source returns. If reached in a value routine, it traps.
        let exit = source_exit(&last.op, target, next, has_result && end == code.len())
            .ok_or(LowerError::InvalidFallthrough { routine })?;
        exits[block.0] = Some(Located {
            value: exit,
            span: last.span,
        });
        work.extend(exit.successors().into_iter().flatten());
    }

    let mut next = 0;
    let labels: Vec<_> = exits
        .iter()
        .map(|exit| {
            exit.map(|_| {
                let label = BlockId(next);
                next += 1;
                label
            })
        })
        .collect();
    let remap = |block: BlockId| {
        let Some(label) = labels[block.0] else {
            // Every successor of a reachable block was visited above.
            unreachable!("reachable edge targets a pruned block");
        };
        label
    };
    Ok(exits
        .into_iter()
        .enumerate()
        .filter_map(|(index, exit)| {
            let exit = exit?;
            let range = range(BlockId(index));
            let last = &code[range.end - 1];
            let end = range.end - usize::from(terminates(&last.op));
            Some(CheckedBlock {
                start: range.start,
                operations: &code[range.start..end],
                exit: Located {
                    span: exit.span,
                    value: match exit.value {
                        Exit::Fallthrough(to) => Exit::Fallthrough(remap(to)),
                        Exit::Jump(to) => Exit::Jump(remap(to)),
                        Exit::Branch(condition, yes, no) => {
                            Exit::Branch(condition, remap(yes), remap(no))
                        }
                        Exit::Return(value) => Exit::Return(value),
                        Exit::Trap => Exit::Trap,
                    },
                },
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reachability_prunes_dead_blocks_but_rejects_reachable_fallthrough() {
        let instruction = |op| CheckedInstruction {
            op,
            span: Span::default(),
        };
        let valid = [
            instruction(Op::Jump(2)),
            instruction(Op::Nop),
            instruction(Op::Return(None)),
        ];
        let blocks = reachable(&valid, 0, false).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1].start, 2);
        assert!(matches!(blocks[0].exit.value, Exit::Jump(BlockId(1))));

        let invalid = [instruction(Op::Jump(1)), instruction(Op::Nop)];
        assert!(matches!(
            reachable(&invalid, 0, false),
            Err(LowerError::InvalidFallthrough { routine: 0 })
        ));
    }
}
