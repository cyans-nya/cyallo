//! Artifact boundary shared by the two emitters and the code publisher.

use super::{AbiLayout, ByteOffset, FunctionVersion, NativeTarget, PreparedFunction};
use crate::bytecode::verified::InstructionId;
use std::{fmt, sync::Arc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeTier {
    Winch,
    Cranelift(CraneliftProfile),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CraneliftProfile {
    /// Quick to compile: no Cranelift optimization passes.
    LowLatency,
    /// Quick to run: Cranelift's speed optimizations.
    Optimized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineRange {
    pub instruction: InstructionId,
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineTrap {
    pub offset: u32,
    pub code: cranelift::codegen::ir::TrapCode,
}

/// Final Cranelift machine-code layout, after lowering, register allocation,
/// and fixups. Logical recovery remains governed by the prepared plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CraneliftMachineInfo {
    pub blocks: Box<[u32]>,
    pub edges: Box<[(u32, u32)]>,
    pub traps: Box<[MachineTrap]>,
}

/// A relocation in the immutable linkage table consumed by native code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Relocation {
    pub instruction: InstructionId,
    pub offset: ByteOffset,
    pub kind: RelocationKind,
    pub target: FunctionVersion,
    pub addend: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocationKind {
    FunctionAddress,
}

impl RelocationKind {
    pub fn width(self) -> usize {
        match self {
            Self::FunctionAddress => AbiLayout::SCALARS.slot_bytes,
        }
    }
}

/// Unlinked bytes and final offsets. This type cannot expose a callable entry.
#[derive(Debug)]
pub struct UnlinkedImage {
    pub(super) plan: Arc<PreparedFunction>,
    pub(super) tier: NativeTier,
    pub(super) bytes: Box<[u8]>,
    pub(super) ranges: Box<[MachineRange]>,
    pub(super) relocations: Box<[Relocation]>,
    pub(super) cranelift: Option<CraneliftMachineInfo>,
}

impl UnlinkedImage {
    pub fn plan(&self) -> &PreparedFunction {
        &self.plan
    }
    pub fn tier(&self) -> NativeTier {
        self.tier
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn ranges(&self) -> &[MachineRange] {
        &self.ranges
    }

    pub fn relocations(&self) -> &[Relocation] {
        &self.relocations
    }

    pub fn cranelift_machine_info(&self) -> Option<&CraneliftMachineInfo> {
        self.cranelift.as_ref()
    }
}

#[derive(Debug)]
pub enum EmitError {
    UnsupportedTarget(NativeTarget),
    MissingSite(InstructionId),
    MissingCall(InstructionId),
    SourceIdOutOfRange(InstructionId),
    AbiOffsetOutOfRange,
    Configuration(cranelift::codegen::settings::SetError),
    Codegen(cranelift::codegen::CodegenError),
    UnexpectedRelocation,
}

impl fmt::Display for EmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedTarget(target) => write!(f, "unsupported emitter target {target:?}"),
            Self::MissingSite(pc) => {
                write!(f, "prepared instruction {} has no machine label", pc.0)
            }
            Self::MissingCall(pc) => write!(f, "prepared call {} is missing its link", pc.0),
            Self::SourceIdOutOfRange(pc) => write!(
                f,
                "source ID {} cannot be represented in native metadata",
                pc.0
            ),
            Self::AbiOffsetOutOfRange => f.write_str("native ABI offset exceeds emitter capacity"),
            Self::Configuration(error) => write!(f, "Cranelift configuration failed: {error}"),
            Self::Codegen(error) => write!(f, "Cranelift emission failed: {error}"),
            Self::UnexpectedRelocation => f.write_str("emitter produced an unsupported relocation"),
        }
    }
}

impl std::error::Error for EmitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Configuration(error) => Some(error),
            Self::Codegen(error) => Some(error),
            _ => None,
        }
    }
}

// The assembler source map reserves the high bit and the all-ones default ID.
const MAX_SOURCE_ID: u32 = (u32::MAX >> 1) - 1;

pub(super) fn checked_source(pc: InstructionId) -> Result<u32, EmitError> {
    (pc.0 <= MAX_SOURCE_ID)
        .then_some(pc.0)
        .ok_or(EmitError::SourceIdOutOfRange(pc))
}

impl NativeTier {
    pub fn emit(self, plan: Arc<PreparedFunction>) -> Result<UnlinkedImage, EmitError> {
        match self {
            Self::Winch => super::winch::emit(plan),
            Self::Cranelift(profile) => super::cranelift::emit(plan, profile),
        }
    }
}
