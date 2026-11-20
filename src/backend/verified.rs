//! Native preparation and artifacts for verified bytecode.

#![deny(unsafe_op_in_unsafe_fn, clippy::undocumented_unsafe_blocks)]

mod abi;
mod cranelift;
mod image;
mod platform;
mod prepare;
mod publish;
mod run;
mod tiering;
mod validate;
mod winch;

pub use abi::{
    AbiArgument, AbiLayout, Architecture, ByteOffset, Failure, GuestRegister, NativeTarget,
    Platform, ScalarOutcome,
};
pub use image::{
    CraneliftMachineInfo, CraneliftProfile, EmitError, MachineRange, MachineTrap, NativeTier,
    Relocation, RelocationKind, UnlinkedImage,
};
use prepare::ElementAccess;
pub use prepare::{
    ArrayAccess, BridgeCall, Call, CallArgument, CodeVersion, FunctionVersion, Layout, LayoutId,
    LoanBase, NativeHome, Operation, Origin, PrepareError, PreparedFunction, RecoverySlot, Site,
    SliceKind, UnaryOp, WordOp, WordRange,
};
pub use publish::{CodeStore, EntryPin, PublishError};
pub use run::{Dispatch, EntryEvent, ExecutionMode, NativeActivation, NativeRunError};
pub use tiering::{TierPolicy, Tiered};
pub use validate::{ImageError, ValidatedImage};
