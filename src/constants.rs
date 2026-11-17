//! Bounded compile-time evaluation using the ordinary checked interpreter.
//! Scalars become immediates; aggregate payloads share an immutable pool and
//! materialize owned value snapshots, never Session addresses.
use crate::{CompileOptions, Context, Diagnostic, Session, Status, Type, Value, ir::Op};
use cyallo_ast::*;
use std::{collections::HashMap, sync::Arc};

const FUEL: usize = 100_000;
const CELLS: usize = 16_384;
const FRAMES: usize = 64;
const BYTES: usize = 1_048_576;
#[derive(Clone)]
enum Template {
    Single(Box<Ast>),
    Modules(Box<cyallo_ir::module_graph::ResolvedProgram>),
}
type Environment = HashMap<String, Option<Expr>>;
type Result<T> = std::result::Result<T, Diagnostic>;
fn key(name: &str) -> String {
    name.to_owned()
}
#[derive(Debug)]
enum ConstantError {
    RuntimeBinding { name: Box<str>, called: bool },
    CapturingClosure,
    ClosureDeclaration,
    TypeMismatch { expected: Type, actual: Type },
    AllocationLimit,
    ImpureInitializer,
    WorkLimit,
    NonContiguousArray,
    MutableValue,
    EnumLayout,
    Check(Diagnostic),
    Runtime(crate::runtime::RuntimeError),
}
impl std::fmt::Display for ConstantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RuntimeBinding { name, called: true } => write!(f, "const cannot call runtime binding {name}"),
            Self::RuntimeBinding { name, called: false } => write!(f, "const cannot depend on runtime binding {name}"),
            Self::CapturingClosure => f.write_str("const closures are not supported yet; use a named stateless routine"),
            Self::ClosureDeclaration => f.write_str("const declarations in closures are not supported yet; use a named routine"),
            Self::TypeMismatch { expected, actual } => write!(f, "const expected {expected:?}, got {actual:?}"),
            Self::AllocationLimit => f.write_str("const evaluation exceeded its allocation limit"),
            Self::ImpureInitializer => f.write_str("const initializer must use compile-time values and pure fn calls"),
            Self::WorkLimit => f.write_str("const evaluation exceeded its work limit"),
            Self::NonContiguousArray => f.write_str("const arrays must have initialized contiguous elements"),
            Self::EnumLayout => f.write_str("constant enum does not match its checked variant contract"),
            Self::MutableValue => f.write_str("const value must be deeply immutable; managed identities and captured callables cannot be frozen"),
            Self::Check(error) => write!(f, "const evaluation: {}", error.message),
            Self::Runtime(error) => write!(f, "const evaluation: {error}"),
        }
    }
}
impl std::error::Error for ConstantError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Check(error) => Some(error),
            Self::Runtime(error) => Some(error),
            _ => None,
        }
    }
}
fn invalid_constant(error: ConstantError, span: Span) -> Diagnostic {
    Diagnostic::new("E104", error.to_string(), span)
}
pub(crate) fn evaluate(source: &str, ast: &mut Ast, options: CompileOptions) -> Result<()> {
    let template = Template::Single(Box::new(ast.clone()));
    let mut globals = Environment::new();
    shadow(&ast.body, &mut globals);
    block(
        source,
        &template,
        &mut ast.body,
        &mut globals,
        options,
        None,
    )?;
    for function in &mut ast.functions {
        let mut env = globals.clone();
        for param in &function.params {
            env.insert(key(&param.name), None);
        }
        shadow(&function.body, &mut env);
        block(
            source,
            &template,
            &mut function.body,
            &mut env,
            options,
            Some(&globals),
        )?;
    }
    for implementation in &mut ast.implementations {
        for function in &mut implementation.methods {
            let mut env = globals.clone();
            for param in &function.params {
                env.insert(key(&param.name), None);
            }
            shadow(&function.body, &mut env);
            block(
                source,
                &template,
                &mut function.body,
                &mut env,
                options,
                Some(&globals),
            )?;
        }
    }
    for property in &mut ast.properties {
        let mut env = globals.clone();
        env.insert("SELF".into(), None);
        for param in &property.getter.params {
            env.insert(key(&param.name), None);
        }
        shadow(&property.getter.body, &mut env);
        block(
            source,
            &template,
            &mut property.getter.body,
            &mut env,
            options,
            Some(&globals),
        )?;
    }
    Ok(())
}
fn shadow(body: &[Stmt], env: &mut Environment) {
    for stmt in body {
        match &stmt.kind {
            StmtKind::Binding(n, ..) | StmtKind::Constant(n, _) | StmtKind::Declare(n, _) => {
                env.insert(key(n), None);
            }
            StmtKind::If(_, a, b) => {
                shadow(a, env);
                shadow(b, env);
            }
            StmtKind::While(_, b) | StmtKind::Repeat(b, _) => shadow(b, env),
            StmtKind::For(name, _, _, _, b) | StmtKind::ForIn(name, _, b) => {
                env.entry(key(name)).or_insert(None);
                shadow(b, env);
            }
            StmtKind::Case(_, arms, b) => {
                for (_, arm) in arms {
                    shadow(arm, env);
                }
                shadow(b, env);
            }
            _ => {}
        }
    }
}
struct Evaluation<'a> {
    source: &'a str,
    template: &'a Template,
    options: CompileOptions,
    globals: Option<&'a Environment>,
}
fn block(
    source: &str,
    template: &Template,
    body: &mut [Stmt],
    env: &mut Environment,
    options: CompileOptions,
    globals: Option<&Environment>,
) -> Result<()> {
    let evaluation = &Evaluation {
        source,
        template,
        options,
        globals,
    };
    cyallo_ast::with_stack(|| {
        for stmt in body {
            match &mut stmt.kind {
                StmtKind::Binding(name, ty, true, _, expr) => {
                    rewrite(expr, env, true, evaluation)?;
                    let (value, inferred) = evaluate_one(
                        source,
                        template,
                        globals.unwrap_or(env),
                        expr,
                        ty.clone(),
                        options,
                    )?;
                    *expr = value.clone();
                    *ty = Some(inferred);
                    env.insert(key(name), Some(value));
                }
                StmtKind::Binding(_, _, false, _, expr)
                | StmtKind::Evaluate(expr)
                | StmtKind::Return(expr) => rewrite(expr, env, false, evaluation)?,
                StmtKind::Constant(name, expr) => {
                    rewrite(expr, env, true, evaluation)?;
                    let (value, _) = evaluate_one(
                        source,
                        template,
                        globals.unwrap_or(env),
                        expr,
                        None,
                        options,
                    )?;
                    *expr = value.clone();
                    env.insert(key(name), Some(value));
                }
                StmtKind::Assign(target, value) | StmtKind::Compound(target, _, value) => {
                    rewrite_target(target, env, evaluation)?;
                    rewrite(value, env, false, evaluation)?;
                }
                StmtKind::Output(values) | StmtKind::Call(_, values) => {
                    for value in values {
                        rewrite(value, env, false, evaluation)?;
                    }
                }
                StmtKind::If(condition, yes, no) => {
                    rewrite(condition, env, false, evaluation)?;
                    block(source, template, yes, env, options, globals)?;
                    block(source, template, no, env, options, globals)?;
                }
                StmtKind::While(condition, body) | StmtKind::Repeat(body, condition) => {
                    rewrite(condition, env, false, evaluation)?;
                    block(source, template, body, env, options, globals)?;
                }
                StmtKind::For(_, start, end, step, body) => {
                    rewrite(start, env, false, evaluation)?;
                    rewrite(end, env, false, evaluation)?;
                    if let Some(step) = step {
                        rewrite(step, env, false, evaluation)?;
                    }
                    block(source, template, body, env, options, globals)?;
                }
                StmtKind::ForIn(_, iterable, body) => {
                    rewrite(iterable, env, false, evaluation)?;
                    block(source, template, body, env, options, globals)?;
                }
                StmtKind::Case(value, arms, other) => {
                    rewrite(value, env, false, evaluation)?;
                    for (label, arm) in arms {
                        rewrite(&mut label.lower, env, false, evaluation)?;
                        if let Some(upper) = &mut label.upper {
                            rewrite(upper, env, false, evaluation)?;
                        }
                        block(source, template, arm, env, options, globals)?;
                    }
                    block(source, template, other, env, options, globals)?;
                }
                _ => {}
            }
        }
        Ok(())
    })
}
fn rewrite_target(expr: &mut Expr, env: &Environment, evaluation: &Evaluation<'_>) -> Result<()> {
    match &mut expr.kind {
        ExprKind::Index(base, indices) => {
            rewrite_target(base, env, evaluation)?;
            for index in indices {
                rewrite(index, env, false, evaluation)?;
            }
        }
        ExprKind::Field(base, _)
        | ExprKind::TupleField(base, _)
        | ExprKind::Parenthesized(base)
        | ExprKind::ExtensionField(base, _, _) => rewrite_target(base, env, evaluation)?,
        _ => {}
    }
    Ok(())
}
fn rewrite(
    expr: &mut Expr,
    env: &Environment,
    require_constant: bool,
    evaluation: &Evaluation<'_>,
) -> Result<()> {
    cyallo_ast::with_stack(|| {
        match &mut expr.kind {
            ExprKind::EnumConstruct(_, _, payload) => {
                if let Some(payload) = payload {
                    rewrite(payload, env, require_constant, evaluation)?;
                }
            }
            ExprKind::AnonymousRecord(fields) | ExprKind::VariantPayload(fields) => {
                for (_, value) in fields {
                    rewrite(value, env, require_constant, evaluation)?;
                }
            }
            ExprKind::Match(value, arms) => {
                rewrite(value, env, require_constant, evaluation)?;
                for arm in arms {
                    if let Some(guard) = &mut arm.guard {
                        rewrite(guard, env, require_constant, evaluation)?;
                    }
                    match &mut arm.body {
                        MatchBody::Expression(value) => {
                            rewrite(value, env, require_constant, evaluation)?
                        }
                        MatchBody::Block(body, tail) => {
                            let mut scope = env.clone();
                            shadow(body, &mut scope);
                            block(
                                evaluation.source,
                                evaluation.template,
                                body,
                                &mut scope,
                                evaluation.options,
                                evaluation.globals,
                            )?;
                            if let Some(tail) = tail {
                                rewrite(tail, &scope, false, evaluation)?;
                            }
                        }
                    }
                }
            }
            ExprKind::Yield(value, _) => rewrite(value, env, require_constant, evaluation)?,
            ExprKind::ManagedNew(_, _, allocator, value) => {
                if let Some(allocator) = allocator {
                    rewrite(allocator, env, require_constant, evaluation)?;
                }
                rewrite(value, env, require_constant, evaluation)?;
            }

            ExprKind::IntrinsicCall(_, _, arguments) => {
                for argument in arguments {
                    rewrite(argument, env, require_constant, evaluation)?;
                }
            }

            ExprKind::Name(name) => match env.get(&key(name)) {
                Some(Some(value)) => {
                    let span = expr.span;
                    *expr = value.clone();
                    // A service key is a declaration identity, not a fresh key at
                    // each use. Its source identity survives constant substitution.
                    if !matches!(&expr.kind, ExprKind::IntrinsicCall(id, _, _) if cyallo_ir::intrinsic::Intrinsic::from_id(*id) == Some(cyallo_ir::intrinsic::Intrinsic::ServiceKeyNew))
                    {
                        expr.span = span;
                    }
                }
                Some(None) if require_constant => {
                    return Err(invalid_constant(
                        ConstantError::RuntimeBinding {
                            name: name.clone(),
                            called: false,
                        },
                        expr.span,
                    ));
                }
                _ => {}
            },
            ExprKind::Call(name, args) => {
                for arg in args.iter_mut() {
                    rewrite(arg, env, require_constant, evaluation)?;
                }
                match env.get(&key(name)) {
                    Some(Some(value)) => {
                        expr.kind = ExprKind::Invoke(Box::new(value.clone()), std::mem::take(args))
                    }
                    Some(None) if require_constant => {
                        return Err(invalid_constant(
                            ConstantError::RuntimeBinding {
                                name: name.clone(),
                                called: true,
                            },
                            expr.span,
                        ));
                    }
                    _ => {}
                }
            }
            ExprKind::DeleteProperty(e)
            | ExprKind::Unary(_, e)
            | ExprKind::Parenthesized(e)
            | ExprKind::Cast(e, _)
            | ExprKind::Field(e, _)
            | ExprKind::TupleField(e, _)
            | ExprKind::ExtensionField(e, _, _) => rewrite(e, env, require_constant, evaluation)?,
            ExprKind::Binary(_, a, b) | ExprKind::NativeCoerce(a, b) => {
                rewrite(a, env, require_constant, evaluation)?;
                rewrite(b, env, require_constant, evaluation)?;
            }
            ExprKind::Invoke(callee, args) | ExprKind::Index(callee, args) => {
                rewrite(callee, env, require_constant, evaluation)?;
                for arg in args {
                    rewrite(arg, env, require_constant, evaluation)?;
                }
            }
            ExprKind::ArrayLiteral(args)
            | ExprKind::Tuple(args)
            | ExprKind::TypedCollection(_, _, args)
            | ExprKind::GenericCall(_, _, args) => {
                for arg in args {
                    rewrite(arg, env, require_constant, evaluation)?;
                }
            }
            ExprKind::Record(_, fields)
            | ExprKind::GenericRecord(_, _, fields)
            | ExprKind::ObjectLiteral(_, fields) => {
                for (_, value) in fields {
                    rewrite(value, env, require_constant, evaluation)?;
                }
            }
            ExprKind::Closure(function, _) => {
                if require_constant {
                    return Err(invalid_constant(ConstantError::CapturingClosure, expr.span));
                }
                // A closure's parameters and locals shadow outer constants.
                let mut captures = env.clone();
                for param in &function.params {
                    captures.insert(key(&param.name), None);
                }
                shadow(&function.body, &mut captures);
                // Constant declarations in closures are checked separately below by
                // the same evaluator, including their runtime capture dependencies.
                rewrite_closure_body(&mut function.body, &captures, evaluation)?;
            }
            ExprKind::ClosureValue(_, captures, _) => {
                for value in captures {
                    rewrite(value, env, require_constant, evaluation)?;
                }
            }
            ExprKind::TypeDescriptor(_)
            | ExprKind::DeferredCallable(_)
            | ExprKind::Literal(_)
            | ExprKind::Frozen(..)
            | ExprKind::Invalid(_) => {}
        }
        Ok(())
    })
}
fn rewrite_closure_body(
    body: &mut [Stmt],
    env: &Environment,
    evaluation: &Evaluation<'_>,
) -> Result<()> {
    cyallo_ast::with_stack(|| {
        // Closure bodies must not evade compile-time validation. The main pass can
        // evaluate their constants after closure conversion in a later extension.
        for stmt in body {
            match &mut stmt.kind {
                StmtKind::Binding(_, _, true, _, _) | StmtKind::Constant(..) => {
                    return Err(invalid_constant(
                        ConstantError::ClosureDeclaration,
                        stmt.span,
                    ));
                }
                StmtKind::Return(e)
                | StmtKind::Evaluate(e)
                | StmtKind::Binding(_, _, false, _, e)
                | StmtKind::Assign(_, e)
                | StmtKind::Compound(_, _, e) => rewrite(e, env, false, evaluation)?,
                StmtKind::If(e, a, b) => {
                    rewrite(e, env, false, evaluation)?;
                    rewrite_closure_body(a, env, evaluation)?;
                    rewrite_closure_body(b, env, evaluation)?;
                }
                StmtKind::While(e, b) | StmtKind::Repeat(b, e) => {
                    rewrite(e, env, false, evaluation)?;
                    rewrite_closure_body(b, env, evaluation)?;
                }
                StmtKind::For(_, a, b, step, body) => {
                    rewrite(a, env, false, evaluation)?;
                    rewrite(b, env, false, evaluation)?;
                    if let Some(e) = step {
                        rewrite(e, env, false, evaluation)?;
                    }
                    rewrite_closure_body(body, env, evaluation)?;
                }
                StmtKind::ForIn(_, iterable, body) => {
                    rewrite(iterable, env, false, evaluation)?;
                    rewrite_closure_body(body, env, evaluation)?;
                }
                StmtKind::Case(e, arms, b) => {
                    rewrite(e, env, false, evaluation)?;
                    for (_, a) in arms {
                        rewrite_closure_body(a, env, evaluation)?;
                    }
                    rewrite_closure_body(b, env, evaluation)?;
                }
                StmtKind::Output(es) | StmtKind::Call(_, es) => {
                    for e in es {
                        rewrite(e, env, false, evaluation)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    })
}
fn evaluate_one(
    source: &str,
    template: &Template,
    globals: &Environment,
    expr: &Expr,
    ty: Option<Type>,
    options: CompileOptions,
) -> Result<(Expr, Type)> {
    let contextual = if let Some(wanted) = &ty {
        crate::compiler::contextualize(expr, wanted)?
    } else {
        expr.clone()
    };
    let expr = &contextual;
    // Already frozen values and literals need no evaluator Session or replay of
    // previously evaluated constants. Aliasing an immutable pool entry is cheap.
    let known = match &expr.kind {
        ExprKind::Frozen(_, ty) => Some(ty.clone()),
        ExprKind::Literal(literal) => Some(match literal {
            Literal::Number(n) => Type::numeric(n.kind),
            Literal::Integer(_) => Type::Integer,
            Literal::Real(_) => Type::Real,
            Literal::Boolean(_) => Type::Boolean,
            Literal::Char(_) => Type::Char,
            Literal::String(_) => Type::String,
        }),
        _ => None,
    };
    if let Some(inferred) = known {
        if let Some(wanted) = &ty
            && *wanted != inferred
        {
            if *wanted == Type::Real
                && let ExprKind::Literal(Literal::Integer(value)) = expr.kind
            {
                return Ok((
                    Expr {
                        kind: ExprKind::Literal(Literal::Real(value as f64)),
                        span: expr.span,
                    },
                    Type::Real,
                ));
            }
            return Err(invalid_constant(
                ConstantError::TypeMismatch {
                    expected: wanted.clone(),
                    actual: inferred.clone(),
                },
                expr.span,
            ));
        }
        let mut value = expr.clone();
        if let ExprKind::Literal(Literal::String(text)) = &expr.kind {
            if text.len() > BYTES {
                return Err(invalid_constant(ConstantError::AllocationLimit, expr.span));
            }
            value.kind = ExprKind::Frozen(
                Arc::new(ConstantValue::String(Arc::from(text.as_ref()))),
                Type::String,
            );
        }
        return Ok((value, inferred));
    }
    let mut known: Vec<_> = globals
        .iter()
        .filter_map(|(n, v)| v.as_ref().map(|v| (n, v)))
        .collect();
    known.sort_by_key(|(n, _)| *n);
    let result = Stmt {
        kind: StmtKind::Binding("$const_result".into(), ty, true, false, expr.clone()),
        span: expr.span,
    };
    let compiled = match template {
        Template::Single(template) => {
            let mut ast = template.as_ref().clone();
            ast.body = known
                .into_iter()
                .map(|(n, v)| Stmt {
                    kind: StmtKind::Binding(n.as_str().into(), None, true, false, v.clone()),
                    span: v.span,
                })
                .collect();
            ast.body.push(result);
            crate::compiler::compile_ast_unfolded(source, ast, options, true)
        }
        Template::Modules(template) => {
            let mut graph = template.as_ref().clone();
            for module in &mut graph.modules {
                module.source.body = module
                    .source
                    .body
                    .iter()
                    .filter_map(|s| {
                        let name = match &s.kind {
                            StmtKind::Binding(n, ..) | StmtKind::Constant(n, _) => n,
                            _ => return None,
                        };
                        globals
                            .get(name.as_ref())
                            .and_then(Option::as_ref)
                            .map(|v| Stmt {
                                kind: StmtKind::Binding(name.clone(), None, true, false, v.clone()),
                                span: s.span,
                            })
                    })
                    .collect();
            }
            graph.entry_mut().source.body.push(result);
            crate::compiler::compile_resolved_unfolded(source, graph, options, true)
        }
    };
    let program =
        Arc::new(compiled.map_err(|d| invalid_constant(ConstantError::Check(d.clone()), d.span))?);
    // Entry code may initialize its own constants, but must not execute an
    // effectful operation. Called fn bodies are checked before interpreter entry.
    let initialization =
        std::iter::once(0).chain(program.module_units.iter().filter_map(|m| m.initializer));
    for function in initialization {
        for instruction in &program.functions()[function].code {
            let valid = match &instruction.op {
                Op::Builtin(_, name, _) => crate::builtins::pure(*name),
                Op::Call(_, to, _) => {
                    program.functions()[*to].kind == RoutineKind::Function
                        || program
                            .module_units
                            .iter()
                            .any(|m| m.initializer == Some(*to))
                }
                Op::Invoke(_, callee, _) => {
                    cyallo_ast::callable(&program.functions()[function].registers[*callee])
                        .is_some_and(|(kind, _, _)| kind == RoutineKind::Function)
                }
                Op::Lookup(_, op, _) => {
                    !op.writes()
                        && !matches!(
                            op,
                            crate::lookup::Operation::CellRead
                                | crate::lookup::Operation::LocationLoad
                                | crate::lookup::Operation::Mapped
                        )
                }
                Op::Dynamic(..)
                | Op::Service(..)
                | Op::TraitCall(..)
                | Op::Input(..)
                | Op::Output(..) => false,
                _ => true,
            };
            if !valid {
                return Err(invalid_constant(
                    ConstantError::ImpureInitializer,
                    instruction.span,
                ));
            }
        }
    }
    let result_index = program.functions()[0]
        .locals
        .iter()
        .position(|l| l.name.as_ref() == "$const_result")
        .unwrap();
    let result_type = program.functions()[0].locals[result_index].ty.clone();
    validate_calls(&program, 0, &mut vec![false; program.functions().len()])?;
    let mut session = Session::with_context(
        program.clone(),
        Context {
            max_cells: CELLS,
            max_frames: FRAMES,
            ..Default::default()
        },
    )
    .map_err(|d| invalid_constant(ConstantError::Check(d), expr.span))?;
    session.limit_constant_memory(BYTES);
    session.run(FUEL);
    match session.status() {
        Status::Finished => {}
        Status::Failed(failure) => {
            return Err(invalid_constant(
                ConstantError::Runtime(failure.error.clone()),
                failure.span,
            )
            .with_related(expr.span, "constant initializer"));
        }
        _ => return Err(invalid_constant(ConstantError::WorkLimit, expr.span)),
    }
    let value = session
        .constant_value(result_index)
        .map_err(|e| invalid_constant(ConstantError::Runtime(e), expr.span))?;
    Ok((
        freeze(value, &result_type, &program, expr.span)?,
        result_type,
    ))
}
fn validate_calls(program: &crate::ir::Program, function: usize, seen: &mut [bool]) -> Result<()> {
    if std::mem::replace(&mut seen[function], true) {
        return Ok(());
    }
    let body = program.functions()[function]
        .checked()
        .map_err(|d| invalid_constant(ConstantError::Check(d.clone()), d.span))?;
    for instruction in &body.code {
        match instruction.op {
            Op::Call(_, to, _) => validate_calls(program, to, seen)?,
            Op::FunctionValue(_, to, _)
                if program.functions()[to].kind == RoutineKind::Function =>
            {
                validate_calls(program, to, seen)?
            }
            _ => {}
        }
    }
    Ok(())
}
fn freeze(value: Value, ty: &Type, program: &crate::ir::Program, span: Span) -> Result<Expr> {
    if matches!(
        value,
        Value::ArraySnapshot(_) | Value::SparseSnapshot(..) | Value::String(_)
    ) && let Some(value) = pool_value(&value)
    {
        return Ok(Expr {
            kind: ExprKind::Frozen(Arc::new(value), ty.clone()),
            span,
        });
    }
    let kind = match (value, ty) {
        // Canonical literal forms: i64 and f64 values never use `Literal::Number`.
        (Value::Number(n), _) => ExprKind::Literal(Literal::number(n)),
        (Value::Integer(v), _) => ExprKind::Literal(Literal::Integer(v)),
        (Value::Real(v), _) => ExprKind::Literal(Literal::Real(v)),
        (Value::Boolean(v), _) => ExprKind::Literal(Literal::Boolean(v)),
        (Value::Char(v), _) => ExprKind::Literal(Literal::Char(v)),
        (Value::String(v), _) => ExprKind::Literal(Literal::String(v.as_ref().into())),
        (Value::ServiceKey(id), Type::ServiceKey(inner)) => ExprKind::IntrinsicCall(
            cyallo_ir::intrinsic::Intrinsic::ServiceKeyNew.id(),
            vec![*inner.clone()],
            vec![Expr {
                kind: ExprKind::Literal(Literal::String(program.services().keys[id].label.clone())),
                span,
            }],
        ),
        (value, Type::Storage(storage, inner))
            if crate::runtime::enums::native_enum_parts(&value).is_some() =>
        {
            let (encoding, payload) = crate::runtime::enums::native_enum_parts(&value).unwrap();
            if cyallo_ir::enums::encoding_storage(encoding) != Some(*storage) {
                return Err(invalid_constant(ConstantError::EnumLayout, span));
            }
            let template = program
                .records
                .values()
                .filter_map(|ty| {
                    if let Type::Enum(contract) = ty {
                        Some(contract)
                    } else {
                        None
                    }
                })
                .find(|contract| contract.variants.iter().any(|v| v.encoding == encoding))
                .ok_or_else(|| invalid_constant(ConstantError::EnumLayout, span))?;
            let arguments = cyallo_ir::enums::native_arguments(*storage, inner)
                .ok_or_else(|| invalid_constant(ConstantError::EnumLayout, span))?;
            let contract = cyallo_ir::enums::instantiate_native_template(template, arguments)
                .ok_or_else(|| invalid_constant(ConstantError::EnumLayout, span))?;
            let variant = contract
                .variants
                .iter()
                .find(|v| v.encoding == encoding)
                .unwrap();
            let payload = match (payload, &variant.payload) {
                (Some(value), Some(ty)) => {
                    Some(Box::new(freeze(value.clone(), ty, program, span)?))
                }
                (None, None) => None,
                _ => return Err(invalid_constant(ConstantError::EnumLayout, span)),
            };
            ExprKind::EnumConstruct(variant.declaration, ty.clone(), payload)
        }
        (Value::Enum(variant, payload), Type::Enum(contract)) => {
            let declared = contract
                .variants
                .iter()
                .find(|v| v.declaration == variant)
                .ok_or_else(|| invalid_constant(ConstantError::EnumLayout, span))?;
            let payload = match (payload, &declared.payload) {
                (Some(v), Some(t)) => Some(Box::new(freeze(*v, t, program, span)?)),
                (None, None) => None,
                _ => return Err(invalid_constant(ConstantError::EnumLayout, span)),
            };
            ExprKind::EnumConstruct(variant, ty.clone(), payload)
        }
        (Value::ArraySnapshot(values), Type::Tuple(types)) => ExprKind::Tuple(
            values
                .into_iter()
                .zip(types)
                .map(|(v, t)| freeze(v, t, program, span))
                .collect::<Result<_>>()?,
        ),
        (Value::ArraySnapshot(values), Type::AnonymousRecord(fields)) => ExprKind::AnonymousRecord(
            values
                .into_iter()
                .zip(fields)
                .map(|(v, (name, t))| Ok((name.clone(), freeze(v, t, program, span)?)))
                .collect::<Result<_>>()?,
        ),
        (Value::ArraySnapshot(values), Type::Record(name, fields)) => ExprKind::Record(
            name.clone(),
            values
                .into_iter()
                .zip(fields)
                .map(|(v, (name, t))| Ok((name.clone(), freeze(v, t, program, span)?)))
                .collect::<Result<_>>()?,
        ),
        (Value::SparseSnapshot(mut values, length), Type::SparseArray(element))
            if length > 0 && length <= CELLS as u64 =>
        {
            let mut elements = Vec::with_capacity(length as usize);
            for i in 0..length as u32 {
                let value = values
                    .remove(&i)
                    .ok_or_else(|| invalid_constant(ConstantError::NonContiguousArray, span))?;
                elements.push(freeze(value, element, program, span)?);
            }
            ExprKind::ArrayLiteral(elements)
        }
        (Value::Function(id, captures), _)
            if captures.is_empty() && program.functions()[id].origin != RoutineOrigin::Closure =>
        {
            ExprKind::ClosureValue(program.functions()[id].symbol.clone(), vec![], ty.clone())
        }
        _ => {
            return Err(invalid_constant(ConstantError::MutableValue, span));
        }
    };
    Ok(Expr { kind, span })
}
fn pool_value(value: &Value) -> Option<ConstantValue> {
    Some(match value {
        Value::Number(n) => ConstantValue::Number(*n),
        Value::Integer(v) => ConstantValue::Integer(*v),
        Value::Real(v) => ConstantValue::Real(*v),
        Value::Boolean(v) => ConstantValue::Boolean(*v),
        Value::Char(v) => ConstantValue::Char(*v),
        Value::String(v) => ConstantValue::String(v.clone()),
        Value::ArraySnapshot(values) => {
            ConstantValue::Array(values.iter().map(pool_value).collect::<Option<_>>()?)
        }
        Value::SparseSnapshot(values, length) => ConstantValue::Sparse(
            values
                .iter()
                .map(|(i, v)| Some((*i, pool_value(v)?)))
                .collect::<Option<_>>()?,
            *length,
        ),
        _ => return None,
    })
}

pub(crate) fn evaluate_modules(
    source: &str,
    graph: &mut cyallo_ir::module_graph::ResolvedProgram,
    options: CompileOptions,
) -> Result<()> {
    let template = Template::Modules(Box::new(graph.clone()));
    let mut globals = Environment::new();
    for module in &graph.modules {
        shadow(&module.source.body, &mut globals);
    }
    for (module, statement) in graph
        .constant_order()
        .map_err(cyallo_ir::module_graph::module_diagnostic)?
    {
        block(
            source,
            &template,
            std::slice::from_mut(&mut graph.modules[module].source.body[statement]),
            &mut globals,
            options,
            None,
        )?;
    }
    for module in &mut graph.modules {
        // Runtime bindings preserve their module's source order. Constant values
        // cross imports through the declaration environment, not shared ASTs.
        for statement in &mut module.source.body {
            if !matches!(
                statement.kind,
                StmtKind::Binding(_, _, true, _, _) | StmtKind::Constant(..)
            ) {
                block(
                    source,
                    &template,
                    std::slice::from_mut(statement),
                    &mut globals,
                    options,
                    None,
                )?;
            }
        }
        for function in &mut module.source.functions {
            let mut env = globals.clone();
            for p in &function.params {
                env.insert(key(&p.name), None);
            }
            shadow(&function.body, &mut env);
            block(
                source,
                &template,
                &mut function.body,
                &mut env,
                options,
                Some(&globals),
            )?;
        }
        for imp in &mut module.source.implementations {
            for function in &mut imp.methods {
                let mut env = globals.clone();
                for p in &function.params {
                    env.insert(key(&p.name), None);
                }
                shadow(&function.body, &mut env);
                block(
                    source,
                    &template,
                    &mut function.body,
                    &mut env,
                    options,
                    Some(&globals),
                )?;
            }
        }
        for property in &mut module.source.properties {
            let mut env = globals.clone();
            for p in &property.getter.params {
                env.insert(key(&p.name), None);
            }
            shadow(&property.getter.body, &mut env);
            block(
                source,
                &template,
                &mut property.getter.body,
                &mut env,
                options,
                Some(&globals),
            )?;
        }
    }
    Ok(())
}
