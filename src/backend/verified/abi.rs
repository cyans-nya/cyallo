//! The scalar guest entry ABI, shared by preparation and native adapters.

use std::mem::{align_of, offset_of, size_of};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Architecture {
    Aarch64,
    X86_64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Linux,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeTarget {
    pub architecture: Architecture,
    pub platform: Platform,
}

impl NativeTarget {
    pub fn host() -> Option<Self> {
        let architecture = if cfg!(target_arch = "aarch64") {
            Architecture::Aarch64
        } else if cfg!(target_arch = "x86_64") {
            Architecture::X86_64
        } else {
            return None;
        };
        let platform = if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(target_os = "linux") {
            Platform::Linux
        } else {
            return None;
        };
        Some(Self {
            architecture,
            platform,
        })
    }

    pub fn argument(self, argument: AbiArgument) -> GuestRegister {
        use AbiArgument::*;
        use Architecture::*;
        use GuestRegister::*;
        // AAPCS64 and System V AMD64 agree between the admitted operating
        // systems for these three pointer-only parameters.
        match (self.architecture, argument) {
            (Aarch64, Context) => X0,
            (Aarch64, Environment) => X1,
            (Aarch64, ResultPlace) => X2,
            (X86_64, Context) => Rdi,
            (X86_64, Environment) => Rsi,
            (X86_64, ResultPlace) => Rdx,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiArgument {
    Context,
    Environment,
    ResultPlace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestRegister {
    X0,
    X1,
    X2,
    Rdi,
    Rsi,
    Rdx,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteOffset(pub u32);

// The admitted targets use the host's 64-bit C pointer representation and
// canonical scalar slot words. Pointers stay pointers at the Rust boundary.
#[repr(C)]
pub(super) struct ScalarContext {
    pub slots: *mut u64,
    pub budget: u64,
    pub next: u64,
    pub failure: u64,
    pub links: *const u64,
    pub callee: u64,
    pub predecessor: u64,
    pub edge: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub(super) enum Edge {
    Entry,
    Next,
    Jump,
    Yes,
    No,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum ScalarOutcome {
    Returned = 0,
    Paused = 1,
    Failed = 2,
    Call = 3,
    /// The runtime performs the operation, and the frame continues after it.
    Bridge = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum Failure {
    Overflow = 1,
    DivisionByZero = 2,
    Trap = 3,
    ConversionOutOfRange = 4,
    EnumVariant = 5,
    ZeroStep = 6,
    IndexOutOfBounds = 7,
    UninitializedElement = 8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbiLayout {
    pub context_bytes: usize,
    pub context_alignment: usize,
    pub slots: usize,
    pub budget: usize,
    pub next: usize,
    pub failure: usize,
    pub links: usize,
    pub callee: usize,
    pub predecessor: usize,
    pub edge: usize,
    pub slot_bytes: usize,
    pub slot_alignment: usize,
}

impl AbiLayout {
    pub const SCALARS: Self = Self {
        context_bytes: size_of::<ScalarContext>(),
        context_alignment: align_of::<ScalarContext>(),
        slots: offset_of!(ScalarContext, slots),
        budget: offset_of!(ScalarContext, budget),
        next: offset_of!(ScalarContext, next),
        failure: offset_of!(ScalarContext, failure),
        links: offset_of!(ScalarContext, links),
        callee: offset_of!(ScalarContext, callee),
        predecessor: offset_of!(ScalarContext, predecessor),
        edge: offset_of!(ScalarContext, edge),
        slot_bytes: size_of::<u64>(),
        slot_alignment: align_of::<u64>(),
    };
}

pub(super) fn scalar_word(value: crate::bytecode::verified::Scalar) -> u64 {
    use crate::bytecode::verified::Scalar;
    match value {
        Scalar::I64(value) => u64::from_ne_bytes(value.to_ne_bytes()),
        Scalar::F64(bits) => bits,
        Scalar::U64(value) => value,
        Scalar::Bool(value) => u64::from(value),
    }
}
