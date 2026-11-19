//! Place scalar storage events using backwards liveness over the lowered CFG.

use super::{
    CallSite, Instruction, Register, RegisterType,
    cfg::{Block, BlockId, Exit, Located},
};
use std::collections::HashSet;

type Live = HashSet<Register>;

struct Facts {
    writes: Live,
    live_in: Live,
}

#[derive(Default)]
struct Events {
    open: Option<Register>,
    close: Vec<Register>,
}

fn ordered(registers: impl Iterator<Item = Register>, types: &[RegisterType]) -> Vec<Register> {
    let mut registers: Vec<_> = registers.collect();
    // End all loans before making their owners' homes available for reuse.
    registers.sort_unstable_by_key(|reg| (!types[reg.index()].is_loan(), *reg));
    registers.dedup();
    registers
}

fn liveness(
    blocks: &[Block],
    calls: &[CallSite],
    lists: &[Box<[Register]>],
    types: &[RegisterType],
) -> Vec<Facts> {
    let mut facts = Vec::with_capacity(blocks.len());
    let mut predecessors = vec![Vec::new(); blocks.len()];
    let mut pending = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        let mut writes = Live::new();
        let mut live_in = Live::new();
        for operation in &block.operations {
            let access = operation.value.register_access(calls, lists);
            live_in.extend(
                access
                    .reads(types)
                    .filter(|register| !writes.contains(register)),
            );
            writes.extend(access.write);
        }
        live_in.extend(
            block
                .exit
                .value
                .read()
                .filter(|register| !writes.contains(register)),
        );
        pending.extend(live_in.iter().map(|&register| (index, register)));
        for successor in block.exit.value.successors().into_iter().flatten() {
            predecessors[successor.0].push(index);
        }
        facts.push(Facts { writes, live_in });
    }

    // Propagate each block and register pair once, including around backedges.
    while let Some((block, register)) = pending.pop() {
        for &predecessor in &predecessors[block] {
            let previous = &mut facts[predecessor];
            if !previous.writes.contains(&register) && previous.live_in.insert(register) {
                pending.push((predecessor, register));
            }
        }
    }
    facts
}

fn place_in_block(
    block: &mut Block,
    calls: &mut [CallSite],
    lists: &[Box<[Register]>],
    types: &[RegisterType],
    mut live: Live,
) {
    let mut events: Vec<_> = (0..block.operations.len())
        .map(|_| Events::default())
        .collect();
    for (operation, event) in block.operations.iter().zip(&mut events).rev() {
        if let Instruction::Call(site) = operation.value {
            // Lowering gives each call instruction its own site. A dead scalar
            // result needs no storage, but the call itself can fail or run effects.
            let call = &mut calls[site.index()];
            if call.destination.is_some_and(|dst| !live.contains(&dst)) {
                call.destination = None;
            }
        }
        let access = operation.value.register_access(calls, lists);
        event.open = access
            .write
            .filter(|&dst| !access.reads(types).any(|src| src == dst));
        event.close = ordered(
            access
                .reads(types)
                .chain(access.write)
                .filter(|reg| !live.contains(reg)),
            types,
        );
        if let Some(dst) = access.write {
            live.remove(&dst);
        }
        live.extend(access.reads(types));
    }

    let original = std::mem::take(&mut block.operations);
    let mut statements = block.statements.iter_mut().peekable();
    for (index, (operation, event)) in original.into_iter().zip(events).enumerate() {
        while let Some(marker) = statements.next_if(|marker| marker.before == index) {
            marker.before = block.operations.len();
        }
        let span = operation.span;
        if let Some(register) = event.open {
            block.operations.push(Located {
                value: Instruction::StorageLive(register),
                span,
            });
        }
        block.operations.push(operation);
        block
            .operations
            .extend(event.close.into_iter().map(|register| Located {
                value: Instruction::StorageDead(register),
                span,
            }));
    }
    for marker in statements {
        marker.before = block.operations.len();
    }
}

/// Insert entry cleanup and branch-specific edge blocks before PC assignment.
pub(super) fn insert(
    blocks: &mut Vec<Block>,
    parameters: &[Register],
    calls: &mut [CallSite],
    lists: &[Box<[Register]>],
    types: &[RegisterType],
) -> Vec<Located<Instruction>> {
    let facts = liveness(blocks, calls, lists, types);
    let entry = &blocks[0];
    let span = entry.statements.first().map_or_else(
        || {
            entry
                .operations
                .first()
                .map_or(entry.exit.span, |operation| operation.span)
        },
        |marker| marker.span,
    );
    // Backedges to the first source block skip this one-time cleanup.
    let prologue = parameters
        .iter()
        .copied()
        .filter(|register| !facts[0].live_in.contains(register))
        .map(|register| Located {
            value: Instruction::StorageDead(register),
            span,
        })
        .collect();
    let block_count = blocks.len();
    let mut edges = Vec::new();
    for block in blocks.iter_mut() {
        let mut live: Live = block
            .exit
            .value
            .successors()
            .into_iter()
            .flatten()
            .flat_map(|to| facts[to.0].live_in.iter().copied())
            .collect();
        live.extend(block.exit.value.read());
        if let Exit::Branch(condition, yes, no) = block.exit.value {
            let span = block.exit.span;
            let mut cleanup_edge = |to: BlockId| {
                let close = ordered(live.difference(&facts[to.0].live_in).copied(), types);
                if close.is_empty() {
                    return to;
                }
                let id = BlockId(block_count + edges.len());
                edges.push(Block {
                    operations: close
                        .into_iter()
                        .map(|register| Located {
                            value: Instruction::StorageDead(register),
                            span,
                        })
                        .collect(),
                    statements: Vec::new(),
                    exit: Located {
                        value: Exit::Jump(to),
                        span,
                    },
                });
                id
            };
            block.exit.value = Exit::Branch(condition, cleanup_edge(yes), cleanup_edge(no));
        }
        place_in_block(block, calls, lists, types, live);
    }
    blocks.extend(edges);
    prologue
}
