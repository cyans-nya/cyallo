//! Backend-independent native compilation contracts.
mod abi;
mod tiered;
pub use crate::admission::{EagerDecision, EagerPolicy};
pub use tiered::compiler_worker_count;
mod winch;
#[cfg(feature = "wasm-adapter")]
mod winch_wasm;
pub use tiered::{
    Tier, TierEvent, TierEventKind, TierInfo, TierPolicy, TieredArtifact, TieredBackend,
};
#[cfg(feature = "wasm-adapter")]
pub use winch_wasm::WasmBackend;
mod cranelift_backend;
mod inlining;
use crate::{ir::Program, runtime::Session};
pub use cranelift_backend::{
    CompilationScope, CraneliftArtifact, CraneliftBackend, CraneliftConfig, CraneliftSpeedBackend,
    EagerCraneliftArtifact, EagerSpeedArtifact, OptimizationLevel, ResumeStrategy,
};
use std::sync::Arc;
fn next_code_version() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}
pub trait Executable {
    fn run(&self, session: &mut Session, budget: usize) -> Result<(), String>;
}
pub trait NativeBackend {
    type Artifact: Executable;
    fn compile(&self, program: Arc<Program>) -> Result<Self::Artifact, String>;
}
