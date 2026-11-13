//! Fixed-type inference for the explicit simplified dialect.
use crate::intrinsic::Intrinsic;
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
struct Candidate<'a> {
    name: &'a str,
    depth: usize,
    span: Span,
    constant: bool,
    binding: bool,
    value: Option<&'a Expr>,
}
pub fn infer(
    body: &[Stmt],
    locals: &mut Vec<Local>,
    globals: &[Local],
    routines: &HashMap<String, Type>,
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
    fn gather<'a>(
        body: &'a [Stmt],
        candidates: &mut Vec<Candidate<'a>>,
        loops: &mut Vec<(&'a str, Span)>,
    ) {
        for s in body {
            match &s.kind {
                StmtKind::Binding(name, _, constant, mutable, value) => {
                    candidates.push(Candidate {
                        name,
                        depth: 0,
                        span: s.span,
                        constant: *constant || !*mutable,
                        binding: true,
                        value: Some(value),
                    })
                }
                StmtKind::Assign(target, value) => {
                    if let Some((name, depth)) = root(target) {
                        candidates.push(Candidate {
                            name,
                            depth,
                            span: target.span,
                            constant: false,
                            binding: false,
                            value: Some(value),
                        });
                    }
                }
                StmtKind::Input(target) => {
                    if let Some((name, depth)) = root(target) {
                        candidates.push(Candidate {
                            name,
                            depth,
                            span: target.span,
                            constant: false,
                            binding: false,
                            value: None,
                        });
                    }
                }
                StmtKind::For(n, _, _, _, b) => {
                    loops.push((n, s.span));
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
    }
    let mut candidates = vec![];
    let mut loops = vec![];
    gather(body, &mut candidates, &mut loops);
    let mut known: HashMap<_, _> = globals
        .iter()
        .chain(locals.iter())
        .map(|l| (key(&l.name), l.ty.clone()))
        .collect();
    for candidate in candidates.iter().filter(|c| c.binding) {
        if !locals.iter().any(|l| l.name.eq_identifier(candidate.name)) {
            known.remove(&key(candidate.name));
        }
    }
    for (n, span) in loops {
        if let std::collections::hash_map::Entry::Vacant(entry) = known.entry(key(n)) {
            entry.insert(Type::Integer);
            locals.push(Local {
                display_name: Some(n.into()),
                name: n.into(),
                ty: Type::Integer,
                constant: false,
                span,
            });
        }
    }
    loop {
        let mut changed = false;
        for c in &candidates {
            let (name, depth) = (c.name, c.depth);
            if known.contains_key(&key(name)) {
                continue;
            }
            let inferred = match c.value {
                Some(e) => expression_type(e, &known, routines),
                None => Ok(Type::String),
            };
            if let Ok(mut ty) = inferred {
                for _ in 0..depth {
                    ty = Type::SparseArray(Box::new(ty));
                }
                known.insert(key(name), ty.clone());
                locals.push(Local {
                    display_name: Some(name.into()),
                    name: name.into(),
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
            .find(|l| l.name.eq_identifier(candidate.name))
        {
            local.constant = candidate.constant;
        }
    }
    for c in candidates {
        let name = c.name;
        if !known.contains_key(&key(name)) {
            return Err(Diagnostic::new(
                "E114",
                format!(
                    "cannot infer type of {name}; initialize it with a known value or declare its type"
                ),
                c.span,
            ));
        }
    }
    Ok(())
}
pub fn expression_type(
    e: &Expr,
    known: &HashMap<String, Type>,
    routines: &HashMap<String, Type>,
) -> Result<Type, Diagnostic> {
    let ty = expression_type_inner(e, known, routines)?;
    Ok(crate::composites::type_view(&ty, routines))
}
fn expression_type_inner(
    e: &Expr,
    known: &HashMap<String, Type>,
    routines: &HashMap<String, Type>,
) -> Result<Type, Diagnostic> {
    match &e.kind {
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
        ExprKind::ManagedNew(kind, allocator, value) => Ok(crate::memory::managed_type(
            *kind,
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
            let id = Intrinsic::from_id(*id)
                .ok_or_else(|| Diagnostic::new("E112", "invalid intrinsic identity", e.span))?;
            let arguments =
                crate::analysis::intrinsic_arguments(id, type_args, args, known, routines)?;
            let types = arguments
                .iter()
                .map(|argument| expression_type(argument, known, routines))
                .collect::<Result<Vec<_>, Diagnostic>>()?;
            Ok(crate::intrinsic::resolve_at_call(id, type_args, args, &types, e.span)?.result)
        }
        ExprKind::TypedCollection(..) => Err(Diagnostic::new(
            "E151",
            "unresolved collection initializer",
            e.span,
        )),
        ExprKind::Cast(_, t) => Ok(t.clone()),
        ExprKind::ExtensionField(base, name, getter) => {
            if let Type::Record(_, fs) = expression_type(base, known, routines)?
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
            Type::Record(_, fs) => fs
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
            if let ExprKind::Field(base, name) | ExprKind::ExtensionField(base, name, _) = &a.kind {
                let receiver = expression_type(base, known, routines)?;
                let parameters = crate::lookup::parameter_types(&receiver, name);
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
                let actual_field = matches!(&receiver,Type::Record(_,fs) if fs.iter().any(|(n,_)|n.eq_identifier(name)));
                if !actual_field
                    && let Some(t) = routines
                        .get(&format!("$CONCRETE${:?}.{}", receiver, key(name)))
                        .and_then(crate::composites::callable)
                {
                    return Ok(t.2.clone());
                }
                let field = args.first().and_then(cyallo_ast::constant_text);
                if let Some(method) = crate::lookup::method(&receiver, name, &ts, field) {
                    return Ok(method.result);
                }
                if let Some((_, result)) = crate::services::intrinsic(&receiver, name, &ts) {
                    return Ok(result);
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
            Type::Storage(StorageKind::Cell, inner) => Ok(*inner),
            _ => Err(Diagnostic::new(
                "E109",
                "dereference requires a reference, managed owner, or Cell",
                e.span,
            )),
        },
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
            Type::Array(_, element)
            | Type::SparseArray(element)
            | Type::Slice(element)
            | Type::Storage(StorageKind::Vec | StorageKind::VecDeque, element) => Ok(*element),
            _ => Err(Diagnostic::new("E114", "not an array", e.span)),
        },
        ExprKind::Call(n, _args) => {
            if let Some(t) = known.get(&key(n)).or_else(|| routines.get(&key(n))) {
                return crate::composites::callable(t)
                    .map(|(_, _, r)| r.clone())
                    .ok_or_else(|| Diagnostic::new("E110", "value is not callable", e.span));
            }
            Err(Diagnostic::new(
                "E106",
                format!("unresolved callable {n}"),
                e.span,
            ))
        }
    }
}
