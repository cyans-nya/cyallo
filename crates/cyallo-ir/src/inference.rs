//! Fixed-type inference for the explicit simplified dialect.
use crate::{
    analysis::{CompileOptions, binary_type, literal_type},
    ir::Local,
};
use cyallo_ast::IdentifierEq as _;
use cyallo_ast::*;
use std::collections::HashMap;
fn key(s: &str) -> String {
    s.to_owned()
}
struct Candidate {
    name: String,
    depth: usize,
    span: Span,
    constant: bool,
    binding: bool,
    value: Option<Expr>,
}
pub fn infer(
    body: &[Stmt],
    locals: &mut Vec<Local>,
    globals: &[Local],
    routines: &crate::declarations::TypeEnvironment,
    _options: CompileOptions,
) -> Result<(), Diagnostic> {
    fn root(e: &Expr) -> Option<(&str, usize)> {
        match &e.kind {
            ExprKind::Name(n) => Some((n, 0)),
            ExprKind::Index(base, indices) if indices.len() == 1 => {
                root(base).map(|(n, d)| (n, d + 1))
            }
            _ => None,
        }
    }
    fn gather(
        body: &[Stmt],
        candidates: &mut Vec<Candidate>,
        loops: &mut Vec<(String, Option<Expr>, Span)>,
    ) {
        cyallo_ast::with_stack(|| {
            for s in body {
                match &s.kind {
                    StmtKind::Binding(name, _, constant, mutable, value) => {
                        candidates.push(Candidate {
                            name: name.to_string(),
                            depth: 0,
                            span: s.span,
                            constant: *constant || !*mutable,
                            binding: true,
                            value: Some(value.clone()),
                        })
                    }
                    StmtKind::Assign(target, value) => {
                        if let Some((name, depth)) = root(target) {
                            candidates.push(Candidate {
                                name: name.to_string(),
                                depth,
                                span: target.span,
                                constant: false,
                                binding: false,
                                value: Some(value.clone()),
                            });
                        }
                    }
                    StmtKind::Input(target) => {
                        if let Some((name, depth)) = root(target) {
                            candidates.push(Candidate {
                                name: name.to_string(),
                                depth,
                                span: target.span,
                                constant: false,
                                binding: false,
                                value: None,
                            });
                        }
                    }
                    StmtKind::For(n, _, _, _, b) => {
                        loops.push((n.to_string(), None, s.span));
                        gather(b, candidates, loops);
                    }
                    StmtKind::ForIn(n, iterable, b) => {
                        loops.push((n.to_string(), Some(iterable.clone()), s.span));
                        gather(b, candidates, loops);
                    }
                    StmtKind::If(_, a, b) => {
                        gather(a, candidates, loops);
                        gather(b, candidates, loops);
                    }
                    StmtKind::While(_, b) | StmtKind::Repeat(b, _) => gather(b, candidates, loops),
                    StmtKind::Case(_, arms, b) => {
                        for (_, b) in arms {
                            gather(b, candidates, loops);
                        }
                        gather(b, candidates, loops);
                    }
                    _ => {}
                }
            }
        })
    }
    let mut candidates = vec![];
    let mut loops = vec![];
    gather(body, &mut candidates, &mut loops);
    let mut patterns = vec![];
    let mut copy = body.to_vec();
    cyallo_ast::visit::visit_block(
        &mut copy,
        &mut |expression| {
            if let ExprKind::Match(value, arms) = &expression.kind {
                for arm in arms {
                    patterns.push((arm.pattern.clone(), *value.clone()));
                    if let MatchBody::Block(body, _) = &arm.body {
                        gather(body, &mut candidates, &mut loops);
                    }
                }
            }
            Ok(())
        },
        &mut |_, _| Ok(()),
    )?;
    let mut known: HashMap<_, _> = globals
        .iter()
        .chain(locals.iter())
        .map(|l| (key(&l.name), l.ty.clone()))
        .collect();
    for candidate in candidates.iter().filter(|c| c.binding) {
        if !locals.iter().any(|l| l.name.eq_identifier(&candidate.name)) {
            known.remove(&key(&candidate.name));
        }
    }
    let mut range_loops = vec![];
    for (n, iterable, span) in loops {
        match iterable {
            // CAIE FOR counters are INTEGER.
            None => {
                if let std::collections::hash_map::Entry::Vacant(entry) = known.entry(key(&n)) {
                    entry.insert(Type::Integer);
                    locals.push(Local {
                        display_name: Some(n.clone().into()),
                        name: n.into(),
                        ty: Type::Integer,
                        constant: false,
                        span,
                    });
                }
            }
            Some(iterable) => range_loops.push((n, iterable, span)),
        }
    }
    loop {
        let mut changed = false;
        // A range loop variable has the range's element type once that is known.
        for (name, iterable, span) in &range_loops {
            if known.contains_key(&key(name)) {
                continue;
            }
            if let Ok(Type::Storage(StorageKind::Range, element)) =
                expression_type(iterable, &known, routines)
            {
                known.insert(key(name), (*element).clone());
                locals.push(Local {
                    display_name: Some(name.clone().into()),
                    name: name.clone().into(),
                    ty: *element,
                    constant: false,
                    span: *span,
                });
                changed = true;
            }
        }
        for (pattern, value) in &patterns {
            if let Ok(ty) = crate::patterns::scrutinee_type(value, &known, routines) {
                for binding in crate::patterns::check(pattern, &ty, routines)?.bindings() {
                    if !known.contains_key(binding.name.as_ref()) {
                        known.insert(binding.name.to_string(), binding.ty.clone());
                        locals.push(Local {
                            display_name: Some(binding.name.clone()),
                            name: binding.name,
                            ty: binding.ty,
                            constant: true,
                            span: binding.span,
                        });
                        changed = true;
                    }
                }
            }
        }
        for c in &candidates {
            let (name, depth) = (&c.name, c.depth);
            if known.contains_key(&key(name)) {
                continue;
            }
            let inferred = match &c.value {
                Some(e) => expression_type(e, &known, routines),
                None => Ok(Type::String),
            };
            if let Ok(mut ty) = inferred {
                for _ in 0..depth {
                    ty = Type::SparseArray(Box::new(ty));
                }
                known.insert(key(name), ty.clone());
                locals.push(Local {
                    display_name: Some(name.clone().into()),
                    name: name.clone().into(),
                    ty,
                    constant: c.constant,
                    span: c.span,
                });
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    for candidate in candidates.iter().filter(|c| c.binding) {
        if let Some(local) = locals
            .iter_mut()
            .find(|l| l.name.eq_identifier(&candidate.name))
        {
            local.constant = candidate.constant;
        }
    }
    let unresolved: std::collections::HashSet<&str> = candidates
        .iter()
        .map(|c| c.name.as_str())
        .filter(|name| !known.contains_key(&key(name)))
        .collect();
    for c in &candidates {
        let name = &c.name;
        if !known.contains_key(&key(name)) {
            // An initializer that does not wait on another uninferred binding
            // failed on its own; report that error rather than hiding it.
            if let Some(value) = &c.value
                && !mentions(value, &unresolved)
                && let Err(error) = expression_type(value, &known, routines)
            {
                return Err(error);
            }
            return Err(Diagnostic::new(
                "E114",
                format!(
                    "cannot infer the type of `{}`; initialize it with a known value or declare its type",
                    cyallo_ast::source_name(name)
                ),
                c.span,
            ));
        }
    }
    Ok(())
}
/// Whether `expression` names any of `names`.
fn mentions(expression: &Expr, names: &std::collections::HashSet<&str>) -> bool {
    let mut copy = expression.clone();
    let mut found = false;
    let _ = cyallo_ast::visit::visit_expr(&mut copy, &mut |e: &mut Expr| {
        if let ExprKind::Name(name) = &e.kind {
            found |= names.contains(name.as_ref());
        }
        Ok(())
    });
    found
}
pub fn expression_type(
    e: &Expr,
    known: &HashMap<String, Type>,
    routines: &crate::declarations::TypeEnvironment,
) -> Result<Type, Diagnostic> {
    let ty = expression_type_inner(e, known, routines)?;
    Ok(crate::composites::type_view(&ty, routines))
}
fn expression_type_inner(
    e: &Expr,
    known: &HashMap<String, Type>,
    routines: &crate::declarations::TypeEnvironment,
) -> Result<Type, Diagnostic> {
    cyallo_ast::with_stack(|| match &e.kind {
        ExprKind::EnumConstruct(_, ty, _) => Ok(ty.clone()),
        ExprKind::AnonymousRecord(fields) | ExprKind::VariantPayload(fields) => {
            let mut types = fields
                .iter()
                .map(|(name, value)| Ok((name.clone(), expression_type(value, known, routines)?)))
                .collect::<Result<Vec<_>, Diagnostic>>()?;
            types.sort_by(|(a, _), (b, _)| a.cmp(b));
            Ok(Type::AnonymousRecord(types))
        }
        ExprKind::Match(value, arms) => {
            crate::patterns::expression_type(value, arms, known, routines, e.span)
        }
        ExprKind::TypeDescriptor(_) => Ok(Type::Storage(
            StorageKind::TypeDescriptor,
            Box::new(Type::Void),
        )),
        ExprKind::DeferredCallable(_) => Ok(Type::Dynamic),
        ExprKind::DeleteProperty(_) => Ok(Type::Boolean),
        ExprKind::ObjectLiteral(kind, fields) => {
            if *kind == ObjectKind::Dynamic {
                Ok(Type::Dynamic)
            } else {
                Ok(Type::Object(
                    fields
                        .iter()
                        .map(|(name, value)| {
                            Ok((name.clone(), expression_type(value, known, routines)?))
                        })
                        .collect::<Result<_, Diagnostic>>()?,
                ))
            }
        }
        ExprKind::NativeCoerce(value, thunk) => {
            expression_type(value, known, routines)?;
            expression_type(thunk, known, routines)
        }
        ExprKind::Yield(value, delegate) => {
            if *delegate {
                return crate::generator::parts(&expression_type(value, known, routines)?)
                    .map(|(_, _, returned)| returned.clone())
                    .ok_or_else(|| cyallo_ast::diagnostic::generator_delegate_required(e.span));
            }
            known
                .get(crate::generator::INPUT)
                .cloned()
                .ok_or_else(|| cyallo_ast::diagnostic::yield_outside_generator(e.span))
        }
        ExprKind::Invalid(diagnostic) => Err(diagnostic.clone()),
        ExprKind::ManagedNew(kind, locality, allocator, value) => Ok(crate::memory::managed_type(
            *kind,
            *locality,
            allocator
                .as_ref()
                .map(|allocator| expression_type(allocator, known, routines))
                .transpose()?
                .as_ref(),
            expression_type(value, known, routines)?,
        )),

        ExprKind::Frozen(_, ty) => Ok(ty.clone()),
        ExprKind::GenericCall(..) | ExprKind::GenericRecord(..) => Err(Diagnostic::new(
            "E151",
            "generic expression has not been specialized",
            e.span,
        )),
        ExprKind::IntrinsicCall(id, type_args, args) => {
            let arguments =
                crate::analysis::intrinsic_arguments(*id, type_args, args, known, routines)?;
            let types = arguments
                .iter()
                .map(|argument| expression_type(argument, known, routines))
                .collect::<Result<Vec<_>, Diagnostic>>()?;
            if let Some(declaration) = id.1 {
                routines.check_native_access(declaration, e.span)?;
                return Ok(crate::native_contract::function(
                    routines,
                    declaration,
                    type_args,
                    &arguments,
                    &types,
                    e.span,
                )?
                .result);
            }
            Ok(crate::intrinsic::resolve_declared(
                *id, routines, type_args, &arguments, &types, e.span,
            )?
            .result)
        }
        ExprKind::TypedCollection(..) => Err(Diagnostic::new(
            "E151",
            "unresolved collection initializer",
            e.span,
        )),
        ExprKind::Cast(_, t) => Ok(t.clone()),
        ExprKind::ExtensionField(base, name, getter) => {
            if let Type::Record(_, fs) | Type::AnonymousRecord(fs) =
                expression_type(base, known, routines)?
                && let Some((_, t)) = fs.iter().find(|(n, _)| n.eq_identifier(name))
            {
                return Ok(t.clone());
            }
            routines
                .get(&key(getter))
                .and_then(crate::composites::callable)
                .map(|(_, _, r)| r.clone())
                .ok_or_else(|| Diagnostic::new("E140", "unknown imported extension", e.span))
        }
        ExprKind::Closure(f, refs) => {
            let t = crate::composites::function_type(f);
            Ok(if refs.is_empty() {
                t
            } else {
                Type::Borrowed(Box::new(t))
            })
        }
        ExprKind::ClosureValue(_, _, t) => Ok(t.clone()),
        ExprKind::Tuple(es) => Ok(Type::Tuple(
            es.iter()
                .map(|e| expression_type(e, known, routines))
                .collect::<Result<_, _>>()?,
        )),
        ExprKind::Record(n, _) => routines
            .get(&key(n))
            .cloned()
            .ok_or_else(|| Diagnostic::new("E116", "unknown record type", e.span)),
        ExprKind::Field(a, n) => match expression_type(a, known, routines)?
            .pointee_or_self()
            .clone()
        {
            Type::Dynamic => Ok(Type::Dynamic),
            Type::Object(fields) => Ok(fields
                .into_iter()
                .find(|(name, _)| name == n)
                .map_or(Type::Dynamic, |(_, ty)| ty)),
            Type::Record(_, fs) | Type::AnonymousRecord(fs) => fs
                .into_iter()
                .find(|(name, _)| name.eq_identifier(n))
                .map(|(_, t)| t)
                .ok_or_else(|| Diagnostic::new("E116", "unknown record field", e.span)),
            _ => Err(Diagnostic::new(
                "E116",
                "field access requires a record",
                e.span,
            )),
        },
        ExprKind::TupleField(a, i) => match expression_type(a, known, routines)?
            .pointee_or_self()
            .clone()
        {
            Type::Tuple(ts) => ts
                .get(*i)
                .cloned()
                .ok_or_else(|| Diagnostic::new("E116", "tuple index out of bounds", e.span)),
            _ => Err(Diagnostic::new(
                "E116",
                "tuple projection requires a tuple",
                e.span,
            )),
        },
        ExprKind::Invoke(a, args) => {
            if expression_type(a, known, routines).is_ok_and(|ty| ty == Type::Dynamic) {
                return Ok(Type::Dynamic);
            }
            if let Ok(Type::NativeCallable(declaration)) = expression_type(a, known, routines) {
                let reference = routines.function_reference(declaration, e.span)?;
                return expression_type(
                    &Expr {
                        kind: ExprKind::IntrinsicCall(reference, vec![], args.clone()),
                        span: e.span,
                    },
                    known,
                    routines,
                );
            }
            if let ExprKind::Field(base, name) | ExprKind::ExtensionField(base, name, _) = &a.kind {
                let receiver = expression_type(base, known, routines)?;
                let parameters =
                    crate::native_contract::parameter_types(routines, &receiver, name, e.span);
                let ts = args
                    .iter()
                    .enumerate()
                    .map(|(index, arg)| {
                        let argument = parameters
                            .as_ref()
                            .and_then(|types| types.get(index))
                            .map(|ty| crate::analysis::contextualize(arg, ty))
                            .transpose()?
                            .unwrap_or_else(|| arg.clone());
                        expression_type(&argument, known, routines)
                    })
                    .collect::<Result<Vec<_>, Diagnostic>>()?;
                let actual_field = matches!(&receiver,Type::Record(_,fs) | Type::AnonymousRecord(fs) if fs.iter().any(|(n,_)|n.eq_identifier(name)));
                if !actual_field
                    && let Some(t) = routines
                        .get(&format!("$CONCRETE${:?}.{}", receiver, key(name)))
                        .and_then(crate::composites::callable)
                {
                    return Ok(t.2.clone());
                }
                if let Some(method) =
                    crate::member::resolve(routines, &receiver, name, args, &ts, e.span)?
                {
                    return Ok(method.result);
                }
                let trait_type = match receiver.pointee_or_self() {
                    Type::ServiceRef(t) => t.as_ref(),
                    t => t,
                };
                if let Type::TraitObject(n) = trait_type {
                    let method = routines
                        .get(&format!("$METHOD${}.{}", key(n), key(name)))
                        .and_then(crate::composites::callable)
                        .map(|(_, _, r)| r.clone());
                    if let Some(result) = method {
                        return Ok(result);
                    }
                    if matches!(a.kind, ExprKind::Field(..)) {
                        return Err(Diagnostic::new("E130", "unknown trait method", e.span));
                    }
                }
            }
            crate::composites::callable(&expression_type(a, known, routines)?)
                .map(|(_, _, r)| r.clone())
                .ok_or_else(|| Diagnostic::new("E110", "value is not callable", e.span))
        }
        ExprKind::Literal(l) => Ok(literal_type(l)),
        ExprKind::ArrayLiteral(values) => {
            let first = values
                .first()
                .ok_or_else(|| Diagnostic::new("E114", "cannot infer an empty array", e.span))?;
            Ok(Type::Array(
                vec![(0, values.len() as i64 - 1)],
                Box::new(expression_type(first, known, routines)?),
            ))
        }
        ExprKind::Binary(Operator::Range | Operator::RangeInclusive, a, _) => Ok(Type::Storage(
            StorageKind::Range,
            Box::new(expression_type(a, known, routines)?),
        )),
        ExprKind::Name(n) => known
            .get(&key(n))
            .or_else(|| routines.get(&key(n)))
            .cloned()
            .ok_or_else(|| Diagnostic::new("E114", "type is not known yet", e.span)),
        ExprKind::Parenthesized(a) => expression_type(a, known, routines),
        ExprKind::Unary(Operator::Borrow | Operator::BorrowMut, a) => Ok(Type::Reference {
            mutable: matches!(e.kind, ExprKind::Unary(Operator::BorrowMut, _)),
            lifetime: None,
            pointee: Box::new(expression_type(a, known, routines)?),
        }),
        ExprKind::Unary(Operator::Dereference, a) => match expression_type(a, known, routines)? {
            Type::Reference { pointee, .. } | Type::Managed { pointee, .. } => Ok(*pointee),
            Type::Opaque(_, mut arguments, traits) if traits.deref() && arguments.len() == 1 => {
                Ok(arguments.remove(0))
            }
            _ => Err(Diagnostic::new(
                "E109",
                "dereference requires a reference, managed owner, or Cell",
                e.span,
            )),
        },
        ExprKind::Unary(Operator::Not, a)
            if expression_type(a, known, routines)? == Type::Dynamic =>
        {
            Ok(Type::Boolean)
        }
        ExprKind::Unary(_, a) => expression_type(a, known, routines),
        ExprKind::Binary(op, a, b) => {
            let ta = expression_type(a, known, routines)?;
            let tb = expression_type(b, known, routines)?;
            if ta.number_kind().is_some() && tb.number_kind().is_some() {
                let context = crate::analysis::numeric_context(a, &ta, b, &tb);
                let a = crate::analysis::contextualize(a, &context)?;
                let b = crate::analysis::contextualize(b, &context)?;
                binary_type(
                    *op,
                    &expression_type(&a, known, routines)?,
                    &expression_type(&b, known, routines)?,
                    e.span,
                )
            } else {
                binary_type(*op, &ta, &tb, e.span)
            }
        }
        ExprKind::Index(a, _) => match expression_type(a, known, routines)?
            .pointee_or_self()
            .clone()
        {
            Type::Dynamic | Type::Object(_) => Ok(Type::Dynamic),
            Type::Array(_, element) | Type::SparseArray(element) | Type::Slice(element) => {
                Ok(*element)
            }
            collection if collection.collection_element().is_some() => {
                Ok(collection.collection_element().unwrap().clone())
            }
            _ => Err(Diagnostic::new("E114", "not an array", e.span)),
        },
        ExprKind::Call(n, args) => {
            if let Some(t) = known.get(&key(n)).or_else(|| routines.get(&key(n))) {
                if *t == Type::Dynamic {
                    return Ok(Type::Dynamic);
                }
                if let Type::NativeCallable(declaration) = t {
                    let reference = routines.function_reference(*declaration, e.span)?;
                    return expression_type(
                        &Expr {
                            kind: ExprKind::IntrinsicCall(reference, vec![], args.clone()),
                            span: e.span,
                        },
                        known,
                        routines,
                    );
                }
                return crate::composites::callable(t)
                    .map(|(_, _, r)| r.clone())
                    .ok_or_else(|| Diagnostic::new("E110", "value is not callable", e.span));
            }
            Err(Diagnostic::new(
                "E106",
                format!("unresolved callable `{}`", cyallo_ast::source_name(n)),
                e.span,
            ))
        }
    })
}
