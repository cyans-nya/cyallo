use super::{Instruction, InstructionId};
use crate::Span;

/// The executable edge that reached a statement whose block emitted no code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementArrival {
    Entry,
    Next(InstructionId),
    Jump(InstructionId),
    Branch { from: InstructionId, taken: bool },
}

impl StatementArrival {
    pub(crate) fn is_backedge_to(self, target: InstructionId) -> bool {
        matches!(self, Self::Jump(from) | Self::Branch { from, .. } if from >= target)
    }

    pub(crate) fn reaches(self, target: InstructionId, code: &[Instruction]) -> bool {
        match self {
            Self::Entry => target == InstructionId(0),
            Self::Next(from) => {
                from.0.checked_add(1).map(InstructionId) == Some(target)
                    && code.get(from.index()).is_some_and(|instruction| {
                        !matches!(
                            instruction,
                            Instruction::Jump(_)
                                | Instruction::Branch(..)
                                | Instruction::Return(_)
                                | Instruction::Trap
                        )
                    })
            }
            Self::Jump(from) => code.get(from.index()) == Some(&Instruction::Jump(target)),
            Self::Branch { from, taken } => {
                let Some(&Instruction::Branch(_, yes, no)) = code.get(from.index()) else {
                    return false;
                };
                target == if taken { yes } else { no }
            }
        }
    }
}

/// A source statement boundary reached before executing its logical PC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatementBoundary {
    pub instruction: InstructionId,
    pub span: Span,
    /// An empty block or an implicit fallthrough can share another statement's PC.
    /// In that case the marker applies only to the recorded incoming edge.
    pub arrival: Option<StatementArrival>,
}

impl StatementBoundary {
    pub(super) fn order(&self) -> (InstructionId, bool) {
        (self.instruction, self.arrival.is_none())
    }
}
