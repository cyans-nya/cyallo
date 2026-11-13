//! Source adapters and executable packaging. Language semantics live in cyallo-ir.
use crate::ir;
use cyallo_ast::*;
pub(crate) use cyallo_ir::analysis::contextualize;
pub use cyallo_ir::{CompileOptions, Dialect};
type Result<T> = std::result::Result<T, Diagnostic>;
pub fn compile(source: &str) -> Result<ir::Program> {
    compile_with_options(source, CompileOptions::default())
}
pub fn compile_with_options(source: &str, options: CompileOptions) -> Result<ir::Program> {
    compile_source(source, options, false)
}
pub(crate) fn compile_source(
    source: &str,
    options: CompileOptions,
    repl: bool,
) -> Result<ir::Program> {
    compile_source_inner(source, options, repl, false)
}
/// Parse declarations and check the entry body; defer routine bodies until used.
pub fn compile_for_execution(source: &str, options: CompileOptions) -> Result<ir::Program> {
    compile_source_inner(source, options, false, true)
}
fn compile_source_inner(
    source: &str,
    options: CompileOptions,
    repl: bool,
    deferred: bool,
) -> Result<ir::Program> {
    let ast = if options.dialect == Dialect::Modern {
        cyallo_parser::parse_modern_with_jobs(
            source,
            repl,
            cyallo_compile_jobs::Jobs(options.jobs),
        )?
    } else if repl {
        caie_int_parser::parse_repl(source)?
    } else {
        caie_int_parser::parse(source)?
    };
    let ast = if options.dialect == Dialect::Modern {
        crate::namespace::single(ast)?
    } else {
        ast
    };
    compile_ast(source, ast, options, deferred)
}
pub(crate) fn compile_ast(
    source: &str,
    mut ast: Ast,
    options: CompileOptions,
    deferred: bool,
) -> Result<ir::Program> {
    cyallo_ir::generics::specialize(&mut ast, options)?;
    if options.dialect == Dialect::Modern {
        crate::constants::evaluate(source, &mut ast, options)?;
    }
    compile_ast_unfolded(source, ast, options, deferred)
}
/// Constant evaluation uses the same checked lowering and interpreter, without
/// recursively starting another constant-evaluation pass.
pub(crate) fn compile_ast_unfolded(
    source: &str,
    ast: Ast,
    options: CompileOptions,
    deferred: bool,
) -> Result<ir::Program> {
    let checked = cyallo_ir::analysis::analyze(source.len(), ast, options, deferred)?;
    let bytecode =
        crate::bytecode::Module::lower(&checked.functions, cyallo_compile_jobs::Jobs(options.jobs));
    Ok(ir::Program {
        records: checked.records,
        requires_tracing: checked.requires_tracing,
        jobs: crate::Jobs(options.jobs),
        inspection: None,
        snapshot_id: 0,
        source_modules: Box::default(),
        services: checked.services,
        source: source.into(),
        functions: checked.functions,
        bytecode,
    })
}
