//! Place scalar values in reusable interpreter frame slots from verified lifetimes.

use super::{Instruction, InstructionId, Register, StorageState};
use std::{cmp::Reverse, collections::BinaryHeap};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FrameSlot(usize);

impl FrameSlot {
    pub fn index(self) -> usize {
        self.0
    }
}

#[derive(Debug, Default)]
pub struct FrameLayout {
    slots: Box<[Option<FrameSlot>]>,
    slot_count: usize,
}

impl FrameLayout {
    pub fn slot_count(&self) -> usize {
        self.slot_count
    }

    /// Registers with no reachable storage lifetime have no frame slot.
    pub fn slot(&self, register: Register) -> Option<FrameSlot> {
        self.slots.get(register.index()).copied().flatten()
    }
}

#[derive(Clone, Copy)]
struct LiveRange {
    first: InstructionId,
    last: InstructionId,
}

pub(super) struct LiveRanges {
    bounds: Vec<Option<LiveRange>>,
}

impl LiveRanges {
    pub(super) fn new(registers: usize) -> Self {
        Self {
            bounds: vec![None; registers],
        }
    }

    fn observe(&mut self, register: usize, pc: InstructionId) {
        let range = self.bounds[register].get_or_insert(LiveRange {
            first: pc,
            last: pc,
        });
        range.first = range.first.min(pc);
        range.last = range.last.max(pc);
    }

    pub(super) fn observe_boundary(&mut self, pc: InstructionId, state: &[StorageState]) {
        for (index, state) in state.iter().enumerate() {
            if *state != StorageState::Dead {
                self.observe(index, pc);
            }
        }
    }

    pub(super) fn observe_instruction(&mut self, pc: InstructionId, instruction: Instruction) {
        if let Instruction::StorageLive(reg)
        | Instruction::StorageDead(reg)
        | Instruction::Return(Some(reg)) = instruction
        {
            self.observe(reg.index(), pc);
        }
    }

    pub(super) fn place(self) -> FrameLayout {
        let mut slots = vec![None; self.bounds.len()];
        let mut intervals: Vec<_> = self
            .bounds
            .into_iter()
            .enumerate()
            .filter_map(|(index, range)| range.map(|range| (index, range)))
            .collect();
        intervals.sort_unstable_by_key(|(index, range)| (range.first, *index));
        let mut assigned = BinaryHeap::new();
        let mut slot_count = 0;
        for (index, range) in intervals {
            // Within a block, storage can change only at the observed markers
            // and return. The enclosing PC intervals may span gaps and backedges.
            // Disjoint intervals therefore never alias co-live registers.
            let slot = if let Some(&Reverse((last, slot))) = assigned.peek()
                && last < range.first
            {
                assigned.pop();
                slot
            } else {
                let slot = FrameSlot(slot_count);
                slot_count += 1;
                slot
            };
            slots[index] = Some(slot);
            assigned.push(Reverse((range.last, slot)));
        }
        FrameLayout {
            slots: slots.into_boxed_slice(),
            slot_count,
        }
    }
}
