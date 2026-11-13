//! Immutable resume metadata. Optimizing entries are intentionally selective.
use crate::{
    Span,
    ir::{Function, Op, Reg},
};
use std::collections::BTreeSet;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueLocation {
    CanonicalSlot(Reg),
}
#[derive(Debug, Clone)]
pub struct OsrEntry {
    pub pc: usize,
    pub span: Span,
    pub values: Box<[(Reg, ValueLocation)]>,
}
#[derive(Debug, Clone)]
struct LiveCheckpoint {
    pc: usize,
    bits: Box<[u64]>,
}
#[derive(Debug, Clone)]
struct LiveBlock {
    start: usize,
    end: usize,
    checkpoints: Box<[LiveCheckpoint]>,
}
#[derive(Debug, Clone, Default)]
pub struct ResumeMetadata {
    live_blocks: Box<[LiveBlock]>,
    pub loop_headers: Box<[usize]>,
    pub entries: Box<[OsrEntry]>,
    pub poll_points: Box<[bool]>,
}
impl ResumeMetadata {
    pub fn build(function: &Function) -> Self {
        let len = function.code.len();
        let mut loops = BTreeSet::new();
        let mut polls = vec![false; len];
        for pc in (0..len).step_by(64) {
            polls[pc] = true;
        }
        if len > 0 {
            polls[0] = true;
        }
        for (pc, inst) in function.code.iter().enumerate() {
            let targets: Vec<usize> = match inst.op {
                Op::Jump(to) => vec![to],
                Op::Branch(_, yes, no) => vec![yes, no],
                _ => vec![],
            };
            for target in targets {
                if target < len {
                    polls[target] = true;
                    if target <= pc {
                        loops.insert(target);
                    }
                }
            }
        }
        let mut pcs = loops.clone();
        pcs.insert(0);
        let live_blocks = live_inputs(function);
        let entries = pcs
            .into_iter()
            .filter(|pc| *pc < len)
            .map(|pc| {
                let bits = checkpoint_bits(&live_blocks, pc).unwrap_or(&[]);
                OsrEntry {
                    pc,
                    span: function.code[pc].span,
                    values: (0..function.registers.len())
                        .filter(|reg| {
                            bits.get(*reg / 64)
                                .is_some_and(|word| word & (1 << (*reg % 64)) != 0)
                        })
                        .map(|reg| (reg, ValueLocation::CanonicalSlot(reg)))
                        .collect(),
                }
            })
            .collect();
        Self {
            live_blocks: live_blocks.into_boxed_slice(),
            loop_headers: loops.into_iter().collect(),
            entries,
            poll_points: polls.into_boxed_slice(),
        }
    }
    pub fn accepts(&self, pc: usize) -> bool {
        self.entries
            .binary_search_by_key(&pc, |entry| entry.pc)
            .is_ok()
    }
    pub fn live_at(&self, function: &Function, pc: usize, output: &mut Vec<u64>) {
        let block_index = self
            .live_blocks
            .partition_point(|block| block.start <= pc)
            .checked_sub(1);
        let Some(block) = block_index.and_then(|index| self.live_blocks.get(index)) else {
            output.clear();
            return;
        };
        if pc >= block.end {
            output.clear();
            return;
        }
        let checkpoint_index = block
            .checkpoints
            .partition_point(|checkpoint| checkpoint.pc < pc);
        let Some(checkpoint) = block.checkpoints.get(checkpoint_index) else {
            output.clear();
            return;
        };
        output.clear();
        output.extend_from_slice(&checkpoint.bits);
        for instruction in (pc..checkpoint.pc).rev() {
            apply_live(&mut output[..], &function.code[instruction].op);
        }
    }
}
pub fn uses_defs(op: &Op) -> (Vec<Reg>, Option<Reg>) {
    match op {
        Op::Yield(destination, value) => (vec![*value], Some(*destination)),
        Op::Statement | Op::Nop | Op::Jump(_) => (vec![], None),
        Op::Drop(a) => (vec![*a], None),
        Op::Literal(r, _)
        | Op::LiteralString(r, _)
        | Op::Frozen(r, _)
        | Op::Address(r, _)
        | Op::ServiceKey(r, _) => (vec![], Some(*r)),
        Op::Lookup(r, _, rs) | Op::Service(r, _, rs) => (rs.clone(), Some(*r)),
        Op::TraitBox(r, a, _) => (vec![*a], Some(*r)),
        Op::TraitCall(r, a, _, rs) => {
            let mut uses = rs.clone();
            uses.push(*a);
            (uses, *r)
        }
        Op::ArrayLiteral(r, rs)
        | Op::Aggregate(r, rs)
        | Op::FunctionValue(r, _, rs)
        | Op::Builtin(r, _, rs) => (rs.clone(), Some(*r)),
        Op::Field(r, a, _) | Op::Extract(r, a, _) => (vec![*a], Some(*r)),
        Op::Invoke(r, a, rs) => {
            let mut uses = rs.clone();
            uses.push(*a);
            (uses, *r)
        }
        Op::Length(r, a)
        | Op::ManagedAddress(r, a)
        | Op::Load(r, a)
        | Op::Take(r, a)
        | Op::FrameLoad(r, a)
        | Op::FrameStore(r, a)
        | Op::Move(r, a)
        | Op::Unary(r, _, a) => (vec![*a], Some(*r)),
        Op::Store(a, r) => (vec![*a, *r], None),
        Op::Input(a, _) | Op::CheckStep(a) => (vec![*a], None),
        Op::FrameInput(r, _) => (vec![], Some(*r)),
        Op::Index(r, a, rs, _) => {
            let mut uses = rs.clone();
            uses.push(*a);
            (uses, Some(*r))
        }
        Op::Binary(r, _, a, b) => (vec![*a, *b], Some(*r)),
        Op::Branch(r, _, _) => (vec![*r], None),
        Op::Call(r, _, rs) => (rs.clone(), *r),
        Op::Return(r) => (r.iter().copied().collect(), None),
        Op::Output(rs, _) => (rs.clone(), None),
    }
}
fn live_inputs(function: &Function) -> Vec<LiveBlock> {
    let n = function.code.len();
    if n == 0 {
        return vec![];
    }
    let words = function.registers.len().div_ceil(64);
    let mut leaders = BTreeSet::from([0]);
    for (pc, instruction) in function.code.iter().enumerate() {
        match instruction.op {
            Op::Jump(target) => {
                if target < n {
                    leaders.insert(target);
                }
                if pc + 1 < n {
                    leaders.insert(pc + 1);
                }
            }
            Op::Branch(_, yes, no) => {
                if yes < n {
                    leaders.insert(yes);
                }
                if no < n {
                    leaders.insert(no);
                }
                if pc + 1 < n {
                    leaders.insert(pc + 1);
                }
            }
            Op::Return(_) if pc + 1 < n => {
                leaders.insert(pc + 1);
            }
            _ => {}
        }
    }

    let starts: Vec<_> = leaders.into_iter().collect();
    let mut block_for_pc = vec![0usize; n];
    let mut ranges = Vec::with_capacity(starts.len());
    for (index, &start) in starts.iter().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(n);
        block_for_pc[start..end].fill(index);
        ranges.push((start, end));
    }
    let mut successors = vec![Vec::new(); ranges.len()];
    for (index, &(_, end)) in ranges.iter().enumerate() {
        let last = end - 1;
        let mut add = |target: usize| {
            if target < n {
                let successor = block_for_pc[target];
                if !successors[index].contains(&successor) {
                    successors[index].push(successor);
                }
            }
        };
        match function.code[last].op {
            Op::Return(_) => {}
            Op::Jump(target) => add(target),
            Op::Branch(_, yes, no) => {
                add(yes);
                add(no);
            }
            _ if end < n => add(end),
            _ => {}
        }
    }

    let mut uses = vec![vec![0u64; words]; ranges.len()];
    let mut defs = vec![vec![0u64; words]; ranges.len()];
    for (block, &(start, end)) in ranges.iter().enumerate() {
        for instruction in &function.code[start..end] {
            let (operands, definition) = uses_defs(&instruction.op);
            for register in operands {
                let mask = 1u64 << (register % 64);
                if defs[block][register / 64] & mask == 0 {
                    uses[block][register / 64] |= mask;
                }
            }
            if let Some(register) = definition {
                defs[block][register / 64] |= 1u64 << (register % 64);
            }
        }
    }

    let mut inputs = vec![vec![0u64; words]; ranges.len()];
    let mut outgoing = vec![0u64; words];
    loop {
        let mut changed = false;
        for block in (0..ranges.len()).rev() {
            outgoing.fill(0);
            for &successor in &successors[block] {
                for (out, input) in outgoing.iter_mut().zip(&inputs[successor]) {
                    *out |= *input;
                }
            }
            for word in 0..words {
                let next = uses[block][word] | (outgoing[word] & !defs[block][word]);
                if inputs[block][word] != next {
                    inputs[block][word] = next;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    ranges
        .into_iter()
        .enumerate()
        .map(|(index, (start, end))| {
            let mut live = vec![0u64; words];
            for &successor in &successors[index] {
                for (out, input) in live.iter_mut().zip(&inputs[successor]) {
                    *out |= *input;
                }
            }
            let mut checkpoints = vec![LiveCheckpoint {
                pc: end,
                bits: live.clone().into_boxed_slice(),
            }];
            for pc in (start..end).rev() {
                apply_live(&mut live, &function.code[pc].op);
                if pc == start
                    || function
                        .gc
                        .instructions
                        .get(pc)
                        .is_some_and(|effects| effects.may_collect)
                {
                    checkpoints.push(LiveCheckpoint {
                        pc,
                        bits: live.clone().into_boxed_slice(),
                    });
                }
            }
            checkpoints.sort_by_key(|checkpoint| checkpoint.pc);
            LiveBlock {
                start,
                end,
                checkpoints: checkpoints.into_boxed_slice(),
            }
        })
        .collect()
}

fn checkpoint_bits(blocks: &[LiveBlock], pc: usize) -> Option<&[u64]> {
    let index = blocks
        .partition_point(|block| block.start <= pc)
        .checked_sub(1)?;
    let block = blocks.get(index)?;
    block
        .checkpoints
        .binary_search_by_key(&pc, |checkpoint| checkpoint.pc)
        .ok()
        .map(|checkpoint| block.checkpoints[checkpoint].bits.as_ref())
}

fn apply_live(live: &mut [u64], op: &Op) {
    let (operands, definition) = uses_defs(op);
    if let Some(register) = definition {
        live[register / 64] &= !(1u64 << (register % 64));
    }
    for register in operands {
        live[register / 64] |= 1u64 << (register % 64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Instruction;
    use cyallo_ast::{Literal, RoutineKind, Span, Type};

    #[test]
    fn straight_line_liveness_uses_sparse_checkpoints() {
        let count = 16_000;
        let mut code = (0..count)
            .map(|register| Instruction {
                op: Op::Literal(register, Literal::Integer(0)),
                span: Span::default(),
            })
            .collect::<Vec<_>>();
        code.push(Instruction {
            op: Op::Return(None),
            span: Span::default(),
        });
        let function = Function {
            generator: None,
            symbol: "loop".into(),
            origin: Default::default(),
            attributes: Default::default(),
            analysis_error: None,
            resume: ResumeMetadata::default(),
            gc: Default::default(),
            kind: RoutineKind::Procedure,
            span: Span::default(),
            pure: false,
            effects: Default::default(),
            name: "large".into(),
            locals: vec![],
            local_storage: Box::default(),
            params: vec![],
            result: Type::Void,
            registers: vec![Type::Integer; count],
            code,
        };
        let resume = ResumeMetadata::build(&function);
        let stored_words = resume
            .live_blocks
            .iter()
            .flat_map(|block| block.checkpoints.iter())
            .map(|checkpoint| checkpoint.bits.len())
            .sum::<usize>();
        let per_instruction_words = function.registers.len().div_ceil(64);
        let eager_words = function.code.len() * per_instruction_words;
        assert!(stored_words * 32 < eager_words);

        let mut live = Vec::new();
        resume.live_at(&function, 100, &mut live);
        assert_eq!(live.len(), per_instruction_words);
        assert!(live.iter().all(|word| *word == 0));
    }
}
