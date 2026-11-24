//! Composite type resolution and closure conversion. Captures are ordinary
//! hidden parameters, so all execution tiers share the existing call ABI.
use crate::{
    analysis::{self, CompileOptions},
    ir::Local,
};
use cyallo_ast::IdentifierEq as _;
use cyallo_ast::*;
use std::collections::{HashMap, HashSet};
type Result<T> = std::result::Result<T, Diagnostic>;
fn key(n: &str) -> String {
    n.to_owned()
}
type Callable<'a> = (RoutineKind, &'a [(Type, bool)], &'a Type);
pub fn callable(t: &Type) -> Option<Callable<'_>> {
    match t {
        Type::Function(k, p, r) => Some((*k, p, r)),
        Type::Borrowed(t) | Type::Reference { pointee: t, .. } => callable(t),
        _ => None,
    }
}
pub fn borrowed(t: &Type) -> bool {
    match t {
        Type::Enum(contract) => contract
            .variants
            .iter()
            .filter_map(|v| v.payload.as_ref())
            .any(borrowed),
        Type::Borrowed(_) => true,
        Type::Tuple(ts) => ts.iter().any(borrowed),
        Type::Record(_, fs) | Type::AnonymousRecord(fs) | Type::Object(fs) => {
            fs.iter().any(|(_, t)| borrowed(t))
        }
        Type::Array(_, t) | Type::SparseArray(t) | Type::Storage(_, t) => borrowed(t),
        Type::Opaque(_, ts, _) => ts.iter().any(borrowed),
        _ => false,
    }
}
pub fn function_type(f: &Function) -> Type {
    Type::Function(
        f.kind,
        f.params.iter().map(|p| (p.ty.clone(), p.by_ref)).collect(),
        Box::new(crate::generator::factory_type(f)),
    )
}
fn bound_names(f: &Function) -> HashSet<String> {
    fn block(body: &[Stmt], names: &mut HashSet<String>) {
        for s in body {
            match &s.kind {
                StmtKind::Binding(n, ..) | StmtKind::Declare(n, _) | StmtKind::Constant(n, _) => {
                    names.insert(key(n));
                }
                StmtKind::If(_, a, b) => {
                    block(a, names);
                    block(b, names);
                }
                StmtKind::While(_, b)
                | StmtKind::Repeat(b, _)
                | StmtKind::For(_, _, _, _, b)
                | StmtKind::ForIn(_, _, b) => block(b, names),
                StmtKind::Case(_, arms, b) => {
                    for (_, b) in arms {
                        block(b, names);
                    }
                    block(b, names);
                }
                _ => {}
            }
        }
    }
    let mut names = f.params.iter().map(|p| key(&p.name)).collect();
    block(&f.body, &mut names);
    names
}
/// Expand nominal backreferences at the boundary where a type's members or
/// layout are needed. A record's fields remain finite and may contain backedges.
pub fn type_view(ty: &Type, definitions: &HashMap<String, Type>) -> Type {
    match ty {
        Type::EnumRef(_, name) | Type::RecordRef(name) => definitions
            .get(name.as_ref())
            .cloned()
            .expect("resolved record identity"),
        Type::Managed {
            kind,
            locality,
            lifetime,
            pointee,
        } => Type::Managed {
            kind: *kind,
            locality: *locality,
            lifetime: lifetime.clone(),
            pointee: Box::new(type_view(pointee, definitions)),
        },
        Type::Reference {
            mutable,
            lifetime,
            pointee,
        } => Type::Reference {
            mutable: *mutable,
            lifetime: lifetime.clone(),
            pointee: Box::new(type_view(pointee, definitions)),
        },
        Type::Storage(kind, inner) => Type::Storage(*kind, Box::new(type_view(inner, definitions))),
        Type::Opaque(name, arguments, traits) => Type::Opaque(
            name.clone(),
            arguments
                .iter()
                .map(|t| type_view(t, definitions))
                .collect(),
            *traits,
        ),
        Type::Array(bounds, inner) => {
            Type::Array(bounds.clone(), Box::new(type_view(inner, definitions)))
        }
        Type::SparseArray(inner) => Type::SparseArray(Box::new(type_view(inner, definitions))),
        Type::Slice(inner) => Type::Slice(Box::new(type_view(inner, definitions))),
        Type::Tuple(types) => {
            Type::Tuple(types.iter().map(|ty| type_view(ty, definitions)).collect())
        }
        _ => ty.clone(),
    }
}
pub fn resolve(ast: &mut Ast) -> Result<HashMap<String, Type>> {
    let native = ast.native.clone();
    let definitions = record_definitions(std::iter::once(&*ast), &native)?;
    resolve_using(ast, &definitions)
}
#[derive(Clone)]
enum CompositeDefinition {
    Record(Record),
    Enum(EnumDeclaration),
    /// A declared `core` enum denoted by a `Storage` marker.
    Native(EnumDeclaration),
}
fn native_key(kind: StorageKind) -> String {
    format!("$native:{kind:?}")
}
fn record_definitions<'a>(
    asts: impl Iterator<Item = &'a Ast>,
    native: &NativeDeclarations,
) -> Result<HashMap<String, CompositeDefinition>> {
    let mut definitions = HashMap::new();
    for declaration in &native.enums {
        definitions.insert(
            native_key(declaration.storage),
            CompositeDefinition::Native(declaration.declaration.clone()),
        );
    }
    for ast in asts {
        for e in &ast.enums {
            if definitions
                .insert(key(&e.name), CompositeDefinition::Enum(e.clone()))
                .is_some()
            {
                return Err(crate::enums::invalid_enum(
                    crate::enums::EnumError::DuplicateType {
                        name: e.name.clone(),
                    },
                    e.span,
                ));
            }
        }
        for r in &ast.records {
            if definitions
                .insert(key(&r.name), CompositeDefinition::Record(r.clone()))
                .is_some()
            {
                return Err(Diagnostic::new("E101", "duplicate record type", r.span));
            }
        }
    }
    Ok(definitions)
}
pub fn resolve_modules(
    asts: &mut [Ast],
    native: &NativeDeclarations,
) -> Result<HashMap<String, Type>> {
    let definitions = record_definitions(asts.iter(), native)?;
    let mut types = HashMap::new();
    for ast in asts {
        types.extend(resolve_using(ast, &definitions)?);
    }
    Ok(types)
}
fn resolve_using(
    ast: &mut Ast,
    definitions: &HashMap<String, CompositeDefinition>,
) -> Result<HashMap<String, Type>> {
    fn ty(
        t: &mut Type,
        defs: &HashMap<String, CompositeDefinition>,
        active: &mut HashMap<String, usize>,
        depth: usize,
        span: Span,
    ) -> Result<()> {
        match t {
            Type::Applied(..) => {
                return Err(Diagnostic::new(
                    "E151",
                    "unknown generic record or missing specialization",
                    span,
                ));
            }
            Type::Named(name) => {
                let identity = key(name);
                let definition = defs.get(&identity).ok_or_else(|| {
                    Diagnostic::new(
                        "E116",
                        format!("unknown type `{}`", cyallo_ast::source_name(name)),
                        span,
                    )
                })?;
                if let CompositeDefinition::Enum(declaration) = definition {
                    let resolved = declaration.resolved.as_ref().ok_or_else(|| {
                        crate::enums::invalid_enum(
                            crate::enums::EnumError::UnresolvedDeclaration {
                                name: declaration.name.clone(),
                            },
                            span,
                        )
                    })?;
                    let instance = EnumIdentity {
                        declaration: resolved.declaration,
                        arguments: resolved.arguments.clone(),
                    };
                    if let Some(start) = active.get(&identity) {
                        if depth > *start {
                            *t = Type::EnumRef(Box::new(instance), declaration.name.clone());
                            return Ok(());
                        }
                        return Err(crate::enums::invalid_enum(
                            crate::enums::EnumError::RecursiveValue {
                                name: declaration.name.clone(),
                            },
                            declaration.span,
                        ));
                    }
                    active.insert(identity.clone(), depth);
                    let mut variants = vec![];
                    for variant in &declaration.variants {
                        let resolved = variant.resolved.as_ref().ok_or_else(|| {
                            crate::enums::invalid_enum(
                                crate::enums::EnumError::UnresolvedDeclaration {
                                    name: variant.name.clone(),
                                },
                                variant.span,
                            )
                        })?;
                        let mut payload = variant.payload.clone();
                        if let Some(payload) = &mut payload {
                            ty(payload, defs, active, depth, variant.span)?;
                        }
                        variants.push(VariantType {
                            encoding: resolved.encoding,
                            declaration: resolved.declaration,
                            name: variant.name.clone(),
                            symbol: resolved.symbol.clone(),
                            payload,
                        });
                    }
                    active.remove(&identity);
                    *t = Type::Enum(std::sync::Arc::new(EnumType {
                        identity: instance,
                        name: declaration.name.clone(),
                        package: resolved.package,
                        open: declaration.open,
                        variants,
                    }));
                    return Ok(());
                }
                let CompositeDefinition::Record(record) = definition else {
                    unreachable!()
                };
                if let Some(start) = active.get(&identity) {
                    if depth > *start {
                        *t = Type::RecordRef(identity.into());
                        return Ok(());
                    }
                    return Err(Diagnostic::new(
                        "E116",
                        "recursive value types require indirection",
                        record.span,
                    ));
                }
                active.insert(identity.clone(), depth);
                let mut fields = record.fields.clone();
                let mut seen = HashSet::new();
                for (name, field) in &mut fields {
                    if !seen.insert(key(name)) {
                        return Err(Diagnostic::new(
                            "E101",
                            "duplicate record field",
                            record.span,
                        ));
                    }
                    ty(field, defs, active, depth, record.span)?;
                }
                active.remove(&identity);
                *t = Type::Record(identity.into(), fields);
            }
            Type::AnonymousRecord(fields) => {
                for (_, field) in fields {
                    ty(field, defs, active, depth, span)?;
                }
            }
            Type::Tuple(types) => {
                for t in types {
                    ty(t, defs, active, depth, span)?;
                }
            }
            Type::Reference { pointee: t, .. } | Type::Managed { pointee: t, .. } => {
                ty(t, defs, active, depth + 1, span)?
            }
            Type::Opaque(_, arguments, traits) if traits.indirect() => {
                for t in arguments {
                    ty(t, defs, active, depth + 1, span)?;
                }
            }
            Type::Slice(t)
            | Type::Array(_, t)
            | Type::SparseArray(t)
            | Type::ServiceKey(t)
            | Type::ServiceRef(t) => ty(t, defs, active, depth, span)?,
            Type::Storage(kind, inner) => {
                ty(inner, defs, active, depth, span)?;
                // A core enum spelled as its marker becomes the declared enum.
                if let Some(CompositeDefinition::Native(declaration)) = defs.get(&native_key(*kind))
                    && let Some(contract) = crate::enums::contract_for(declaration, *kind, inner)
                {
                    *t = Type::Enum(std::sync::Arc::new(contract));
                }
            }
            Type::Opaque(_, arguments, _) => {
                for t in arguments {
                    ty(t, defs, active, depth, span)?;
                }
            }
            Type::Borrowed(inner) => {
                ty(inner, defs, active, depth, span)?;
                if !matches!(inner.as_ref(), Type::Function(..)) {
                    return Err(Diagnostic::new(
                        "E116",
                        "borrow requires a function type",
                        span,
                    ));
                }
            }
            Type::Function(kind, params, result) => {
                if *kind == RoutineKind::Function && params.iter().any(|(_, by_ref)| *by_ref) {
                    return Err(Diagnostic::new(
                        "E102",
                        "fn parameters must use values",
                        span,
                    ));
                }
                for (t, _) in params {
                    ty(t, defs, active, depth + 1, span)?;
                }
                ty(result, defs, active, depth + 1, span)?;
                if borrowed(result) {
                    return Err(Diagnostic::new(
                        "E117",
                        "borrowed closures cannot escape through a result type",
                        span,
                    ));
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn function(f: &mut Function, defs: &HashMap<String, CompositeDefinition>) -> Result<()> {
        for p in &mut f.params {
            ty(&mut p.ty, defs, &mut HashMap::new(), 0, p.span)?;
        }
        ty(&mut f.result, defs, &mut HashMap::new(), 0, f.span)?;
        if f.origin == RoutineOrigin::EnumConstructor
            && crate::ownership::effectful_lifecycle(&f.result)
        {
            f.kind = RoutineKind::Procedure;
        }
        if borrowed(&f.result) {
            return Err(Diagnostic::new(
                "E117",
                "borrowed closures cannot escape through a result type",
                f.span,
            ));
        }
        block(&mut f.body, defs)
    }
    fn block(body: &mut [Stmt], defs: &HashMap<String, CompositeDefinition>) -> Result<()> {
        visit_block(
            body,
            &mut |e| {
                if let ExprKind::Cast(_, t) = &mut e.kind {
                    ty(t, defs, &mut HashMap::new(), 0, e.span)?;
                }
                if let ExprKind::IntrinsicCall(_, types, _) = &mut e.kind {
                    for t in types {
                        ty(t, defs, &mut HashMap::new(), 0, e.span)?;
                    }
                }
                if let ExprKind::Closure(f, _) = &mut e.kind {
                    function(f, defs)?;
                }
                Ok(())
            },
            &mut |t, span| ty(t, defs, &mut HashMap::new(), 0, span),
        )
    }
    block(&mut ast.body, definitions)?;
    for f in &mut ast.functions {
        function(f, definitions)?;
    }
    for t in &mut ast.traits {
        for f in &mut t.methods {
            for p in &mut f.params {
                if !p.receiver {
                    ty(&mut p.ty, definitions, &mut HashMap::new(), 0, p.span)?;
                }
            }
            ty(&mut f.result, definitions, &mut HashMap::new(), 0, f.span)?;
        }
    }
    for implementation in &mut ast.implementations {
        ty(
            &mut implementation.receiver,
            definitions,
            &mut HashMap::new(),
            0,
            implementation.span,
        )?;
        for f in &mut implementation.methods {
            // `Self` is the implementing type wherever the signature writes it, not
            // only in the receiver: `other: &Self` and `-> Self` too.
            let receiver = &implementation.receiver;
            let replace = |ty: &mut Type| {
                cyallo_ast::visit::visit_type(ty, &mut |ty| {
                    if *ty == Type::SelfType {
                        *ty = receiver.clone();
                    }
                });
            };
            for p in &mut f.params {
                replace(&mut p.ty);
            }
            replace(&mut f.result);
            function(f, definitions)?;
        }
    }
    for property in &mut ast.properties {
        ty(
            &mut property.receiver,
            definitions,
            &mut HashMap::new(),
            0,
            property.span,
        )?;
        function(&mut property.getter, definitions)?;
    }
    let mut types = HashMap::new();
    for declaration in &ast.enums {
        let mut t = Type::Named(declaration.name.clone());
        ty(
            &mut t,
            definitions,
            &mut HashMap::new(),
            0,
            declaration.span,
        )?;
        types.insert(key(&declaration.name), t);
    }
    for r in &ast.records {
        let mut t = Type::Named(r.name.clone());
        ty(&mut t, definitions, &mut HashMap::new(), 0, r.span)?;
        types.insert(key(&r.name), t);
    }
    Ok(types)
}
// The visitor deliberately treats closure bodies as separate scopes.
pub use cyallo_ast::visit::{visit_block, visit_expr};
pub fn lift(
    ast: &mut Ast,
    globals: &[Local],
    types: &crate::declarations::TypeEnvironment,
    options: CompileOptions,
) -> Result<()> {
    lift_module(ast, globals, types, options, "main")
}
pub fn lift_module(
    ast: &mut Ast,
    globals: &[Local],
    types: &crate::declarations::TypeEnvironment,
    options: CompileOptions,
    module: &str,
) -> Result<()> {
    fn block(
        body: &mut [Stmt],
        known: &[Local],
        types: &crate::declarations::TypeEnvironment,
        options: CompileOptions,
        generated: &mut Vec<Function>,
        owner: &str,
    ) -> Result<()> {
        let mut closure_index = 0;
        visit_block(
            body,
            &mut |e| {
                let ExprKind::Closure(original, refs) = &e.kind else {
                    return Ok(());
                };
                let mut f = original.as_ref().clone();
                if f.kind == RoutineKind::Function && f.params.iter().any(|p| p.by_ref) {
                    return Err(Diagnostic::new(
                        "E102",
                        "fn parameters must use values",
                        e.span,
                    ));
                }
                let external_type = function_type(&f);
                // Explicit local bindings shadow captures. Implicit assignment uses
                // existing outer storage, just like normal routine/global lookup.
                let local_names = bound_names(&f);
                let mut used = HashSet::new();
                fn names(body: &mut [Stmt], used: &mut HashSet<String>) -> Result<()> {
                    visit_block(
                        body,
                        &mut |e| {
                            match &mut e.kind {
                                ExprKind::Name(n) | ExprKind::Call(n, _) => {
                                    used.insert(key(n));
                                }
                                ExprKind::Closure(f, _) => {
                                    let mut nested = HashSet::new();
                                    names(&mut f.body, &mut nested)?;
                                    for name in bound_names(f) {
                                        nested.remove(&name);
                                    }
                                    used.extend(nested);
                                }
                                _ => {}
                            }
                            Ok(())
                        },
                        &mut |_, _| Ok(()),
                    )
                }
                names(&mut f.body, &mut used)?;
                let mut reference_captures = HashMap::new();
                for capture in refs {
                    let name = &capture.name;
                    if reference_captures
                        .insert(key(name), capture.mutable)
                        .is_some()
                        || !used.contains(&key(name))
                        || local_names.contains(&key(name))
                        || !known.iter().any(|local| local.name.eq_identifier(name))
                    {
                        return Err(Diagnostic::new(
                            "E117",
                            format!(
                                "invalid or unused reference capture `{}`",
                                cyallo_ast::source_name(name)
                            ),
                            e.span,
                        ));
                    }
                }
                let mut captures = vec![];
                let mut params = vec![];
                let mut is_borrowed = false;
                let mut ordered: Vec<_> = known.iter().collect();
                ordered.sort_by_key(|local| key(&local.name));
                for local in ordered {
                    if !used.contains(&key(&local.name)) || local_names.contains(&key(&local.name))
                    {
                        continue;
                    }
                    if f.generator && f.kind == RoutineKind::Function {
                        return Err(cyallo_ast::diagnostic::generator_capture_requires_proc(
                            e.span,
                        ));
                    }
                    let reference = reference_captures.get(&key(&local.name)).copied();
                    if reference == Some(true) && local.constant {
                        return Err(Diagnostic::new(
                            "E107",
                            "cannot borrow an immutable binding mutably",
                            e.span,
                        ));
                    }
                    let mut value = Expr {
                        kind: ExprKind::Name(local.name.clone()),
                        span: e.span,
                    };
                    let mut ty = local.ty.clone();
                    if let Some(mutable) = reference {
                        value = Expr {
                            kind: ExprKind::Unary(
                                if mutable {
                                    Operator::BorrowMut
                                } else {
                                    Operator::Borrow
                                },
                                Box::new(value),
                            ),
                            span: e.span,
                        };
                        ty = Type::Reference {
                            mutable,
                            lifetime: None,
                            pointee: Box::new(ty),
                        };
                    }
                    is_borrowed |= reference.is_some() || borrowed(&local.ty);
                    captures.push(value);
                    params.push(Param {
                        arity: ParameterArity::One,
                        passing: ParameterPassing::Value,
                        receiver: false,
                        name: local.name.clone(),
                        ty,
                        by_ref: false,
                        span: e.span,
                    });
                }
                fn dereference_captures(
                    body: &mut [Stmt],
                    references: &HashMap<String, bool>,
                ) -> Result<()> {
                    visit_block(
                        body,
                        &mut |expression| {
                            match &mut expression.kind {
                                ExprKind::Name(name) if references.contains_key(name.as_ref()) => {
                                    expression.kind = ExprKind::Unary(
                                        Operator::Dereference,
                                        Box::new(expression.clone()),
                                    );
                                }
                                ExprKind::Call(name, arguments)
                                    if references.contains_key(name.as_ref()) =>
                                {
                                    let target = Expr {
                                        kind: ExprKind::Unary(
                                            Operator::Dereference,
                                            Box::new(Expr {
                                                kind: ExprKind::Name(name.clone()),
                                                span: expression.span,
                                            }),
                                        ),
                                        span: expression.span,
                                    };
                                    expression.kind = ExprKind::Invoke(
                                        Box::new(target),
                                        std::mem::take(arguments),
                                    );
                                }
                                ExprKind::Closure(function, _) => {
                                    dereference_captures(&mut function.body, references)?
                                }
                                _ => {}
                            }
                            Ok(())
                        },
                        &mut |_, _| Ok(()),
                    )
                }
                dereference_captures(&mut f.body, &reference_captures)?;
                params.append(&mut f.params);
                f.params = params;
                // Reserve a stable name before descending into nested closures.
                let index = generated.len();
                f.origin = RoutineOrigin::Closure;
                f.name = format!("$closure{closure_index}${owner}").into();
                closure_index += 1;
                generated.push(f.clone());
                let mut locals = analysis::collect(&f.body, &f.params)?;
                crate::generator::input_local(&f, &mut locals);
                crate::inference::infer(&f.body, &mut locals, &[], types, options)?;
                block(&mut f.body, &locals, types, options, generated, &f.name)?;
                generated[index] = f.clone();
                let ty = if is_borrowed {
                    Type::Borrowed(Box::new(external_type))
                } else {
                    external_type
                };
                e.kind = ExprKind::ClosureValue(f.name, captures, ty);
                Ok(())
            },
            &mut |_, _| Ok(()),
        )
    }
    let mut generated = vec![];
    block(
        &mut ast.body,
        globals,
        types,
        options,
        &mut generated,
        module,
    )?;
    for f in &mut ast.functions {
        // Keep deferred routines deferred when they contain no closure syntax.
        let mut has_closure = false;
        visit_block(
            &mut f.body,
            &mut |e| {
                has_closure |= matches!(e.kind, ExprKind::Closure(..));
                Ok(())
            },
            &mut |_, _| Ok(()),
        )?;
        if !has_closure {
            continue;
        }
        let mut locals = analysis::collect(&f.body, &f.params)?;
        crate::generator::input_local(f, &mut locals);
        crate::inference::infer(&f.body, &mut locals, globals, types, options)?;
        let mut known = globals.to_vec();
        known.retain(|g| !locals.iter().any(|l| l.name.eq_identifier(&g.name)));
        known.extend(locals);
        block(&mut f.body, &known, types, options, &mut generated, &f.name)?;
    }
    ast.functions.extend(generated);
    Ok(())
}
