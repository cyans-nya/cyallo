//! Backend-independent native compilation contracts.
mod abi;
mod error;
pub use error::CompileError;
mod tiered;
pub use crate::admission::{EagerDecision, EagerPolicy};
pub mod verified;
mod winch;
#[cfg(feature = "wasm-adapter")]
mod winch_wasm;
pub use tiered::{
    CompilerPool, Tier, TierEvent, TierEventKind, TierInfo, TierPolicy, TieredArtifact,
    TieredBackend,
};
#[cfg(feature = "wasm-adapter")]
pub use winch_wasm::WasmBackend;
mod cranelift_backend;
mod inlining;
use crate::{
    ir::Program,
    runtime::{EngineError, Session},
};
pub use cranelift_backend::{
    CompilationScope, CraneliftArtifact, CraneliftBackend, CraneliftConfig, CraneliftSpeedBackend,
    EagerCraneliftArtifact, EagerSpeedArtifact, OptimizationLevel, ResumeStrategy,
};
use std::sync::Arc;
pub trait Executable {
    fn run(&self, session: &mut Session, budget: usize) -> Result<(), EngineError>;
}
pub trait NativeBackend {
    type Artifact: Executable;
    fn compile(&self, program: Arc<Program>) -> Result<Self::Artifact, CompileError>;
}
