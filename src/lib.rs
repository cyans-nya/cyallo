//! Typed language, CAIE compatibility, register bytecode and background tiered execution.
//!
//! ```
//! use std::sync::Arc;
//! use cyallo::{compile_with_options, CompileOptions, Dialect, Session, Status};
//! let program = Arc::new(compile_with_options(
//!     "Count <- 0\nArr[10] <- 123\nOUTPUT LENGTH(Arr)\n",
//!     CompileOptions { dialect: Dialect::Simplified, ..Default::default() },
//! )?);
//! let mut session = Session::new(program)?;
//! session.run(1_000);
//! assert_eq!(session.status(), &Status::Finished);
//! assert_eq!(session.context.output, "11\n");
//! # Ok::<(), cyallo::Diagnostic>(())
//! ```
mod admission;
pub mod backend;
use cyallo_ir::builtins;
use cyallo_ir::namespace;
pub mod bytecode;
mod compiler;
pub use cyallo_channel as channel;
pub use cyallo_scheduler as concurrency;
mod constants;
pub mod debugger;
pub mod diagnostics;
use cyallo_ir::effects;
pub use cyallo_ir::gc_metadata;
pub mod highlight;
pub mod inspection;
pub mod ir;
use cyallo_ir::lookup;
pub mod metrics;
pub use cyallo_ir::osr;
pub mod repl;
pub mod runtime;
pub use compiler::{CompileOptions, Dialect, compile, compile_for_execution, compile_with_options};
pub use cyallo_ast::{Diagnostic, SourceFile, Span, Type};
pub use cyallo_compile_jobs::Jobs;
pub use runtime::{Context, Session, Status, Value};
pub mod modules;
pub use cyallo_ir::services;
