//! Typed native compilation failures.
use cyallo_ast::Diagnostic;
use std::{fmt, sync::Arc};

#[derive(Debug, Clone)]
pub enum CompileError {
    /// The routine (or program) failed semantic checks.
    Check(Diagnostic),
    /// `opt(always_lazy)` routines never compile natively.
    AlwaysLazy,
    UnsupportedTarget,
    /// The private native ABI requires 64-bit pointers.
    PointerWidth,
    UnknownFunction(usize),
    InvalidTierPolicy,
    Module(Arc<cranelift_module::ModuleError>),
    #[cfg(feature = "wasm-adapter")]
    Wasm(Arc<wasmtime::Error>),
}
impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Check(diagnostic) => diagnostic.fmt(f),
            Self::AlwaysLazy => f.write_str("opt(always_lazy) forbids native compilation"),
            Self::UnsupportedTarget => {
                f.write_str("native code generation does not support this target")
            }
            Self::PointerWidth => f.write_str("native ABI requires 64-bit pointers"),
            Self::UnknownFunction(function) => write!(f, "unknown routine {function}"),
            Self::InvalidTierPolicy => f.write_str("invalid tier policy"),
            Self::Module(error) => write!(f, "code generation failed: {error}"),
            #[cfg(feature = "wasm-adapter")]
            Self::Wasm(error) => write!(f, "Wasm adapter failed: {error}"),
        }
    }
}
impl std::error::Error for CompileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Check(diagnostic) => Some(diagnostic),
            Self::Module(error) => Some(error.as_ref()),
            #[cfg(feature = "wasm-adapter")]
            Self::Wasm(error) => Some(error.as_ref().as_ref()),
            _ => None,
        }
    }
}
impl From<Diagnostic> for CompileError {
    fn from(diagnostic: Diagnostic) -> Self {
        Self::Check(diagnostic)
    }
}
impl From<&Diagnostic> for CompileError {
    fn from(diagnostic: &Diagnostic) -> Self {
        Self::Check(diagnostic.clone())
    }
}
impl From<cranelift_module::ModuleError> for CompileError {
    fn from(error: cranelift_module::ModuleError) -> Self {
        Self::Module(Arc::new(error))
    }
}
#[cfg(feature = "wasm-adapter")]
impl From<wasmtime::Error> for CompileError {
    fn from(error: wasmtime::Error) -> Self {
        Self::Wasm(Arc::new(error))
    }
}
