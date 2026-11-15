//! Checked language IR and semantic analysis, independent of execution engines.
pub use cyallo_ast::{Diagnostic, Span, Type};
pub mod builtins;
pub mod concurrent;
pub mod core_package;
pub mod declarations;
pub mod effects;
pub mod gc_metadata;
pub mod generator;
pub mod import_policy;
pub mod internal;
pub mod intrinsic;
pub mod ir;
pub mod lookup;
pub mod member;
pub mod namespace;
pub mod native_contract;
pub mod native_thunk;
pub mod osr;
pub mod services;
pub mod standard;

pub use cyallo_compile_jobs::Jobs;
pub mod options;
pub use options::{CompileOptions, Dialect};
pub mod admission;
pub mod analysis;
pub mod composites;
pub mod generics;
pub mod inference;
pub mod ownership;

pub mod dynamic;
pub mod memory;
