use crate::intrinsic::{Intrinsic, Lowering};
use crate::ir::{self, Op, Reg, Slot};
use cyallo_ast::IdentifierEq as _;
use cyallo_ast::*;
use std::collections::HashMap;
type Result<T> = std::result::Result<T, Diagnostic>;
fn key(s: &str) -> String {
    s.to_owned()
}
#[derive(Debug, Clone)]
struct Signature {
    kind: RoutineKind,
    params: Vec<Param>,
    result: Type,
    id: usize,
}
fn temporary(expression: &Expr) -> bool {
    match &expression.kind {
        ExprKind::Name(_)
        | ExprKind::Field(..)
        | ExprKind::TupleField(..)
        | ExprKind::Index(..)
        | ExprKind::Unary(Operator::Dereference, _) => false,
        ExprKind::Parenthesized(inner) => temporary(inner),
        _ => true,
    }
}
struct Lower<'a> {
    function: ir::Function,
    globals: &'a [ir::Local],
    signatures: &'a HashMap<String, Signature>,
    types: &'a HashMap<String, Type>,
    services: &'a std::sync::Arc<crate::services::Metadata>,
    span: Span,
    options: CompileOptions,
    loops: Vec<LoopScope>,
    moves: std::collections::HashSet<(u32, u32)>,
    entry: bool,
    block_depth: usize,
    drop_scopes: Vec<Vec<(Slot, Type)>>,
}
struct RangeIteration<'e> {
    start: &'e Expr,
    end: &'e Expr,
    inclusive: bool,
    reversed: bool,
}
fn not_a_range(span: Span) -> Diagnostic {
    Diagnostic::new(
        "E109",
        "for-in iterates over a range: `a..b`, `a..=b`, or `(a..b).rev()`",
        span,
    )
}
struct LoopScope {
    drop_depth: usize,
    breaks: Vec<usize>,
    continues: Vec<usize>,
}
pub use crate::{CompileOptions, Dialect};
pub fn analyze(
    source_len: usize,
    mut ast: Ast,
    options: CompileOptions,
    deferred: bool,
) -> Result<ir::Module> {
    if options.dialect != Dialect::Modern {
        resolve_compatibility_calls(&mut ast)?;
    }
    let mut requires_tracing = false;
    cyallo_ast::visit::visit_block(
        &mut ast.body,
        &mut |expression| {
            requires_tracing |= matches!(expression.kind, ExprKind::ManagedNew(..))
                || matches!(&expression.kind, ExprKind::IntrinsicCall(id, ..) if Intrinsic::from_id(*id) == Some(Intrinsic::RcTryNewIn));
            Ok(())
        },
        &mut |_, _| Ok(()),
    )?;
    for function in &mut ast.functions {
        cyallo_ast::visit::visit_block(
            &mut function.body,
            &mut |expression| {
                requires_tracing |= matches!(expression.kind, ExprKind::ManagedNew(..))
                    || matches!(&expression.kind, ExprKind::IntrinsicCall(id, ..) if Intrinsic::from_id(*id) == Some(Intrinsic::RcTryNewIn));
                Ok(())
            },
            &mut |_, _| Ok(()),
        )?;
    }
    let mut routine_types = crate::composites::resolve(&mut ast)?;
    let records = std::sync::Arc::new(routine_types.clone());
    let services = std::sync::Arc::new(crate::services::prepare(&mut ast)?);
    for t in &services.traits {
        for m in &t.methods {
            routine_types.insert(
                format!("$METHOD${}.{}", key(&t.name), key(&m.name)),
                Type::Function(m.kind, m.params.clone(), Box::new(m.result.clone())),
            );
        }
    }
    for imp in &services.implementations {
        for method in &services.traits[imp.trait_id].methods {
            let name = format!("$CONCRETE${:?}.{}", imp.receiver, key(&method.name));
            let ty = Type::Function(
                method.kind,
                method.params.clone(),
                Box::new(method.result.clone()),
            );
            if routine_types.insert(name.clone(), ty).is_some() {
                routine_types.insert(name, Type::Void);
            }
        }
    }
    routine_types.extend(
        ast.functions
            .iter()
            .map(|f| (key(&f.name), crate::composites::function_type(f))),
    );
    let source_names = ast.source_names.clone();
    let mut globals = collect(&ast.body, &[])?;
    if matches!(options.dialect, Dialect::Modern | Dialect::Simplified) {
        crate::inference::infer(&ast.body, &mut globals, &[], &routine_types, options)?;
    }
    crate::composites::lift(&mut ast, &globals, &routine_types, options)?;
    for function in &mut ast.functions {
        cyallo_ast::visit::visit_block(
            &mut function.body,
            &mut |expression| {
                requires_tracing |= matches!(expression.kind, ExprKind::ManagedNew(..))
                    || matches!(&expression.kind, ExprKind::IntrinsicCall(id, ..) if Intrinsic::from_id(*id) == Some(Intrinsic::RcTryNewIn));
                Ok(())
            },
            &mut |_, _| Ok(()),
        )?;
    }
    routine_types.extend(
        ast.functions
            .iter()
            .map(|f| (key(&f.name), crate::composites::function_type(f))),
    );
    globals = collect(&ast.body, &[])?;
    if matches!(options.dialect, Dialect::Modern | Dialect::Simplified) {
        crate::inference::infer(&ast.body, &mut globals, &[], &routine_types, options)?;
    }
    for local in &mut globals {
        if let Some(name) = source_names.get(&local.name) {
            local.display_name = Some(name.clone());
        }
    }
    let mut signatures = HashMap::new();
    for (i, f) in ast.functions.iter().enumerate() {
        collect(&[], &f.params)?;
        if signatures.contains_key(&key(&f.name))
            || ast.records.iter().any(|r| r.name.eq_identifier(&f.name))
            || (options.dialect != Dialect::Modern && crate::builtins::standard(&f.name))
            || globals.iter().any(|g| key(&g.name) == key(&f.name))
        {
            return Err(Diagnostic::new(
                "E101",
                "duplicate or reserved routine name",
                f.span,
            ));
        }
        if f.kind == RoutineKind::Function
            && f.origin == RoutineOrigin::Source
            && f.params.iter().any(|p| p.by_ref)
        {
            return Err(Diagnostic::new(
                "E102",
                "FUNCTION parameters must be BYVAL",
                f.span,
            ));
        }
        signatures.insert(
            key(&f.name),
            Signature {
                kind: f.kind,
                params: f.params.clone(),
                result: f.result.clone(),
                id: i + 1,
            },
        );
    }
    let mut functions = vec![];
    let mut main = Lower {
        function: ir::Function {
            generator: None,
            symbol: "<main>".into(),
            origin: RoutineOrigin::Source,
            attributes: Default::default(),
            analysis_error: None,
            resume: Default::default(),
            gc: Default::default(),
            kind: RoutineKind::Procedure,
            span: Span::new(0, source_len as u32),
            pure: false,
            effects: Default::default(),
            name: "<main>".into(),
            locals: globals.clone(),
            local_storage: Default::default(),
            params: vec![],
            result: Type::Void,
            registers: vec![],
            code: vec![],
        },
        globals: &globals,
        signatures: &signatures,
        types: &routine_types,
        services: &services,
        span: Span::default(),
        options,
        loops: vec![],
        moves: Default::default(),
        entry: false,
        block_depth: 0,
        drop_scopes: vec![],
    };
    main.entry = true;
    if options.dialect == Dialect::Modern {
        main.moves = crate::ownership::check(
            &ast.body,
            &globals,
            &[],
            &routine_types,
            &[],
            false,
            &Type::Void,
        )?;
    }
    main.block(&ast.body)?;
    main.emit(Op::Return(None));
    functions.push(main.function);
    if deferred {
        let context = std::sync::Arc::new(DeferredContext {
            source_names: source_names.clone(),
            globals,
            signatures,
            routine_types,
            options,
            services: services.clone(),
            headers: ast.functions.iter().map(blank_routine).collect(),
        });
        let mut checked = context.headers.clone();
        checked.insert(0, functions.pop().unwrap());
        crate::effects::analyze_one(&mut checked, 0, &services)?;
        let mut main = checked.remove(0);
        conservative_calls(&mut main);
        finish_lowering(&mut main, true);
        let mut routines = vec![ir::Routine::ready(main)];
        for (index, f) in ast.functions.into_iter().enumerate() {
            let estimate = crate::admission::estimate(&f);
            routines.push(ir::Routine::deferred(
                DeferredBody {
                    function: f,
                    context: context.clone(),
                    index: index + 1,
                },
                estimate.eager_candidate(),
                estimate.shape,
                estimate.nodes.saturating_mul(2),
                estimate
                    .calls
                    .iter()
                    .filter_map(|n| context.signatures.get(&key(n)).map(|s| s.id))
                    .collect(),
            ));
        }
        return Ok(ir::Module {
            records,
            requires_tracing,
            services,
            functions: routines,
        });
    }
    let bodies = cyallo_compile_jobs::Jobs(options.jobs).map(&ast.functions, 4, |_, _, f| {
        lower_routine(
            f,
            &globals,
            &signatures,
            &routine_types,
            options,
            &services,
            &source_names,
        )
    });
    for body in bodies {
        functions.push(body?);
    }
    crate::effects::analyze(&mut functions, &services)?;
    cyallo_compile_jobs::Jobs(options.jobs).for_each_mut(
        &mut functions,
        4,
        |_, index, function| finish_lowering(function, index == 0),
    );
    let functions: Vec<_> = functions.into_iter().map(ir::Routine::ready).collect();
    Ok(ir::Module {
        records,
        requires_tracing,
        services,
        functions,
    })
}
fn lower_routine(
    f: &cyallo_ast::Function,
    globals: &[ir::Local],
    signatures: &HashMap<String, Signature>,
    routine_types: &HashMap<String, Type>,
    options: CompileOptions,
    services: &std::sync::Arc<crate::services::Metadata>,
    source_names: &HashMap<Box<str>, Box<str>>,
) -> Result<ir::Function> {
    if f.generator && crate::generator::parts(&f.result).is_none() {
        return Err(cyallo_ast::diagnostic::generator_factory_signature_required(f.span));
    }
    let mut locals = collect(&f.body, &f.params)?;
    crate::generator::input_local(f, &mut locals);
    if matches!(options.dialect, Dialect::Modern | Dialect::Simplified) {
        crate::inference::infer(&f.body, &mut locals, globals, routine_types, options)?;
    }
    for local in &mut locals {
        if let Some(name) = source_names.get(&local.name) {
            local.display_name = Some(name.clone());
        }
    }
    let moves = if options.dialect == Dialect::Modern {
        crate::ownership::check(
            &f.body,
            &locals,
            globals,
            routine_types,
            &f.params,
            f.kind == RoutineKind::Function,
            &crate::generator::result(f),
        )?
    } else {
        Default::default()
    };
    let mut lower = Lower {
        function: ir::Function {
            generator: f.generator.then(|| crate::generator::factory_type(f)),
            symbol: f.name.clone(),
            origin: f.origin,
            attributes: f.attributes,
            analysis_error: None,
            resume: Default::default(),
            gc: Default::default(),
            kind: f.kind,
            span: f.span,
            pure: options.dialect == Dialect::Modern && f.kind == RoutineKind::Function,
            effects: Default::default(),
            name: f.source_name.clone(),
            locals,
            local_storage: Default::default(),
            params: f.params.iter().map(|p| p.by_ref).collect(),
            result: crate::generator::result(f),
            registers: vec![],
            code: vec![],
        },
        globals,
        signatures,
        types: routine_types,
        services,
        span: f.span,
        options,
        loops: vec![],
        moves: Default::default(),
        entry: false,
        block_depth: 0,
        drop_scopes: vec![],
    };
    lower.moves = moves;
    lower.block(&f.body)?;
    if lower.function.result != Type::Void
        && !(f.generator && crate::generator::unit(&lower.function.result))
        && !returns(&f.body)
    {
        return Err(Diagnostic::new(
            "E103",
            "non-void routine must return a value on every path",
            f.span,
        ));
    }
    lower.emit(Op::Return(None));
    Ok(lower.function)
}
#[derive(Debug)]
struct DeferredContext {
    source_names: HashMap<Box<str>, Box<str>>,
    services: std::sync::Arc<crate::services::Metadata>,
    globals: Vec<ir::Local>,
    signatures: HashMap<String, Signature>,
    routine_types: HashMap<String, Type>,
    options: CompileOptions,
    headers: Vec<ir::Function>,
}
#[derive(Debug)]
pub struct DeferredBody {
    pub function: cyallo_ast::Function,
    context: std::sync::Arc<DeferredContext>,
    index: usize,
}
fn blank_routine(f: &cyallo_ast::Function) -> ir::Function {
    ir::Function {
        generator: f.generator.then(|| crate::generator::factory_type(f)),
        symbol: f.name.clone(),
        origin: f.origin,
        attributes: f.attributes,
        analysis_error: None,
        resume: Default::default(),
        gc: Default::default(),
        kind: f.kind,
        span: f.span,
        pure: false,
        effects: Default::default(),
        name: f.source_name.clone(),
        locals: vec![],
        local_storage: Default::default(),
        params: f.params.iter().map(|p| p.by_ref).collect(),
        result: crate::generator::result(f),
        registers: vec![],
        code: vec![],
    }
}
impl DeferredBody {
    pub fn analyze(&self) -> ir::Function {
        let c = &self.context;
        let result = (|| {
            let function = lower_routine(
                &self.function,
                &c.globals,
                &c.signatures,
                &c.routine_types,
                c.options,
                &c.services,
                &c.source_names,
            )?;
            // Check local purity against declaration contracts without forcing
            // cold callees. Every callee must pass its own check before entry.
            let mut functions = c.headers.clone();
            let mut main = blank_routine(&self.function);
            main.name = "<main>".into();
            main.locals = c.globals.clone();
            functions.insert(0, main);
            functions[self.index] = function;
            crate::effects::analyze_one(&mut functions, self.index, &c.services)?;
            let mut function = functions.swap_remove(self.index);
            if function.code.iter().any(|i| matches!(i.op, Op::Call(..))) {
                function.effects.transitive_complete = false;
                function.effects.may_allocate = true;
                function.effects.may_fail = true;
                function.effects.may_diverge = true;
            }
            conservative_calls(&mut function);
            finish_lowering(&mut function, false);
            Ok::<_, Diagnostic>(function)
        })();
        result.unwrap_or_else(|error| {
            let mut f = blank_routine(&self.function);
            f.analysis_error = Some(error);
            f
        })
    }
}
fn finish_lowering(function: &mut ir::Function, is_main: bool) {
    if is_main {
        function.local_storage = (0..function.locals.len())
            .map(ir::LocalStorage::Cell)
            .collect();
    } else {
        place_frame_locals(function);
    }
    function.gc = crate::gc_metadata::GcMetadata::build(function);
    function.resume = crate::osr::ResumeMetadata::build(function);
}

/// A scalar can live in a frame register if all uses of its address are direct
/// reads or writes in that invocation. Checked lowering gives each Address its
/// own destination register, including when the instruction is inside a loop.
fn place_frame_locals(function: &mut ir::Function) {
    use ir::LocalStorage;
    let mut direct: Vec<_> = function
        .locals
        .iter()
        .enumerate()
        .map(|(i, local)| {
            matches!(
                local.ty,
                Type::Integer
                    | Type::Real
                    | Type::Boolean
                    | Type::Char
                    | Type::Context
                    | Type::Registration
                    | Type::ServiceKey(_)
                    | Type::ServiceRef(_)
                    | Type::TraitObject(_)
            ) && function.params.get(i) != Some(&true)
        })
        .collect();
    let mut origins = vec![None; function.registers.len()];
    let mut first_address = vec![None; function.locals.len()];
    for inst in &function.code {
        if let Op::Address(r, slot) = inst.op
            && !slot.global
            && direct[slot.index]
        {
            origins[r] = Some(slot.index);
            first_address[slot.index].get_or_insert(r);
        }
        if let Op::Move(destination, source) = inst.op {
            origins[destination] = origins[source];
        }
    }
    for inst in &function.code {
        let direct_use = match inst.op {
            Op::Load(_, addr) | Op::Store(addr, _) | Op::Input(addr, _) | Op::Move(_, addr) => {
                Some(addr)
            }
            _ => None,
        };
        for r in crate::osr::uses_defs(&inst.op).0 {
            if Some(r) != direct_use
                && let Some(local) = origins[r]
            {
                direct[local] = false;
            }
        }
    }

    let mut cells = 0;
    function.local_storage = function
        .locals
        .iter()
        .enumerate()
        .map(|(i, local)| {
            if direct[i] {
                // Reuse the first eliminated address slot. Unused named locals
                // still need a slot for argument setup and debugger inspection.
                let register = first_address[i].unwrap_or_else(|| {
                    let r = function.registers.len();
                    function.registers.push(local.ty.clone());
                    r
                });
                LocalStorage::Register(register)
            } else {
                let index = cells;
                cells += 1;
                LocalStorage::Cell(index)
            }
        })
        .collect();
    let frame_register = |r: Reg| match function.local_storage[origins[r]?] {
        LocalStorage::Register(r) => Some(r),
        LocalStorage::Cell(_) => None,
    };
    for inst in &mut function.code {
        // Keep original PCs and spans, so stepping and fuel costs stay exact.
        let op = match &inst.op {
            Op::Address(r, _) => frame_register(*r).map(|_| Op::Nop),
            Op::Move(r, _) if origins[*r].is_some() => frame_register(*r).map(|_| Op::Nop),
            Op::Load(r, a) => frame_register(*a).map(|a| Op::FrameLoad(*r, a)),
            Op::Store(a, r) => frame_register(*a).map(|a| Op::FrameStore(a, *r)),
            Op::Input(a, ty) => frame_register(*a).map(|a| Op::FrameInput(a, ty.clone())),
            _ => None,
        };
        if let Some(op) = op {
            inst.op = op;
        }
    }
}

fn conservative_calls(function: &mut ir::Function) {
    if function.code.iter().any(|i| matches!(i.op, Op::Call(..))) {
        let effect = &mut function.effects;
        effect.transitive_complete = false;
        effect.may_allocate = true;
        effect.may_fail = true;
        effect.may_diverge = true;
        if !function.pure {
            effect.reads_mutable_globals = true;
            effect.writes_globals = true;
            effect.writes_references = true;
            effect.io = true;
            effect.nondeterministic = true;
        }
    }
}
fn returns(body: &[Stmt]) -> bool {
    body.iter().any(|s| match &s.kind {
        StmtKind::Return(_) => true,
        StmtKind::If(_, a, b) => returns(a) && returns(b),
        StmtKind::Case(_, arms, b) => {
            !arms.is_empty() && arms.iter().all(|(_, b)| returns(b)) && returns(b)
        }
        _ => false,
    })
}
pub fn literal_type(l: &Literal) -> Type {
    match l {
        Literal::Number(n) => Type::numeric(n.kind),
        Literal::Integer(_) => Type::Integer,
        Literal::Real(_) => Type::Real,
        Literal::Boolean(_) => Type::Boolean,
        Literal::Char(_) => Type::Char,
        Literal::String(_) => Type::String,
    }
}
fn const_type(e: &Expr) -> Result<Type> {
    match &e.kind {
        ExprKind::Frozen(_, ty) => Ok(ty.clone()),
        ExprKind::Literal(l) => Ok(literal_type(l)),
        ExprKind::Unary(op, a)
            if *op == Operator::Subtract
                && matches!(a.kind, ExprKind::Literal(Literal::Real(_))) =>
        {
            Ok(Type::Real)
        }
        _ => Err(Diagnostic::new(
            "E104",
            "CONSTANT requires a literal value",
            e.span,
        )),
    }
}
pub fn collect(body: &[Stmt], params: &[Param]) -> Result<Vec<ir::Local>> {
    let mut locals = vec![];
    let mut seen = HashMap::new();
    for p in params {
        if locals
            .iter()
            .any(|l: &ir::Local| l.name.eq_identifier(&p.name))
        {
            return Err(Diagnostic::new("E101", "duplicate parameter", p.span));
        }
        seen.insert(key(&p.name), p.span);
        locals.push(ir::Local {
            display_name: Some(p.name.clone()),
            name: p.name.clone(),
            ty: p.ty.clone(),
            constant: false,
            span: p.span,
        });
    }
    fn walk(
        body: &[Stmt],
        locals: &mut Vec<ir::Local>,
        seen: &mut HashMap<String, Span>,
    ) -> Result<()> {
        for s in body {
            if let StmtKind::Declare(name, _)
            | StmtKind::Constant(name, _)
            | StmtKind::Binding(name, _, _, _, _) = &s.kind
                && let Some(first) = seen.insert(key(name), s.span)
            {
                return Err(Diagnostic::new(
                    "E101",
                    format!("duplicate name `{}`", cyallo_ast::source_name(name)),
                    s.span,
                )
                .with_related(first, "first declared here"));
            }
            let decl = match &s.kind {
                StmtKind::Declare(n, t) => Some((n, t.clone(), false)),
                StmtKind::Binding(n, Some(t), constant, mutable, _) => {
                    Some((n, t.clone(), *constant || !*mutable))
                }
                StmtKind::Constant(n, e) => {
                    let t = const_type(e)?;
                    Some((n, t, true))
                }
                _ => None,
            };
            if let Some((n, t, c)) = decl {
                if let Some(first) = locals.iter().find(|l| l.name.eq_identifier(n)) {
                    return Err(Diagnostic::new(
                        "E101",
                        format!("duplicate name `{}`", cyallo_ast::source_name(n)),
                        s.span,
                    )
                    .with_related(first.span, "first declared here"));
                }
                locals.push(ir::Local {
                    display_name: Some(n.clone()),
                    name: n.clone(),
                    ty: t,
                    constant: c,
                    span: s.span,
                });
            }
            match &s.kind {
                StmtKind::If(_, a, b) => {
                    walk(a, locals, seen)?;
                    walk(b, locals, seen)?;
                }
                StmtKind::While(_, b)
                | StmtKind::Repeat(b, _)
                | StmtKind::For(_, _, _, _, b)
                | StmtKind::ForIn(_, _, b) => walk(b, locals, seen)?,
                StmtKind::Case(_, arms, b) => {
                    for (_, b) in arms {
                        walk(b, locals, seen)?;
                    }
                    walk(b, locals, seen)?;
                }
                _ => {}
            }
        }
        Ok(())
    }
    walk(body, &mut locals, &mut seen)?;
    Ok(locals)
}
fn numeric(t: &Type) -> bool {
    t.number_kind().is_some()
}
pub fn contextual_number(expression: &Expr) -> bool {
    match &expression.kind {
        ExprKind::Literal(Literal::Integer(_) | Literal::Real(_)) => true,
        ExprKind::Parenthesized(inner)
        | ExprKind::Unary(Operator::Add | Operator::Subtract, inner) => contextual_number(inner),
        ExprKind::Binary(operator, a, b) if operator.is_arithmetic() => {
            contextual_number(a) && contextual_number(b)
        }
        _ => false,
    }
}
pub fn numeric_context(a: &Expr, ta: &Type, b: &Expr, tb: &Type) -> Type {
    match (contextual_number(a), contextual_number(b)) {
        (true, false) => tb.clone(),
        (false, _) => ta.clone(),
        (true, true) => {
            if *ta == Type::Real || *tb == Type::Real {
                Type::Real
            } else {
                ta.clone()
            }
        }
    }
}
pub fn intrinsic_arguments(
    id: Intrinsic,
    type_args: &[Type],
    arguments: &[Expr],
    known: &HashMap<String, Type>,
    routines: &HashMap<String, Type>,
) -> Result<Vec<Expr>> {
    let expected = if id == Intrinsic::LayoutNew {
        Some(Type::numeric(NumberKind::Usize))
    } else if matches!(
        id,
        Intrinsic::VecLiteral | Intrinsic::DequeLiteral | Intrinsic::CellNew
    ) {
        type_args.first().cloned()
    } else if matches!(
        id,
        Intrinsic::Abs
            | Intrinsic::Min
            | Intrinsic::Max
            | Intrinsic::Clamp
            | Intrinsic::NumAbs
            | Intrinsic::NumMin
            | Intrinsic::NumMax
            | Intrinsic::NumClamp
            | Intrinsic::Round
            | Intrinsic::Floor
            | Intrinsic::Ceil
            | Intrinsic::Sqrt
            | Intrinsic::Pow
            | Intrinsic::FloatRound
            | Intrinsic::FloatFloor
            | Intrinsic::FloatCeil
            | Intrinsic::FloatSqrt
            | Intrinsic::FloatPow
    ) {
        arguments
            .iter()
            .find(|argument| !contextual_number(argument))
            .or_else(|| {
                arguments
                    .iter()
                    .find(|argument| matches!(argument.kind, ExprKind::Literal(Literal::Real(_))))
            })
            .or(arguments.first())
            .map(|argument| crate::inference::expression_type(argument, known, routines))
            .transpose()?
    } else {
        None
    };
    arguments
        .iter()
        .map(|argument| {
            expected
                .as_ref()
                .map(|ty| contextualize(argument, ty))
                .unwrap_or_else(|| Ok(argument.clone()))
        })
        .collect()
}
pub fn contextualize(expression: &Expr, wanted: &Type) -> Result<Expr> {
    let mut e = expression.clone();
    if let Some(kind) = wanted.number_kind() {
        let number = match e.kind {
            ExprKind::Literal(Literal::Integer(n)) => Some(Number {
                kind: NumberKind::I64,
                bits: n as u64,
            }),
            ExprKind::Literal(Literal::Real(n)) => Some(Number {
                kind: NumberKind::F64,
                bits: n.to_bits(),
            }),
            _ => None,
        };
        if let Some(number) = number {
            if number.kind.float() && !kind.float() {
                return Err(Diagnostic::new(
                    "E109",
                    "floating literal requires a floating type or an explicit cast",
                    e.span,
                ));
            }
            // Integer literals never become floats implicitly; `2` in a float
            // context is a type error, as in Rust. Write `2.0` or cast.
            if kind.float() && !number.kind.float() {
                return Ok(e);
            }
            e.kind = ExprKind::Literal(Literal::number(number.cast(kind).map_err(|error| {
                cyallo_ast::diagnostic::numeric_conversion_failed(error, e.span)
            })?));
        } else if let ExprKind::Binary(op, a, b) = &mut e.kind
            && matches!(
                *op,
                Operator::Add
                    | Operator::Subtract
                    | Operator::Multiply
                    | Operator::Divide
                    | Operator::RealDivide
                    | Operator::IntegerDivide
                    | Operator::Remainder
            )
        {
            **a = contextualize(a, wanted)?;
            **b = contextualize(b, wanted)?;
        } else if let ExprKind::Parenthesized(a)
        | ExprKind::Unary(Operator::Add | Operator::Subtract, a) = &mut e.kind
        {
            **a = contextualize(a, wanted)?;
        }
    }
    if let ExprKind::ArrayLiteral(values) = &e.kind
        && values.is_empty()
        && let Type::Array(bounds, _) = wanted
        && bounds == &[(0, -1)]
    {
        e.kind = ExprKind::Frozen(
            std::sync::Arc::new(ConstantValue::Array(Vec::new())),
            wanted.clone(),
        );
    }
    if let (ExprKind::ArrayLiteral(values), Type::Array(_, element) | Type::SparseArray(element)) =
        (&mut e.kind, wanted)
    {
        for value in values {
            *value = contextualize(value, element)?;
        }
    }
    if let Type::Storage(kind, element) = wanted
        && matches!(*kind, StorageKind::Vec | StorageKind::VecDeque)
        && let ExprKind::IntrinsicCall(id, types, values) = &mut e.kind
        && matches!(
            Intrinsic::from_id(*id),
            Some(Intrinsic::VecLiteral | Intrinsic::DequeLiteral)
        )
    {
        for value in values.iter_mut() {
            *value = contextualize(value, element)?;
        }
        *types = vec![*element.clone()];
    }
    Ok(e)
}
fn assignable(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Name(_) | ExprKind::Unary(Operator::Dereference, _) => true,
        ExprKind::Parenthesized(a)
        | ExprKind::Index(a, _)
        | ExprKind::Field(a, _)
        | ExprKind::ExtensionField(a, _, _)
        | ExprKind::TupleField(a, _) => assignable(a),
        _ => false,
    }
}
/// The operation a typed binary operator performs: `+` producing a string is concatenation.
/// Types with structural equality: scalars, strings, and tuples, records and
/// fixed arrays built from them. Identity-bearing values are excluded.
fn comparable(ty: &Type) -> bool {
    match ty {
        Type::Integer
        | Type::Real
        | Type::Numeric(_)
        | Type::Boolean
        | Type::Char
        | Type::String
        | Type::Symbol => true,
        Type::Tuple(items) => items.iter().all(comparable),
        Type::Record(_, fields) => fields.iter().all(|(_, ty)| comparable(ty)),
        Type::Array(_, element) => comparable(element),
        _ => false,
    }
}
/// Integer `/` is the checked, truncating integer division.
fn specialized_operator(op: Operator, result: &Type) -> Operator {
    match (op, result) {
        (Operator::Add, Type::String) => Operator::Concat,
        (Operator::Divide, result) if result.number_kind().is_some_and(|kind| !kind.float()) => {
            Operator::IntegerDivide
        }
        _ => op,
    }
}
pub fn binary_type(op: cyallo_ast::Operator, a: &Type, b: &Type, span: Span) -> Result<Type> {
    if matches!(a, Type::Numeric(_)) || matches!(b, Type::Numeric(_)) {
        if a != b {
            return Err(Diagnostic::new(
                "E109",
                "numeric operands require identical types; use a checked cast",
                span,
            ));
        }
        let kind = a.number_kind().unwrap();
        return match op {
            Operator::Equal
            | Operator::NotEqual
            | Operator::Less
            | Operator::Greater
            | Operator::LessEqual
            | Operator::GreaterEqual => Ok(Type::Boolean),
            Operator::Add
            | Operator::Subtract
            | Operator::Multiply
            | Operator::Divide
            | Operator::Remainder => Ok(a.clone()),
            Operator::IntegerDivide if !kind.float() => Ok(a.clone()),
            _ => Err(Diagnostic::new("E105", "invalid numeric operation", span)),
        };
    }
    let t = match op {
        Operator::And | Operator::Or if *a == Type::Boolean && *b == Type::Boolean => {
            Some(Type::Boolean)
        }
        Operator::Concat
            if matches!(a, Type::String | Type::Char) && matches!(b, Type::String | Type::Char) =>
        {
            Some(Type::String)
        }
        // `+` concatenates when either side is a string; char + char stays an error.
        Operator::Add
            if matches!(
                (a, b),
                (Type::String, Type::String | Type::Char) | (Type::Char, Type::String)
            ) =>
        {
            Some(Type::String)
        }
        Operator::IntegerDivide | Operator::Remainder | Operator::Divide
            if *a == Type::Integer && *b == Type::Integer =>
        {
            Some(Type::Integer)
        }
        Operator::Remainder | Operator::Divide if *a == Type::Real && *b == Type::Real => {
            Some(Type::Real)
        }
        Operator::RealDivide if numeric(a) && numeric(b) => Some(Type::Real),
        Operator::Add | Operator::Subtract | Operator::Multiply if numeric(a) && numeric(b) => {
            Some(if *a == Type::Real || *b == Type::Real {
                Type::Real
            } else {
                Type::Integer
            })
        }
        Operator::Equal | Operator::NotEqual if a == b && comparable(a) => Some(Type::Boolean),
        Operator::Less | Operator::Greater | Operator::LessEqual | Operator::GreaterEqual
            if (numeric(a) && numeric(b)) || (a == b && matches!(a, Type::Char | Type::String)) =>
        {
            Some(Type::Boolean)
        }
        Operator::Equal | Operator::NotEqual if numeric(a) && numeric(b) => Some(Type::Boolean),
        _ => None,
    };
    t.ok_or_else(|| {
        Diagnostic::new(
            "E105",
            format!("operator `{op}` cannot be applied to `{a}` and `{b}`"),
            span,
        )
    })
}
impl Lower<'_> {
    fn concrete_addressable(&self, e: &Expr, known: &HashMap<String, Type>) -> bool {
        match &e.kind {
            ExprKind::Name(_) => true,
            ExprKind::Parenthesized(a)
            | ExprKind::Field(a, _)
            | ExprKind::TupleField(a, _)
            | ExprKind::Index(a, _) => self.concrete_addressable(a, known),
            ExprKind::ExtensionField(a, n, _) => {
                self.concrete_addressable(a, known)
                    && matches!(crate::inference::expression_type(a,known,self.types),Ok(Type::Record(_,fs)) if fs.iter().any(|(f,_)|f.eq_identifier(n)))
            }
            _ => false,
        }
    }
    fn lookup_op(
        &mut self,
        op: crate::lookup::Operation,
        ty: Type,
        rs: Vec<Reg>,
        span: Span,
    ) -> Result<(Reg, Type)> {
        if rs
            .iter()
            .any(|r| crate::composites::borrowed(&self.function.registers[*r]))
        {
            return Err(Diagnostic::new(
                "E117",
                "borrowed values cannot enter managed storage or lookup plans",
                span,
            ));
        }
        let r = self.reg(ty.clone());
        self.span = span;
        self.emit(Op::Lookup(r, op, rs));
        Ok((r, ty))
    }
    fn field_type(&self, t: &Type, field: &ExprKind, span: Span) -> Result<(usize, Type)> {
        let t = crate::composites::type_view(t, self.types);
        match (&t, field) {
            (Type::Record(_, fs), ExprKind::Field(_, n)) => fs
                .iter()
                .enumerate()
                .find(|(_, (name, _))| name.eq_identifier(n))
                .map(|(i, (_, t))| (i, t.clone())),
            (Type::Tuple(ts), ExprKind::TupleField(_, i)) => ts.get(*i).map(|t| (*i, t.clone())),
            _ => None,
        }
        .ok_or_else(|| Diagnostic::new("E116", "unknown field or tuple index", span))
    }
    fn invoke(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        span: Span,
        discard: bool,
    ) -> Result<(Reg, Type)> {
        if let ExprKind::Parenthesized(inner) = &callee.kind {
            return self.invoke(inner, args, span, discard);
        }
        if let ExprKind::ClosureValue(name, captures, _) = &callee.kind
            && captures.is_empty()
        {
            // A frozen routine reference carries declaration identity. Resolve
            // through signatures even if a local shadows its source name.
            let sig = self.signatures[&key(name)].clone();
            if sig.result == Type::Void && !discard {
                return Err(Diagnostic::new(
                    "E110",
                    "procedure cannot be used as an expression",
                    span,
                ));
            }
            let args = self.arguments(&sig, args)?;
            let r = self.reg(sig.result.clone());
            self.span = span;
            self.emit(Op::Call(
                (sig.result != Type::Void).then_some(r),
                sig.id,
                args,
            ));
            return Ok((r, sig.result));
        }
        if let ExprKind::ExtensionField(base, name, _) = &callee.kind {
            let known = self
                .globals
                .iter()
                .chain(self.function.locals.iter())
                .map(|l| (key(&l.name), l.ty.clone()))
                .collect();
            let ty = crate::inference::expression_type(base, &known, self.types)?;
            let real = match &ty {
                Type::Context => [
                    "extend",
                    "isolate",
                    "service",
                    "provide",
                    "available",
                    "dispose",
                    "inject",
                    "on_dispose",
                ]
                .iter()
                .any(|n| name.eq_identifier(n)),
                Type::Registration => name.eq_identifier("dispose"),
                Type::TraitObject(t) => self.services.trait_id(t).is_some_and(|id| {
                    self.services.traits[id]
                        .methods
                        .iter()
                        .any(|m| m.name.eq_identifier(name))
                }),
                Type::ServiceRef(t) => {
                    if let Type::TraitObject(t) = t.as_ref() {
                        self.services.trait_id(t).is_some_and(|id| {
                            self.services.traits[id]
                                .methods
                                .iter()
                                .any(|m| m.name.eq_identifier(name))
                        })
                    } else {
                        false
                    }
                }
                _ => self.services.concrete_method(&ty, name).is_some(),
            };
            if real {
                return self.invoke(
                    &Expr {
                        kind: ExprKind::Field(base.clone(), name.clone()),
                        span: callee.span,
                    },
                    args,
                    span,
                    discard,
                );
            }
        }
        if let ExprKind::Field(base, name) = &callee.kind {
            let known = self
                .globals
                .iter()
                .chain(self.function.locals.iter())
                .map(|l| (key(&l.name), l.ty.clone()))
                .collect();
            let receiver_ty = crate::inference::expression_type(base, &known, self.types)?;
            let actual_field = matches!(&receiver_ty,Type::Record(_,fs) if fs.iter().any(|(n,_)|n.eq_identifier(name)));
            if !actual_field
                && let Some((implementation, method)) = self
                    .services
                    .concrete_method(receiver_ty.pointee_or_self(), name)
            {
                let m = self.services.traits
                    [self.services.implementations[implementation].trait_id]
                    .methods[method]
                    .clone();
                let id = self.services.implementations[implementation].methods[method];
                if m.result == Type::Void && !discard {
                    return Err(Diagnostic::new(
                        "E110",
                        "procedure cannot be used as an expression",
                        span,
                    ));
                }
                let addressable = self.concrete_addressable(base, &known);
                let receiver = if m.receiver != crate::services::Receiver::Value && addressable {
                    self.projection_address(base, m.receiver == crate::services::Receiver::Mutable)?
                        .0
                } else {
                    let (value, ty) = self.expr(base)?;
                    if m.receiver != crate::services::Receiver::Value {
                        let index = self.function.locals.len();
                        self.function.locals.push(ir::Local {
                            display_name: None,
                            name: format!("$receiver{}:{}", span.start, span.end).into(),
                            ty: ty.clone(),
                            constant: false,
                            span,
                        });
                        let address = self.reg(ty);
                        self.emit(Op::Address(
                            address,
                            Slot {
                                global: false,
                                index,
                            },
                        ));
                        self.emit(Op::Store(address, value));
                        address
                    } else {
                        value
                    }
                };
                let sig = Signature {
                    kind: m.kind,
                    params: m
                        .params
                        .iter()
                        .enumerate()
                        .map(|(i, (ty, by_ref))| Param {
                            receiver: false,
                            name: format!("arg{i}").into(),
                            ty: ty.clone(),
                            by_ref: *by_ref,
                            span,
                        })
                        .collect(),
                    result: m.result.clone(),
                    id,
                };
                let mut rs = vec![receiver];
                rs.extend(self.arguments(&sig, args)?);
                let r = self.reg(m.result.clone());
                self.span = span;
                self.emit(Op::Call((m.result != Type::Void).then_some(r), id, rs));
                return Ok((r, m.result));
            }
            if matches!(receiver_ty, Type::Managed { .. })
                || matches!(
                    receiver_ty.pointee_or_self(),
                    Type::Storage(..) | Type::Array(..) | Type::Slice(..) | Type::Managed { .. }
                )
            {
                let parameters = crate::lookup::parameter_types(&receiver_ty, name);
                let known = self
                    .globals
                    .iter()
                    .chain(self.function.locals.iter())
                    .map(|local| (key(&local.name), local.ty.clone()))
                    .collect();
                let arguments = args
                    .iter()
                    .enumerate()
                    .map(|(index, arg)| {
                        parameters
                            .as_ref()
                            .and_then(|types| types.get(index))
                            .map(|ty| contextualize(arg, ty))
                            .unwrap_or_else(|| Ok(arg.clone()))
                    })
                    .collect::<Result<Vec<_>>>()?;
                let types = arguments
                    .iter()
                    .map(|arg| crate::inference::expression_type(arg, &known, self.types))
                    .collect::<Result<Vec<_>>>()?;
                let field = args.first().and_then(cyallo_ast::constant_text);
                let method = crate::lookup::method(&receiver_ty, name, &types, field)
                    .ok_or_else(|| crate::lookup::error(span))?;
                let receiver = match method.receiver {
                    crate::lookup::ReceiverMode::Owner | crate::lookup::ReceiverMode::Move => {
                        self.expr(base)?.0
                    }
                    crate::lookup::ReceiverMode::Value => {
                        let (register, ty) = self.expr(base)?;
                        if let Type::Reference { pointee, .. } | Type::Managed { pointee, .. } = ty
                        {
                            let loaded = self.reg(*pointee);
                            self.emit(Op::Load(loaded, register));
                            loaded
                        } else {
                            register
                        }
                    }
                    mode => {
                        self.projection_address(base, mode == crate::lookup::ReceiverMode::Mutable)?
                            .0
                    }
                };
                let mut operands = vec![receiver];
                for argument in &arguments {
                    operands.push(self.expr(argument)?.0);
                }
                if method.result == Type::Void && !discard {
                    return Err(Diagnostic::new(
                        "E110",
                        "procedure cannot be used as an expression",
                        span,
                    ));
                }
                let result = self.lookup_op(method.operation, method.result, operands, span)?;
                if temporary(base)
                    && crate::ownership::effectful_lifecycle(&receiver_ty)
                    && matches!(
                        method.receiver,
                        crate::lookup::ReceiverMode::Value | crate::lookup::ReceiverMode::Owner
                    )
                {
                    self.lookup_op(
                        crate::lookup::Operation::DropValue,
                        Type::Void,
                        vec![receiver],
                        span,
                    )?;
                }
                return Ok(result);
            }
            if matches!(
                receiver_ty.pointee_or_self(),
                Type::Context | Type::Registration | Type::TraitObject(_) | Type::ServiceRef(_)
            ) {
                let (mut receiver, mut ty) = self.expr(base)?;
                if let Type::Reference { pointee, .. } = ty {
                    ty = *pointee;
                    let value = self.reg(ty.clone());
                    self.emit(Op::Load(value, receiver));
                    receiver = value;
                }
                if let Type::TraitObject(trait_name) = match &ty {
                    Type::ServiceRef(t) => t.as_ref(),
                    t => t,
                } {
                    let trait_id = self
                        .services
                        .trait_id(trait_name)
                        .ok_or_else(|| Diagnostic::new("E130", "unknown trait", span))?;
                    let method = self.services.traits[trait_id]
                        .methods
                        .iter()
                        .position(|m| m.name.eq_identifier(name))
                        .ok_or_else(|| Diagnostic::new("E130", "unknown trait method", span))?;
                    let m = self.services.traits[trait_id].methods[method].clone();
                    let sig = Signature {
                        kind: m.kind,
                        params: m
                            .params
                            .iter()
                            .enumerate()
                            .map(|(i, (t, b))| Param {
                                receiver: false,
                                name: format!("arg{i}").into(),
                                ty: t.clone(),
                                by_ref: *b,
                                span,
                            })
                            .collect(),
                        result: m.result.clone(),
                        id: 0,
                    };
                    if m.result == Type::Void && !discard {
                        return Err(Diagnostic::new(
                            "E110",
                            "procedure cannot be used as an expression",
                            span,
                        ));
                    }
                    let rs = self.arguments(&sig, args)?;
                    let r = self.reg(m.result.clone());
                    self.span = span;
                    self.emit(Op::TraitCall(
                        (m.result != Type::Void).then_some(r),
                        receiver,
                        method,
                        rs,
                    ));
                    return Ok((r, m.result));
                }
                let mut rs = vec![receiver];
                let mut ts = vec![];
                for a in args {
                    let (r, t) = self.expr(a)?;
                    rs.push(r);
                    ts.push(t);
                }
                let (operation, result) =
                    crate::services::intrinsic(&ty, name, &ts).ok_or_else(|| {
                        Diagnostic::new(
                            "E131",
                            format!(
                                "invalid service operation `{}` or argument types",
                                cyallo_ast::source_name(name)
                            ),
                            span,
                        )
                    })?;
                if result == Type::Void && !discard {
                    return Err(Diagnostic::new(
                        "E110",
                        "procedure cannot be used as an expression",
                        span,
                    ));
                }
                let r = self.reg(result.clone());
                self.span = span;
                self.emit(Op::Service(r, operation, rs));
                return Ok((r, result));
            }
        }
        let (mut target, mut ty) = self.expr(callee)?;
        if let Type::Reference { pointee, .. } = ty {
            ty = *pointee;
            let loaded = self.reg(ty.clone());
            self.emit(Op::Load(loaded, target));
            target = loaded;
        }
        let (kind, params, result) = crate::composites::callable(&ty)
            .ok_or_else(|| Diagnostic::new("E110", "value is not callable", span))?;
        let sig = Signature {
            kind,
            params: params
                .iter()
                .enumerate()
                .map(|(i, (ty, by_ref))| Param {
                    receiver: false,
                    name: format!("arg{i}").into(),
                    ty: ty.clone(),
                    by_ref: *by_ref,
                    span,
                })
                .collect(),
            result: result.clone(),
            id: 0,
        };
        if sig.result == Type::Void && !discard {
            return Err(Diagnostic::new(
                "E110",
                "procedure cannot be used as an expression",
                span,
            ));
        }
        let rs = self.arguments(&sig, args)?;
        let r = self.reg(sig.result.clone());
        self.span = span;
        self.emit(Op::Invoke(
            (sig.result != Type::Void).then_some(r),
            target,
            rs,
        ));
        Ok((r, sig.result))
    }
    fn check_borrow_store(&self, target: &Expr, ty: &Type) -> Result<()> {
        if !crate::composites::borrowed(ty) {
            return Ok(());
        }
        match &target.kind {
            ExprKind::Name(n) => {
                let (slot, _) = self.lookup(n)?;
                if slot.global || self.function.params.get(slot.index) == Some(&true) {
                    return Err(Diagnostic::new(
                        "E117",
                        "borrowed closure cannot escape into global or ref storage",
                        target.span,
                    ));
                }
            }
            ExprKind::Parenthesized(a)
            | ExprKind::Index(a, _)
            | ExprKind::Field(a, _)
            | ExprKind::ExtensionField(a, _, _)
            | ExprKind::TupleField(a, _) => self.check_borrow_store(a, ty)?,
            _ => {}
        }
        Ok(())
    }
    /// The bounds and direction of a `for … in` iterable. The iterable is a range
    /// expression, optionally reversed through the resolved `Range::rev` member.
    fn range_iteration<'e>(&self, iterable: &'e Expr) -> Result<RangeIteration<'e>> {
        match &iterable.kind {
            ExprKind::Parenthesized(inner) => self.range_iteration(inner),
            ExprKind::Binary(op @ (Operator::Range | Operator::RangeInclusive), start, end) => {
                Ok(RangeIteration {
                    start,
                    end,
                    inclusive: *op == Operator::RangeInclusive,
                    reversed: false,
                })
            }
            ExprKind::Invoke(callee, arguments) if arguments.is_empty() => {
                let ExprKind::Field(base, name) = &callee.kind else {
                    return Err(not_a_range(iterable.span));
                };
                let known = self
                    .globals
                    .iter()
                    .chain(self.function.locals.iter())
                    .map(|local| (key(&local.name), local.ty.clone()))
                    .collect();
                let receiver = crate::inference::expression_type(base, &known, self.types)?;
                match crate::lookup::method(&receiver, name, &[], None) {
                    Some(crate::lookup::Method {
                        operation: crate::lookup::Operation::RangeReverse,
                        ..
                    }) => {
                        let inner = self.range_iteration(base)?;
                        Ok(RangeIteration {
                            reversed: !inner.reversed,
                            ..inner
                        })
                    }
                    _ => Err(not_a_range(iterable.span)),
                }
            }
            _ => Err(not_a_range(iterable.span)),
        }
    }
    /// Statement-level output keeps the source dialect's spelling.
    fn output_style(&self) -> ir::TextStyle {
        if self.options.dialect == Dialect::Modern {
            ir::TextStyle::Plain
        } else {
            ir::TextStyle::Caie
        }
    }
    fn close_loop(&mut self, end: usize, next: usize) {
        let scope = self.loops.pop().unwrap();
        for at in scope.breaks {
            self.function.code[at].op = Op::Jump(end);
        }
        for at in scope.continues {
            self.function.code[at].op = Op::Jump(next);
        }
    }
    fn emit(&mut self, op: Op) -> usize {
        let i = self.function.code.len();
        self.function.code.push(ir::Instruction {
            op,
            span: self.span,
        });
        i
    }
    fn reg(&mut self, t: Type) -> Reg {
        let r = self.function.registers.len();
        self.function.registers.push(t);
        r
    }
    fn lookup(&self, n: &str) -> Result<(Slot, ir::Local)> {
        if let Some((i, l)) = self
            .function
            .locals
            .iter()
            .enumerate()
            .find(|(_, l)| l.name.eq_identifier(n))
        {
            return Ok((
                Slot {
                    global: false,
                    index: i,
                },
                ir::Local {
                    ty: crate::composites::type_view(&l.ty, self.types),
                    ..l.clone()
                },
            ));
        }
        if let Some((i, l)) = self
            .globals
            .iter()
            .enumerate()
            .find(|(_, l)| l.name.eq_identifier(n))
        {
            return Ok((
                Slot {
                    global: true,
                    index: i,
                },
                ir::Local {
                    ty: crate::composites::type_view(&l.ty, self.types),
                    ..l.clone()
                },
            ));
        }
        Err(Diagnostic::new(
            "E106",
            format!("undeclared identifier `{}`", cyallo_ast::source_name(n)),
            self.span,
        ))
    }
    fn projection_address(&mut self, expression: &Expr, write: bool) -> Result<(Reg, Type)> {
        let known = self
            .globals
            .iter()
            .chain(self.function.locals.iter())
            .map(|local| (key(&local.name), local.ty.clone()))
            .collect();
        let ty = crate::inference::expression_type(expression, &known, self.types)?;
        if let Type::Reference {
            mutable, pointee, ..
        } = ty
        {
            if write && !mutable {
                return Err(Diagnostic::new(
                    "E107",
                    "cannot write through a read-only reference",
                    expression.span,
                ));
            }
            Ok((self.expr(expression)?.0, *pointee))
        } else if let Type::Managed { pointee, .. } = ty {
            if write {
                return Err(Diagnostic::new(
                    "E107",
                    "shared managed storage requires interior mutability",
                    expression.span,
                ));
            }
            let owner = self.expr(expression)?.0;
            let address = self.reg(*pointee.clone());
            self.emit(Op::ManagedAddress(address, owner));
            Ok((address, *pointee))
        } else {
            self.address(expression, write)
        }
    }
    /// A Cell place supports value reads and replacement, but never exposes a
    /// pointee address. Resolve it before ordinary reference-place lowering.
    fn cell_target<'e>(&self, target: &'e Expr) -> Result<Option<(&'e Expr, Type)>> {
        if let ExprKind::Parenthesized(inner) = &target.kind {
            return self.cell_target(inner);
        }
        let ExprKind::Unary(Operator::Dereference, base) = &target.kind else {
            return Ok(None);
        };
        let known = self
            .globals
            .iter()
            .chain(self.function.locals.iter())
            .map(|local| (key(&local.name), local.ty.clone()))
            .collect();
        Ok(
            match crate::inference::expression_type(base, &known, self.types)? {
                Type::Storage(StorageKind::Cell, inner) => Some((base, *inner)),
                _ => None,
            },
        )
    }
    fn address(&mut self, e: &Expr, write: bool) -> Result<(Reg, Type)> {
        cyallo_ast::with_stack(|| {
            self.span = e.span;
            if self.cell_target(e)?.is_some() {
                return Err(Diagnostic::new(
                    "E107",
                    "Cell contents do not expose a reference or support compound assignment; use *cell = value or cell.replace(value)",
                    e.span,
                ));
            }
            match &e.kind {
                ExprKind::Unary(Operator::Dereference, base) => {
                    let (reference, ty) = self.expr(base)?;
                    let (mutable, pointee) = match ty {
                        Type::Reference {
                            mutable, pointee, ..
                        } => (mutable, pointee),
                        Type::Managed { pointee, .. } => {
                            if write {
                                return Err(Diagnostic::new(
                                    "E107",
                                    "shared managed storage requires interior mutability",
                                    e.span,
                                ));
                            }
                            let address = self.reg(*pointee.clone());
                            self.emit(Op::ManagedAddress(address, reference));
                            return Ok((address, *pointee));
                        }
                        _ => {
                            return Err(Diagnostic::new(
                                "E109",
                                "dereference requires a reference or managed owner",
                                e.span,
                            ));
                        }
                    };
                    if write && !mutable {
                        return Err(Diagnostic::new(
                            "E107",
                            "cannot write through a read-only reference",
                            e.span,
                        ));
                    }
                    Ok((reference, *pointee))
                }
                ExprKind::ExtensionField(base, name, _) => {
                    let known = self
                        .globals
                        .iter()
                        .chain(self.function.locals.iter())
                        .map(|l| (key(&l.name), l.ty.clone()))
                        .collect();
                    if let Type::Record(_, fields) =
                        crate::inference::expression_type(base, &known, self.types)?
                        && fields.iter().any(|(n, _)| n.eq_identifier(name))
                    {
                        self.address(
                            &Expr {
                                kind: ExprKind::Field(base.clone(), name.clone()),
                                span: e.span,
                            },
                            write,
                        )
                    } else {
                        Err(Diagnostic::new(
                            "E108",
                            "extension properties are read-only",
                            e.span,
                        ))
                    }
                }
                ExprKind::Field(base, _) | ExprKind::TupleField(base, _) => {
                    let (a, t) = self.projection_address(base, write)?;
                    let (index, ty) = self.field_type(&t, &e.kind, e.span)?;
                    let r = self.reg(ty.clone());
                    self.span = e.span;
                    self.emit(Op::Field(r, a, index));
                    Ok((r, ty))
                }
                ExprKind::Parenthesized(inner) => self.address(inner, write),
                ExprKind::Name(n) => {
                    let (s, l) = self.lookup(n)?;
                    if write && l.constant {
                        return Err(Diagnostic::new("E107", "cannot modify a constant", e.span));
                    }
                    let r = self.reg(l.ty.clone());
                    self.emit(Op::Address(r, s));
                    Ok((r, l.ty))
                }
                ExprKind::Index(base, indices) => {
                    let (a, t) = if !write && !assignable(base) {
                        // Value expressions (including substituted constant arrays)
                        // need a rooted temporary for the ordinary indexing helper.
                        let (value, ty) = self.expr(base)?;
                        let index = self.function.locals.len();
                        self.function.locals.push(ir::Local {
                            display_name: None,
                            name: format!("$index_value${index}").into(),
                            ty: ty.clone(),
                            constant: true,
                            span: base.span,
                        });
                        let address = self.reg(ty.clone());
                        self.emit(Op::Address(
                            address,
                            Slot {
                                global: false,
                                index,
                            },
                        ));
                        self.emit(Op::Store(address, value));
                        (address, ty)
                    } else {
                        self.projection_address(base, write)?
                    };
                    let (dimensions, element) = match t {
                        Type::Array(bounds, element) => (bounds.len(), element),
                        Type::SparseArray(element) | Type::Slice(element) => (1, element),
                        Type::Storage(
                            cyallo_ast::StorageKind::Vec | cyallo_ast::StorageKind::VecDeque,
                            element,
                        ) => (1, element),
                        _ => {
                            return Err(Diagnostic::new(
                                "E108",
                                "indexing requires an array",
                                e.span,
                            ));
                        }
                    };
                    if dimensions != indices.len() {
                        return Err(Diagnostic::new(
                            "E108",
                            "incorrect number of array indices",
                            e.span,
                        ));
                    }
                    let mut rs = vec![];
                    for e in indices {
                        let (r, t) = self.expr(e)?;
                        if t.number_kind().is_none_or(|kind| kind.float()) {
                            return Err(Diagnostic::new(
                                "E109",
                                "index requires an integer",
                                e.span,
                            ));
                        }
                        rs.push(r);
                    }
                    let r = self.reg(*element.clone());
                    self.span = e.span;
                    self.emit(Op::Index(r, a, rs, write));
                    Ok((r, *element))
                }
                _ if !write
                    && matches!(
                        e.kind,
                        ExprKind::Literal(_)
                            | ExprKind::Frozen(..)
                            | ExprKind::ArrayLiteral(_)
                            | ExprKind::Record(..)
                            | ExprKind::Tuple(_)
                    ) =>
                {
                    let (value, ty) = self.expr(e)?;
                    let index = self.function.locals.len();
                    self.function.locals.push(ir::Local {
                        display_name: None,
                        name: format!("$temporary{index}").into(),
                        ty: ty.clone(),
                        constant: true,
                        span: e.span,
                    });
                    let address = self.reg(ty.clone());
                    self.emit(Op::Address(
                        address,
                        Slot {
                            global: false,
                            index,
                        },
                    ));
                    self.emit(Op::Store(address, value));
                    Ok((address, ty))
                }
                _ => Err(Diagnostic::new(
                    "E108",
                    "expected assignable variable or array element",
                    e.span,
                )),
            }
        })
    }
    fn require(&self, want: &Type, got: &Type, span: Span) -> Result<()> {
        if crate::memory::same_regions(want, got)
            || matches!((want,got),(Type::Reference{mutable:w,pointee:a,..},Type::Reference{mutable:g,pointee:b,..}) if (!w || *g) && (crate::memory::same_regions(a,b) || matches!(b.as_ref(),Type::Borrowed(inner) if inner==a)))
            || (self.options.dialect != Dialect::Modern
                && *want == Type::Real
                && *got == Type::Integer)
            || matches!(want, Type::Borrowed(inner) if inner.as_ref() == got)
        {
            Ok(())
        } else {
            let hint = if want.number_kind().is_some_and(|kind| kind.float())
                && got.number_kind().is_some_and(|kind| !kind.float())
            {
                "; integers never convert to floats implicitly: write a float literal such as `2.0` or cast with `as`"
            } else {
                ""
            };
            Err(Diagnostic::new(
                "E109",
                format!("mismatched types: expected `{want}`, found `{got}`{hint}"),
                span,
            ))
        }
    }
    fn expr_expected(&mut self, expression: &Expr, expected: &Type) -> Result<(Reg, Type)> {
        if let Type::Reference {
            mutable, pointee, ..
        } = expected
            && let Type::Slice(element) = pointee.as_ref()
        {
            let expression = contextualize(expression, expected)?;
            let (value, actual) = self.expr(&expression)?;
            if let Type::Reference {
                mutable: writable,
                pointee,
                ..
            } = &actual
                && (!*mutable || *writable)
                && matches!(pointee.as_ref(), Type::Array(_, t) | Type::Storage(StorageKind::Vec, t) if t == element)
            {
                return self.lookup_op(
                    crate::lookup::Operation::Slice(*mutable),
                    expected.clone(),
                    vec![value],
                    expression.span,
                );
            }
            self.require(expected, &actual, expression.span)?;
            return Ok((value, actual));
        }
        self.expr(&contextualize(expression, expected)?)
    }
    fn rc_new_in(
        &mut self,
        value: Reg,
        value_ty: Type,
        allocator: Reg,
        allocator_ty: Type,
        fallible: bool,
        span: Span,
    ) -> Result<(Reg, Type)> {
        if matches!(allocator_ty, Type::Reference { mutable: false, .. }) {
            return Err(Diagnostic::new(
                "E109",
                "allocator requires mutable access",
                span,
            ));
        }
        let implementation = self
            .services
            .allocator(allocator_ty.pointee_or_self())
            .ok_or_else(|| {
                Diagnostic::new(
                    "E109",
                    "allocator type must implement std::mem::Allocator",
                    span,
                )
            })?;
        let allocate = implementation.methods[0];
        let deallocate = implementation.methods[1];
        let index = self.function.locals.len();
        self.function.locals.push(ir::Local {
            name: format!("$allocator{index}").into(),
            display_name: None,
            ty: allocator_ty.clone(),
            constant: false,
            span,
        });
        let address = self.reg(allocator_ty.clone());
        self.emit(Op::Address(
            address,
            Slot {
                global: false,
                index,
            },
        ));
        self.emit(Op::Store(address, allocator));
        let borrowed = matches!(allocator_ty, Type::Reference { .. });
        let receiver_ty = Type::Reference {
            mutable: true,
            lifetime: None,
            pointee: Box::new(allocator_ty.pointee_or_self().clone()),
        };
        let receiver = self.reg(receiver_ty);
        if borrowed {
            self.emit(Op::Load(receiver, address));
        } else {
            self.emit(Op::Move(receiver, address));
        }
        let (layout, _) = self.lookup_op(
            crate::lookup::Operation::LayoutForRc,
            crate::memory::layout(),
            vec![],
            span,
        )?;
        let allocated = self.reg(crate::lookup::result_type(
            crate::memory::allocation(),
            crate::memory::error(),
        ));
        self.emit(Op::Call(Some(allocated), allocate, vec![receiver, layout]));
        let state = self.reg(allocator_ty.clone());
        self.emit(Op::Take(state, address));
        let owner_ty = crate::memory::managed_type(MemoryKind::Rc, Some(&allocator_ty), value_ty);
        let result_ty = if fallible {
            crate::lookup::result_type(owner_ty, crate::memory::error())
        } else {
            owner_ty
        };
        self.lookup_op(
            crate::lookup::Operation::RcNewIn {
                deallocate,
                borrowed,
                fallible,
            },
            result_ty,
            vec![state, allocated, value],
            span,
        )
    }
    fn yield_expr(&mut self, value: &Expr, delegate: bool, span: Span) -> Result<(Reg, Type)> {
        let factory = self
            .function
            .generator
            .clone()
            .ok_or_else(|| cyallo_ast::diagnostic::yield_outside_generator(span))?;
        let (yielded, input, _) = crate::generator::parts(&factory).unwrap();
        if crate::generator::borrowed(yielded) {
            return Err(cyallo_ast::diagnostic::borrowed_generator_yield(span));
        }
        if !delegate {
            let (value, actual) = self.expr_expected(value, yielded)?;
            self.require(yielded, &actual, span)?;
            let result = self.reg(input.clone());
            self.emit(Op::Yield(result, value));
            return Ok((result, input.clone()));
        }
        let (child, child_type) = self.expr(value)?;
        let (child_yield, child_input, returned) = crate::generator::parts(&child_type)
            .ok_or_else(|| cyallo_ast::diagnostic::generator_delegate_required(span))?;
        self.require(yielded, child_yield, span)?;
        self.require(input, child_input, span)?;
        let step_type = crate::generator::step(&child_type).unwrap();
        let step = self.reg(step_type);
        self.emit(Op::Lookup(
            step,
            crate::lookup::Operation::GeneratorResume(false),
            vec![child],
        ));
        let test_pc = self.function.code.len();
        let complete = self.reg(Type::Boolean);
        self.emit(Op::Lookup(
            complete,
            crate::lookup::Operation::GeneratorIsComplete,
            vec![step],
        ));
        let branch = self.function.code.len();
        self.emit(Op::Branch(complete, 0, branch + 1));
        let value = self.reg(yielded.clone());
        self.emit(Op::Lookup(
            value,
            crate::lookup::Operation::GeneratorYielded,
            vec![step],
        ));
        let reply = self.reg(input.clone());
        self.emit(Op::Yield(reply, value));
        self.emit(Op::Lookup(
            step,
            crate::lookup::Operation::GeneratorResume(true),
            vec![child, reply],
        ));
        self.emit(Op::Jump(test_pc));
        let finish = self.function.code.len();
        self.function.code[branch].op = Op::Branch(complete, finish, branch + 1);
        let result = self.reg(returned.clone());
        self.emit(Op::Lookup(
            result,
            crate::lookup::Operation::GeneratorCompleted,
            vec![step],
        ));
        let closed = self.reg(Type::Void);
        self.emit(Op::Lookup(
            closed,
            crate::lookup::Operation::GeneratorClose,
            vec![child],
        ));
        Ok((result, returned.clone()))
    }
    fn expr(&mut self, e: &Expr) -> Result<(Reg, Type)> {
        cyallo_ast::with_stack(|| {
            self.span = e.span;
            match &e.kind {
                ExprKind::Yield(value, delegate) => self.yield_expr(value, *delegate, e.span),
                ExprKind::Invalid(diagnostic) => Err(diagnostic.clone()),
                ExprKind::ManagedNew(kind, allocator, value) => {
                    let allocator = allocator
                        .as_ref()
                        .map(|allocator| self.expr(allocator))
                        .transpose()?;
                    let (value, ty) = self.expr(value)?;
                    if let Some((allocator, allocator_ty)) = allocator {
                        if *kind != MemoryKind::Rc {
                            return Err(Diagnostic::new(
                                "E109",
                                "custom allocation requires @rc",
                                e.span,
                            ));
                        }
                        return self.rc_new_in(value, ty, allocator, allocator_ty, false, e.span);
                    }
                    self.lookup_op(
                        crate::lookup::Operation::ManagedNew(*kind, false),
                        crate::memory::managed_type(*kind, None, ty),
                        vec![value],
                        e.span,
                    )
                }

                ExprKind::Frozen(value, ty) => {
                    let r = self.reg(ty.clone());
                    self.emit(Op::Frozen(r, value.clone()));
                    Ok((r, ty.clone()))
                }
                ExprKind::GenericCall(..) | ExprKind::GenericRecord(..) => Err(Diagnostic::new(
                    "E151",
                    "cannot infer generic arguments; supply explicit type arguments",
                    e.span,
                )),
                ExprKind::IntrinsicCall(id, type_args, args) => {
                    let intrinsic = Intrinsic::from_id(*id).ok_or_else(|| {
                        Diagnostic::new("E112", "invalid intrinsic identity", e.span)
                    })?;
                    let known = self
                        .globals
                        .iter()
                        .chain(self.function.locals.iter())
                        .map(|local| (key(&local.name), local.ty.clone()))
                        .collect();
                    let arguments =
                        intrinsic_arguments(intrinsic, type_args, args, &known, self.types)?;
                    let mut registers = Vec::new();
                    let mut operands = Vec::new();
                    for argument in &arguments {
                        let (register, ty) = self.expr(argument)?;
                        registers.push(register);
                        operands.push(ty);
                    }
                    let call = crate::intrinsic::resolve_at_call(
                        intrinsic, type_args, args, &operands, e.span,
                    )?;
                    match call.lowering {
                        Lowering::AddressLength => {
                            let result = self.reg(Type::Integer);
                            self.emit(Op::Length(result, registers[0]));
                            Ok((result, Type::Integer))
                        }
                        Lowering::RcTryNewIn => self.rc_new_in(
                            registers[0],
                            operands[0].clone(),
                            registers[1],
                            operands[1].clone(),
                            true,
                            e.span,
                        ),
                        Lowering::Print => {
                            self.emit(Op::Output(registers, ir::TextStyle::Plain));
                            Ok((self.reg(Type::Void), Type::Void))
                        }
                        Lowering::Input => {
                            let Type::Reference { pointee, .. } = &operands[0] else {
                                unreachable!("checked input signature");
                            };
                            self.emit(Op::Input(registers[0], *pointee.clone()));
                            Ok((self.reg(Type::Void), Type::Void))
                        }
                        Lowering::Storage(op) => self.lookup_op(op, call.result, registers, e.span),
                        Lowering::Context => {
                            let r = self.reg(call.result.clone());
                            self.emit(Op::Service(
                                r,
                                crate::services::Operation::Context,
                                registers,
                            ));
                            Ok((r, call.result))
                        }
                        Lowering::ServiceKey => {
                            let key = self
                                .services
                                .keys
                                .iter()
                                .position(|k| k.span == e.span.start)
                                .ok_or_else(|| {
                                    Diagnostic::new("E131", "undeclared service key", e.span)
                                })?;
                            let r = self.reg(call.result.clone());
                            self.emit(Op::ServiceKey(r, key));
                            Ok((r, call.result))
                        }
                        Lowering::Utility(id) => {
                            if matches!(
                                self.options.dialect,
                                Dialect::Compatibility | Dialect::Strict
                            ) && !self.options.convenience_builtins
                            {
                                if !crate::builtins::standard_id(id) {
                                    return Err(Diagnostic::new(
                                        "E112",
                                        "utility requires explicit convenience builtins",
                                        e.span,
                                    ));
                                }
                                if id == Intrinsic::Length && operands != [Type::String] {
                                    return Err(Diagnostic::new(
                                        "E112",
                                        "CAIE LENGTH requires STRING",
                                        e.span,
                                    ));
                                }
                            }
                            let r = self.reg(call.result.clone());
                            self.emit(Op::Builtin(r, id, registers));
                            Ok((r, call.result))
                        }
                    }
                }
                ExprKind::TypedCollection(..) => Err(Diagnostic::new(
                    "E151",
                    "unresolved collection initializer",
                    e.span,
                )),
                ExprKind::Cast(base, wanted) if wanted.number_kind().is_some() => {
                    let (value, got) = self.expr(base)?;
                    if got.number_kind().is_none() {
                        return Err(Diagnostic::new(
                            "E109",
                            "numeric cast requires a numeric value",
                            e.span,
                        ));
                    }
                    let r = self.reg(wanted.clone());
                    self.emit(Op::Builtin(
                        r,
                        Intrinsic::cast(wanted.number_kind().unwrap()),
                        vec![value],
                    ));
                    Ok((r, wanted.clone()))
                }
                ExprKind::Cast(base, Type::TraitObject(name)) => {
                    let (value, ty) = self.expr(base)?;
                    if crate::composites::borrowed(&ty) {
                        return Err(Diagnostic::new(
                            "E117",
                            "borrowed value cannot escape into an owned trait object",
                            e.span,
                        ));
                    }
                    let trait_id = self
                        .services
                        .trait_id(name)
                        .ok_or_else(|| Diagnostic::new("E130", "unknown trait", e.span))?;
                    let implementation = self
                        .services
                        .implementations
                        .iter()
                        .position(|i| i.trait_id == trait_id && i.receiver == ty)
                        .ok_or_else(|| {
                            Diagnostic::new("E130", "type does not implement trait", e.span)
                        })?;
                    let t = Type::TraitObject(name.clone());
                    let r = self.reg(t.clone());
                    self.span = e.span;
                    self.emit(Op::TraitBox(r, value, implementation));
                    Ok((r, t))
                }
                ExprKind::Cast(..) => Err(Diagnostic::new(
                    "E130",
                    "only implemented trait-object conversions are supported",
                    e.span,
                )),
                ExprKind::ExtensionField(base, name, getter) => {
                    let known = self
                        .globals
                        .iter()
                        .chain(self.function.locals.iter())
                        .map(|l| (key(&l.name), l.ty.clone()))
                        .collect();
                    if let Type::Record(_, fields) =
                        crate::inference::expression_type(base, &known, self.types)?
                        && fields.iter().any(|(n, _)| n.eq_identifier(name))
                    {
                        return self.expr(&Expr {
                            kind: ExprKind::Field(base.clone(), name.clone()),
                            span: e.span,
                        });
                    }
                    let call = Expr {
                        kind: ExprKind::Call(getter.clone(), vec![*base.clone()]),
                        span: e.span,
                    };
                    self.expr(&call)
                }

                ExprKind::Closure(..) => unreachable!("closures have been converted"),
                ExprKind::ClosureValue(name, captures, ty) => {
                    let id = self.signatures[&key(name)].id;
                    let mut rs = vec![];
                    for capture in captures {
                        rs.push(self.expr(capture)?.0);
                    }
                    let r = self.reg(ty.clone());
                    self.span = e.span;
                    self.emit(Op::FunctionValue(r, id, rs));
                    Ok((r, ty.clone()))
                }
                ExprKind::Tuple(es) => {
                    let mut rs = vec![];
                    let mut ts = vec![];
                    for e in es {
                        let (r, t) = self.expr(e)?;
                        rs.push(r);
                        ts.push(t);
                    }
                    let ty = Type::Tuple(ts);
                    let r = self.reg(ty.clone());
                    self.span = e.span;
                    self.emit(Op::Aggregate(r, rs));
                    Ok((r, ty))
                }
                ExprKind::Record(name, fields) => {
                    let ty =
                        self.types.get(&key(name)).cloned().ok_or_else(|| {
                            Diagnostic::new("E116", "unknown record type", e.span)
                        })?;
                    let Type::Record(_, declared) = &ty else {
                        return Err(Diagnostic::new("E116", "expected record type", e.span));
                    };
                    let mut rs = vec![None; declared.len()];
                    for (name, value) in fields {
                        let index = declared
                            .iter()
                            .position(|(n, _)| n.eq_identifier(name))
                            .ok_or_else(|| {
                                Diagnostic::new(
                                    "E116",
                                    format!("unknown field `{}`", cyallo_ast::source_name(name)),
                                    value.span,
                                )
                            })?;
                        if rs[index].is_some() {
                            return Err(Diagnostic::new(
                                "E101",
                                "duplicate record field",
                                value.span,
                            ));
                        }
                        let (r, t) = self.expr_expected(value, &declared[index].1)?;
                        self.require(&declared[index].1, &t, value.span)?;
                        rs[index] = Some(r);
                    }
                    let rs = rs.into_iter().collect::<Option<Vec<_>>>().ok_or_else(|| {
                        Diagnostic::new(
                            "E116",
                            "record literal must initialize every field",
                            e.span,
                        )
                    })?;
                    let r = self.reg(ty.clone());
                    self.span = e.span;
                    self.emit(Op::Aggregate(r, rs));
                    Ok((r, ty))
                }
                ExprKind::Field(base, _) | ExprKind::TupleField(base, _) => {
                    if assignable(base) {
                        let (a, t) = self.address(e, false)?;
                        let r = self.reg(t.clone());
                        self.emit(if self.moves.contains(&(e.span.start, e.span.end)) {
                            Op::Take(r, a)
                        } else {
                            Op::Load(r, a)
                        });
                        Ok((r, t))
                    } else {
                        let (a, t) = self.expr(base)?;
                        let (i, ty) = self.field_type(&t, &e.kind, e.span)?;
                        let r = self.reg(ty.clone());
                        self.span = e.span;
                        self.emit(Op::Extract(r, a, i));
                        Ok((r, ty))
                    }
                }
                ExprKind::Invoke(callee, args) => self.invoke(callee, args, e.span, false),
                ExprKind::Name(n)
                    if self.options.dialect == Dialect::Modern
                        && self.lookup(n).is_err()
                        && self.signatures.contains_key(&key(n)) =>
                {
                    let sig = &self.signatures[&key(n)];
                    let ty = self.types[&key(n)].clone();
                    let id = sig.id;
                    let r = self.reg(ty.clone());
                    self.emit(Op::FunctionValue(r, id, vec![]));
                    Ok((r, ty))
                }
                ExprKind::Parenthesized(inner) => self.expr(inner),
                ExprKind::ArrayLiteral(values) => {
                    let mut regs = vec![];
                    let mut element = None;
                    for value in values {
                        let (r, t) = self.expr(value)?;
                        if let Some(want) = &element {
                            if !crate::memory::same_regions(want, &t) {
                                return Err(Diagnostic::new(
                                    "E109",
                                    "array literal elements must have the same type",
                                    value.span,
                                ));
                            }
                        } else {
                            element = Some(t);
                        }
                        regs.push(r);
                    }
                    let element = element.ok_or_else(|| {
                        Diagnostic::new(
                            "E114",
                            "cannot infer an empty array; initialize an indexed element first",
                            e.span,
                        )
                    })?;
                    let t = Type::Array(vec![(0, values.len() as i64 - 1)], Box::new(element));
                    let r = self.reg(t.clone());
                    self.span = e.span;
                    self.emit(Op::ArrayLiteral(r, regs));
                    Ok((r, t))
                }
                ExprKind::Literal(l) => {
                    let t = literal_type(l);
                    let r = self.reg(t.clone());
                    if let Literal::String(text) = l {
                        self.emit(Op::LiteralString(r, std::sync::Arc::from(text.as_ref())));
                    } else {
                        self.emit(Op::Literal(
                            r,
                            match l {
                                Literal::Number(n) => Literal::number(*n),
                                _ => l.clone(),
                            },
                        ));
                    }
                    Ok((r, t))
                }
                ExprKind::Name(_) | ExprKind::Index(..) => {
                    let (a, t) = self.address(e, false)?;
                    let r = self.reg(t.clone());
                    self.emit(if self.moves.contains(&(e.span.start, e.span.end)) {
                        Op::Take(r, a)
                    } else {
                        Op::Load(r, a)
                    });
                    Ok((r, t))
                }
                ExprKind::Unary(Operator::Borrow | Operator::BorrowMut, a) => {
                    let mutable = matches!(e.kind, ExprKind::Unary(Operator::BorrowMut, _));
                    let (address, pointee) = self.address(a, mutable)?;
                    let ty = Type::Reference {
                        mutable,
                        lifetime: None,
                        pointee: Box::new(pointee),
                    };
                    let r = self.reg(ty.clone());
                    self.emit(Op::Move(r, address));
                    Ok((r, ty))
                }
                ExprKind::Unary(Operator::Dereference, _) => {
                    if let Some((base, ty)) = self.cell_target(e)? {
                        if !crate::ownership::copy(&ty)
                            || matches!(ty, Type::Reference { mutable: true, .. })
                        {
                            return Err(Diagnostic::new(
                                "E109",
                                "Cell dereference reads require Copy contents; use cell.replace(value) to transfer ownership",
                                e.span,
                            ));
                        }
                        let (address, _) = self.address(base, false)?;
                        return self.lookup_op(
                            crate::lookup::Operation::CellRead,
                            ty,
                            vec![address],
                            e.span,
                        );
                    }
                    let (address, ty) = self.address(e, false)?;
                    let r = self.reg(ty.clone());
                    self.emit(Op::Load(r, address));
                    Ok((r, ty))
                }
                ExprKind::Unary(op, a) => {
                    let (a, t) = self.expr(a)?;
                    if !(if *op == Operator::Not {
                        t == Type::Boolean
                    } else {
                        numeric(&t)
                    }) {
                        return Err(Diagnostic::new("E105", "invalid unary operand", e.span));
                    }
                    let r = self.reg(t.clone());
                    self.span = e.span;
                    self.emit(Op::Unary(r, *op, a));
                    Ok((r, t))
                }
                ExprKind::Binary(op, a, b) => {
                    let known = self
                        .globals
                        .iter()
                        .chain(self.function.locals.iter())
                        .map(|l| (key(&l.name), l.ty.clone()))
                        .collect();
                    let ta = crate::inference::expression_type(a, &known, self.types).ok();
                    let tb = crate::inference::expression_type(b, &known, self.types).ok();
                    let context = ta
                        .as_ref()
                        .zip(tb.as_ref())
                        .filter(|(a, b)| numeric(a) && numeric(b))
                        .map(|(ta, tb)| numeric_context(a, ta, b, tb));
                    let a = context
                        .as_ref()
                        .map(|ty| contextualize(a, ty))
                        .transpose()?
                        .unwrap_or_else(|| *a.clone());
                    let b = context
                        .as_ref()
                        .map(|ty| contextualize(b, ty))
                        .transpose()?
                        .unwrap_or_else(|| *b.clone());
                    let a = &a;
                    let b = &b;
                    let (a, ta) = self.expr(a)?;
                    let r = self.reg(ta.clone());
                    if *op == Operator::And || *op == Operator::Or {
                        self.require(&Type::Boolean, &ta, e.span)?;
                        self.emit(Op::Move(r, a));
                        let branch = self.emit(Op::Statement);
                        let rhs = self.function.code.len();
                        let (b, tb) = self.expr(b)?;
                        self.require(&Type::Boolean, &tb, e.span)?;
                        self.emit(Op::Move(r, b));
                        let end = self.function.code.len();
                        self.function.code[branch].op = if *op == Operator::And {
                            Op::Branch(a, rhs, end)
                        } else {
                            Op::Branch(a, end, rhs)
                        };
                        return Ok((r, Type::Boolean));
                    }
                    let (b, tb) = self.expr(b)?;
                    if matches!(op, Operator::Range | Operator::RangeInclusive) {
                        self.require(&ta, &tb, e.span)?;
                        if ta.number_kind().is_none_or(|kind| kind.float()) {
                            return Err(Diagnostic::new(
                                "E109",
                                "range bounds require integers",
                                e.span,
                            ));
                        }
                        return self.lookup_op(
                            crate::lookup::Operation::Range(*op == Operator::RangeInclusive),
                            Type::Storage(cyallo_ast::StorageKind::Range, Box::new(ta)),
                            vec![a, b],
                            e.span,
                        );
                    }
                    if self.options.dialect == Dialect::Modern
                        && numeric(&ta)
                        && numeric(&tb)
                        && ta != tb
                    {
                        return Err(Diagnostic::new(
                            "E109",
                            "numeric operands require identical types; use a checked cast",
                            e.span,
                        ));
                    }
                    let t = binary_type(*op, &ta, &tb, e.span)?;
                    self.function.registers[r] = t.clone();
                    self.span = e.span;
                    self.emit(Op::Binary(r, specialized_operator(*op, &t), a, b));
                    Ok((r, t))
                }
                ExprKind::Call(n, args) => {
                    if self.options.dialect == Dialect::Modern && self.lookup(n).is_ok() {
                        let callee = Expr {
                            kind: ExprKind::Name(n.clone()),
                            span: e.span,
                        };
                        return self.invoke(&callee, args, e.span, false);
                    }
                    if let Some(sig) = self.signatures.get(&key(n)).cloned() {
                        if sig.result == Type::Void {
                            return Err(Diagnostic::new(
                                "E110",
                                "procedure cannot be used as an expression",
                                e.span,
                            ));
                        }
                        let rs = self.arguments(&sig, args)?;
                        let r = self.reg(sig.result.clone());
                        self.span = e.span;
                        self.emit(Op::Call(Some(r), sig.id, rs));
                        Ok((r, sig.result))
                    } else {
                        Err(Diagnostic::new(
                            "E106",
                            format!("unresolved callable `{}`", cyallo_ast::source_name(n)),
                            e.span,
                        ))
                    }
                }
            }
        })
    }
    fn arguments(&mut self, sig: &Signature, args: &[Expr]) -> Result<Vec<Reg>> {
        if args.len() != sig.params.len() {
            return Err(Diagnostic::new(
                "E110",
                "incorrect argument count",
                self.span,
            ));
        }
        let mut regs = vec![];
        for (a, p) in args.iter().zip(&sig.params) {
            let (r, t) = if p.by_ref {
                self.address(a, true)?
            } else {
                self.expr_expected(a, &p.ty)?
            };
            if p.by_ref && p.ty != t {
                return Err(Diagnostic::new(
                    "E109",
                    "BYREF requires identical types",
                    a.span,
                ));
            }
            self.require(&p.ty, &t, a.span)?;
            regs.push(r);
        }
        Ok(regs)
    }
    fn drop_slots(&mut self, slots: Vec<(Slot, Type)>) {
        for (slot, ty) in slots.into_iter().rev() {
            let address = self.reg(ty);
            self.emit(Op::Address(address, slot));
            self.emit(Op::Drop(address));
        }
    }
    fn block(&mut self, body: &[Stmt]) -> Result<()> {
        cyallo_ast::with_stack(|| {
            self.block_depth += 1;
            self.drop_scopes.push(vec![]);
            for statement in body {
                self.statement(statement)?;
            }
            let slots = self.drop_scopes.pop().expect("active block");
            self.drop_slots(slots);
            self.block_depth -= 1;
            Ok(())
        })
    }
    fn condition(&mut self, e: &Expr) -> Result<Reg> {
        let (r, t) = self.expr(e)?;
        self.require(&Type::Boolean, &t, e.span)?;
        Ok(r)
    }
    fn statement(&mut self, s: &Stmt) -> Result<()> {
        self.span = s.span;
        self.emit(Op::Statement);
        match &s.kind {
            StmtKind::Binding(n, _, _, _, e) => {
                let (slot, local) = self.lookup(n)?;
                let (v, t) = self.expr_expected(e, &local.ty)?;
                self.require(&local.ty, &t, e.span)?;
                if self.options.dialect == Dialect::Modern
                    && !(self.entry && self.block_depth == 1)
                    && crate::ownership::needs_drop(&local.ty)
                {
                    self.drop_scopes
                        .last_mut()
                        .expect("active block")
                        .push((slot, local.ty.clone()));
                }
                let addr = self.reg(local.ty);
                self.emit(Op::Address(addr, slot));
                self.emit(Op::Store(addr, v));
            }
            StmtKind::Compound(target, op, value) => {
                let (addr, ty) = self.address(target, true)?;
                let old = self.reg(ty.clone());
                self.emit(Op::Load(old, addr));
                let (rhs, rty) = self.expr(&contextualize(value, &ty)?)?;
                let result = binary_type(*op, &ty, &rty, s.span)?;
                self.require(&ty, &result, s.span)?;
                let operator = specialized_operator(*op, &result);
                let r = self.reg(result);
                self.span = s.span;
                self.emit(Op::Binary(r, operator, old, rhs));
                self.emit(Op::Store(addr, r));
            }
            StmtKind::Evaluate(e) => {
                let result = if let ExprKind::Invoke(callee, args) = &e.kind {
                    Some(self.invoke(callee, args, e.span, true)?)
                } else if let ExprKind::Call(n, args) = &e.kind
                    && self.options.dialect == Dialect::Modern
                    && self.lookup(n).is_ok()
                {
                    Some(self.invoke(
                        &Expr {
                            kind: ExprKind::Name(n.clone()),
                            span: e.span,
                        },
                        args,
                        e.span,
                        true,
                    )?)
                } else if let ExprKind::Call(n, args) = &e.kind
                    && let Some(sig) = self.signatures.get(&key(n)).cloned()
                    && sig.result == Type::Void
                {
                    let args = self.arguments(&sig, args)?;
                    self.emit(Op::Call(None, sig.id, args));
                    None
                } else {
                    Some(self.expr(e)?)
                };
                if self.options.dialect == Dialect::Modern
                    && let Some((value, ty)) = result
                    && temporary(e)
                    && crate::ownership::needs_drop(&ty)
                {
                    self.lookup_op(
                        crate::lookup::Operation::DropValue,
                        Type::Void,
                        vec![value],
                        e.span,
                    )?;
                }
            }
            StmtKind::ReturnVoid => {
                if self.function.result != Type::Void {
                    return Err(Diagnostic::new(
                        "E111",
                        "function return requires a value",
                        s.span,
                    ));
                }
                self.emit(Op::Return(None));
            }
            StmtKind::Break | StmtKind::Continue => {
                if self.loops.is_empty() {
                    return Err(Diagnostic::new(
                        "E115",
                        "break/continue requires a loop",
                        s.span,
                    ));
                }
                let depth = self.loops.last().unwrap().drop_depth;
                let slots = self.drop_scopes[depth..]
                    .iter()
                    .flat_map(|scope| scope.iter().cloned())
                    .collect();
                self.drop_slots(slots);
                let at = self.emit(Op::Jump(usize::MAX));
                let scope = self.loops.last_mut().unwrap();
                if matches!(s.kind, StmtKind::Break) {
                    scope.breaks.push(at);
                } else {
                    scope.continues.push(at);
                }
            }
            StmtKind::Declare(..) => {}
            StmtKind::Constant(n, e) => {
                let (v, t) = self.expr(e)?;
                let (slot, l) = self.lookup(n)?;
                self.require(&l.ty, &t, e.span)?;
                let a = self.reg(l.ty);
                self.emit(Op::Address(a, slot));
                self.emit(Op::Store(a, v));
            }
            StmtKind::Assign(a, e) => {
                let known = self
                    .globals
                    .iter()
                    .chain(self.function.locals.iter())
                    .map(|local| (key(&local.name), local.ty.clone()))
                    .collect();
                let expected = crate::inference::expression_type(a, &known, self.types)?;
                let (v, t) = self.expr_expected(e, &expected)?;
                self.check_borrow_store(a, &t)?;
                if let Some((base, contents)) = self.cell_target(a)? {
                    self.require(&contents, &t, e.span)?;
                    let (address, _) = self.address(base, false)?;
                    self.lookup_op(
                        crate::lookup::Operation::CellWrite,
                        Type::Void,
                        vec![address, v],
                        a.span,
                    )?;
                    return Ok(());
                }
                let (a, ta) = self.address(a, true)?;
                self.require(&ta, &t, e.span)?;
                self.emit(Op::Store(a, v));
            }
            StmtKind::Output(es) => {
                if es.len() == 1 {
                    if let ExprKind::Invoke(callee, args) = &es[0].kind {
                        let (r, t) = self.invoke(callee, args, es[0].span, true)?;
                        if t != Type::Void {
                            if !matches!(
                                t,
                                Type::Integer
                                    | Type::Real
                                    | Type::Numeric(_)
                                    | Type::String
                                    | Type::Char
                                    | Type::Boolean
                            ) {
                                return Err(Diagnostic::new(
                                    "E109",
                                    "OUTPUT requires scalar values",
                                    s.span,
                                ));
                            }
                            self.emit(Op::Output(vec![r], self.output_style()));
                        }
                        return Ok(());
                    }
                    if let ExprKind::Call(n, args) = &es[0].kind
                        && self.options.dialect == Dialect::Modern
                        && self.lookup(n).is_ok()
                    {
                        let callee = Expr {
                            kind: ExprKind::Name(n.clone()),
                            span: es[0].span,
                        };
                        let (r, t) = self.invoke(&callee, args, es[0].span, true)?;
                        if t != Type::Void {
                            self.emit(Op::Output(vec![r], self.output_style()));
                        }
                        return Ok(());
                    }
                }
                if es.len() == 1
                    && let ExprKind::Call(n, args) = &es[0].kind
                    && let Some(sig) = self.signatures.get(&key(n)).cloned()
                    && sig.result == Type::Void
                {
                    let args = self.arguments(&sig, args)?;
                    self.emit(Op::Call(None, sig.id, args));
                    return Ok(());
                }
                let mut rs = vec![];
                for e in es {
                    let (r, t) = self.expr(e)?;
                    if t == Type::Void && es.len() == 1 {
                        return Ok(());
                    }
                    if !matches!(
                        t,
                        Type::Integer
                            | Type::Real
                            | Type::Numeric(_)
                            | Type::Boolean
                            | Type::Char
                            | Type::String
                    ) {
                        return Err(Diagnostic::new(
                            "E109",
                            "OUTPUT requires scalar values",
                            e.span,
                        ));
                    }
                    rs.push(r);
                }
                self.emit(Op::Output(rs, self.output_style()));
            }
            StmtKind::Input(e) => {
                let (a, t) = self.address(e, true)?;
                if !matches!(
                    t,
                    Type::Integer
                        | Type::Real
                        | Type::Numeric(_)
                        | Type::Boolean
                        | Type::Char
                        | Type::String
                ) {
                    return Err(Diagnostic::new(
                        "E109",
                        "INPUT requires scalar storage",
                        e.span,
                    ));
                }
                self.emit(Op::Input(a, t));
            }
            StmtKind::Call(n, args) => {
                let sig = self.signatures.get(&key(n)).cloned().ok_or_else(|| {
                    Diagnostic::new(
                        "E110",
                        format!("unknown procedure `{}`", cyallo_ast::source_name(n)),
                        s.span,
                    )
                })?;
                if sig.kind != RoutineKind::Procedure {
                    return Err(Diagnostic::new("E110", "CALL requires a procedure", s.span));
                }
                let args = self.arguments(&sig, args)?;
                self.emit(Op::Call(None, sig.id, args));
            }
            StmtKind::Return(e) => {
                let (r, t) = self.expr_expected(e, &self.function.result.clone())?;
                if crate::composites::borrowed(&t) {
                    return Err(Diagnostic::new(
                        "E117",
                        "borrowed closure cannot escape through return",
                        e.span,
                    ));
                }
                if self.function.result == Type::Void {
                    return Err(Diagnostic::new(
                        "E111",
                        "RETURN value requires a function",
                        s.span,
                    ));
                }
                self.require(&self.function.result, &t, e.span)?;
                self.emit(Op::Return(Some(r)));
            }
            StmtKind::If(e, a, b) => {
                let r = self.condition(e)?;
                let branch = self.emit(Op::Statement);
                let yes = self.function.code.len();
                self.block(a)?;
                let skip = self.emit(Op::Statement);
                let no = self.function.code.len();
                self.block(b)?;
                let end = self.function.code.len();
                self.function.code[branch].op = Op::Branch(r, yes, no);
                self.function.code[skip].op = Op::Jump(end);
            }
            StmtKind::While(e, b) => {
                let start = self.function.code.len() - 1;
                let r = self.condition(e)?;
                let branch = self.emit(Op::Statement);
                let yes = self.function.code.len();
                self.loops.push(LoopScope {
                    drop_depth: self.drop_scopes.len(),
                    breaks: vec![],
                    continues: vec![],
                });
                self.block(b)?;
                self.emit(Op::Jump(start));
                let end = self.function.code.len();
                self.function.code[branch].op = Op::Branch(r, yes, end);
                self.close_loop(end, start);
            }
            StmtKind::Repeat(b, e) => {
                let start = self.function.code.len() - 1;
                self.loops.push(LoopScope {
                    drop_depth: self.drop_scopes.len(),
                    breaks: vec![],
                    continues: vec![],
                });
                self.block(b)?;
                let condition_start = self.function.code.len();
                let r = self.condition(e)?;
                let branch = self.emit(Op::Statement);
                let end = self.function.code.len();
                self.function.code[branch].op = Op::Branch(r, end, start);
                self.close_loop(end, condition_start);
            }
            StmtKind::For(n, a, z, step, b) => {
                let target = Expr {
                    kind: ExprKind::Name(n.clone()),
                    span: s.span,
                };
                let (addr, t) = self.address(&target, true)?;
                self.require(&Type::Integer, &t, s.span)?;
                let (first, t) = self.expr(a)?;
                self.require(&Type::Integer, &t, a.span)?;
                let (last, t) = self.expr(z)?;
                self.require(&Type::Integer, &t, z.span)?;
                let default = Expr {
                    kind: ExprKind::Literal(Literal::Integer(1)),
                    span: s.span,
                };
                let (step, t) = self.expr(step.as_ref().unwrap_or(&default))?;
                self.require(&Type::Integer, &t, s.span)?;
                self.emit(Op::CheckStep(step));
                self.emit(Op::Store(addr, first));
                let start = self.function.code.len();
                self.span = s.span;
                self.emit(Op::Statement);
                let cur = self.reg(Type::Integer);
                self.emit(Op::Load(cur, addr));
                let zero = self.reg(Type::Integer);
                self.emit(Op::Literal(zero, Literal::Integer(0)));
                let positive = self.reg(Type::Boolean);
                self.emit(Op::Binary(positive, Operator::Greater, step, zero));
                let select = self.emit(Op::Statement);
                let asc = self.function.code.len();
                let cond = self.reg(Type::Boolean);
                self.emit(Op::Binary(cond, Operator::LessEqual, cur, last));
                let join = self.emit(Op::Statement);
                let desc = self.function.code.len();
                self.emit(Op::Binary(cond, Operator::GreaterEqual, cur, last));
                let test = self.function.code.len();
                let branch = self.emit(Op::Statement);
                let yes = self.function.code.len();
                self.loops.push(LoopScope {
                    drop_depth: self.drop_scopes.len(),
                    breaks: vec![],
                    continues: vec![],
                });
                self.block(b)?;
                let increment = self.function.code.len();
                self.span = s.span;
                let v = self.reg(Type::Integer);
                self.emit(Op::Load(v, addr));
                let finished = self.reg(Type::Boolean);
                self.emit(Op::Binary(finished, Operator::Equal, v, last));
                let skip_increment = self.emit(Op::Statement);
                let advance = self.function.code.len();
                let next = self.reg(Type::Integer);
                self.emit(Op::Binary(next, Operator::Add, v, step));
                self.emit(Op::Store(addr, next));
                self.emit(Op::Jump(start));
                let end = self.function.code.len();
                self.function.code[select].op = Op::Branch(positive, asc, desc);
                self.function.code[join].op = Op::Jump(test);
                self.function.code[branch].op = Op::Branch(cond, yes, end);
                self.function.code[skip_increment].op = Op::Branch(finished, end, advance);
                self.close_loop(end, increment);
            }
            StmtKind::ForIn(n, iterable, b) => {
                let range = self.range_iteration(iterable)?;
                let (first, t) = self.expr(range.start)?;
                let (last, end_type) = self.expr(range.end)?;
                self.require(&t, &end_type, range.end.span)?;
                let Some(kind) = t.number_kind().filter(|kind| !kind.float()) else {
                    return Err(Diagnostic::new(
                        "E109",
                        "for-in ranges require integer bounds",
                        iterable.span,
                    ));
                };
                let target = Expr {
                    kind: ExprKind::Name(n.clone()),
                    span: s.span,
                };
                let (addr, variable) = self.address(&target, true)?;
                self.require(&t, &variable, s.span)?;
                let one = self.reg(t.clone());
                let unit = cyallo_ast::Number::integer(kind, 1).map_err(|error| {
                    cyallo_ast::diagnostic::invalid_numeric_literal(error, s.span)
                })?;
                self.emit(Op::Literal(one, Literal::number(unit)));
                // An empty range skips the body. An exclusive bound becomes
                // inclusive by stepping back once, which cannot overflow
                // because the range is non-empty.
                let empty = self.reg(Type::Boolean);
                let empty_test = if range.inclusive {
                    Operator::Greater
                } else {
                    Operator::GreaterEqual
                };
                self.emit(Op::Binary(empty, empty_test, first, last));
                let skip = self.emit(Op::Statement);
                let bound = if range.inclusive {
                    last
                } else {
                    let bound = self.reg(t.clone());
                    self.emit(Op::Binary(bound, Operator::Subtract, last, one));
                    bound
                };
                let (from, to, advance_by) = if range.reversed {
                    (bound, first, Operator::Subtract)
                } else {
                    (first, bound, Operator::Add)
                };
                self.emit(Op::Store(addr, from));
                let start = self.function.code.len();
                self.loops.push(LoopScope {
                    drop_depth: self.drop_scopes.len(),
                    breaks: vec![],
                    continues: vec![],
                });
                self.block(b)?;
                // The counter stops on equality, so it never steps past `to`.
                let increment = self.function.code.len();
                self.span = s.span;
                let current = self.reg(t.clone());
                self.emit(Op::Load(current, addr));
                let finished = self.reg(Type::Boolean);
                self.emit(Op::Binary(finished, Operator::Equal, current, to));
                let test = self.emit(Op::Statement);
                let advance = self.function.code.len();
                let next = self.reg(t);
                self.emit(Op::Binary(next, advance_by, current, one));
                self.emit(Op::Store(addr, next));
                self.emit(Op::Jump(start));
                let end = self.function.code.len();
                self.function.code[skip].op = Op::Branch(empty, end, skip + 1);
                self.function.code[test].op = Op::Branch(finished, end, advance);
                self.close_loop(end, increment);
            }
            StmtKind::Case(e, arms, fallback) => {
                let (value, t) = self.expr(e)?;
                binary_type(Operator::Equal, &t, &t, e.span)?;
                let mut exits = vec![];
                for (label, body) in arms {
                    let (r, lt) = self.expr(&label.lower)?;
                    if lt != t {
                        return Err(Diagnostic::new(
                            "E109",
                            "CASE label type differs",
                            label.span(),
                        ));
                    }
                    let cmp = self.reg(Type::Boolean);
                    if let Some(upper) = &label.upper {
                        let (u, ut) = self.expr(upper)?;
                        if ut != t {
                            return Err(Diagnostic::new(
                                "E109",
                                "CASE range types differ",
                                upper.span,
                            ));
                        }
                        binary_type(Operator::LessEqual, &t, &t, label.span())?;
                        let lo = self.reg(Type::Boolean);
                        let hi = self.reg(Type::Boolean);
                        self.emit(Op::Binary(lo, Operator::GreaterEqual, value, r));
                        self.emit(Op::Binary(hi, Operator::LessEqual, value, u));
                        // Both comparisons are already evaluated and cannot have side effects.
                        self.emit(Op::Move(cmp, hi));
                        let test = self.emit(Op::Statement);
                        let next = self.function.code.len();
                        let no = self.reg(Type::Boolean);
                        self.emit(Op::Literal(no, Literal::Boolean(false)));
                        self.emit(Op::Move(cmp, no));
                        let end = self.function.code.len();
                        self.function.code[test].op = Op::Branch(lo, end, next);
                    } else {
                        self.emit(Op::Binary(cmp, Operator::Equal, value, r));
                    }
                    let branch = self.emit(Op::Statement);
                    let yes = self.function.code.len();
                    self.block(body)?;
                    exits.push(self.emit(Op::Statement));
                    let no = self.function.code.len();
                    self.function.code[branch].op = Op::Branch(cmp, yes, no);
                }
                self.block(fallback)?;
                let end = self.function.code.len();
                for i in exits {
                    self.function.code[i].op = Op::Jump(end);
                }
            }
        }
        Ok(())
    }
}
pub fn builtin_type(n: Intrinsic, types: &[Type], span: Span) -> Result<Type> {
    crate::builtins::result_type(n, types).ok_or_else(|| {
        Diagnostic::new(
            "E112",
            format!(
                "`{n}` does not accept ({})",
                types
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            span,
        )
    })
}

/// The compatibility adapter declares builtin identities before type analysis.
fn resolve_compatibility_calls(ast: &mut Ast) -> Result<()> {
    let names: std::collections::HashSet<_> =
        ast.functions.iter().map(|f| f.name.clone()).collect();
    let mut resolve = |e: &mut Expr| {
        if let ExprKind::Call(name, args) = &mut e.kind
            && !names.contains(name)
            && let Some(id) = crate::builtins::canonical(name)
        {
            e.kind = ExprKind::IntrinsicCall(id.id(), vec![], std::mem::take(args));
        }
        Ok(())
    };
    cyallo_ast::visit::visit_block(&mut ast.body, &mut resolve, &mut |_, _| Ok(()))?;
    for f in &mut ast.functions {
        cyallo_ast::visit::visit_block(&mut f.body, &mut resolve, &mut |_, _| Ok(()))?;
    }
    Ok(())
}
