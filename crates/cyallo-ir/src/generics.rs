//! Monomorphization of records and routines before ordinary checked lowering.
mod enumeration;
use cyallo_ast::IdentifierEq as _;
use cyallo_ast::*;
use std::collections::HashMap;
type Result<T> = std::result::Result<T, Diagnostic>;
fn key(s: &str) -> String {
    s.to_owned()
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenericError {
    SpecializationLimit { limit: usize },
    TypeArity { expected: usize, actual: usize },
    UnknownRecord { name: Box<str> },
    UnknownRoutine { name: Box<str> },
    ArgumentArity { expected: usize, actual: usize },
    IncompatibleArguments,
    DuplicateRecord { name: Box<str> },
    DuplicateRoutine { name: Box<str> },
    InferenceDidNotConverge { iterations: usize },
}
impl std::fmt::Display for GenericError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SpecializationLimit { .. } => {
                f.write_str("generic specialization limit exceeded")
            }
            Self::TypeArity { .. } => f.write_str("wrong number of type arguments"),
            Self::UnknownRecord { .. } => f.write_str("unknown generic record"),
            Self::UnknownRoutine { .. } => f.write_str("unknown generic routine"),
            Self::ArgumentArity { .. } => f.write_str("wrong number of generic routine arguments"),
            Self::IncompatibleArguments => f.write_str("generic arguments do not agree"),
            Self::DuplicateRecord { .. } => f.write_str("duplicate generic record"),
            Self::DuplicateRoutine { .. } => f.write_str("duplicate generic routine"),
            Self::InferenceDidNotConverge { .. } => {
                f.write_str("generic inference did not converge")
            }
        }
    }
}
impl std::error::Error for GenericError {}
fn invalid_generic(error: GenericError, span: Span) -> Diagnostic {
    Diagnostic::new("E151", error.to_string(), span)
}
fn walk_type(t: &mut Type, f: &mut impl FnMut(&mut Type)) {
    cyallo_ast::with_stack(|| {
        match t {
            Type::Enum(e) => {
                let e = std::sync::Arc::make_mut(e);
                for t in &mut e.identity.arguments {
                    walk_type(t, f);
                }
                for v in &mut e.variants {
                    if let Some(t) = &mut v.payload {
                        walk_type(t, f);
                    }
                }
            }
            Type::EnumRef(identity, _) => {
                for t in &mut identity.arguments {
                    walk_type(t, f);
                }
            }
            Type::Reference { pointee: t, .. }
            | Type::Managed { pointee: t, .. }
            | Type::Slice(t)
            | Type::Array(_, t)
            | Type::SparseArray(t)
            | Type::Borrowed(t)
            | Type::ServiceKey(t)
            | Type::ServiceRef(t)
            | Type::Storage(_, t) => walk_type(t, f),
            Type::Tuple(ts) | Type::Applied(_, ts) | Type::Opaque(_, ts, _) => {
                for t in ts {
                    walk_type(t, f)
                }
            }
            Type::AnonymousRecord(fs) | Type::Record(_, fs) | Type::Object(fs) => {
                for (_, t) in fs {
                    walk_type(t, f)
                }
            }
            Type::Function(_, ps, r) => {
                for (t, _) in ps {
                    walk_type(t, f)
                }
                walk_type(r, f)
            }
            _ => {}
        }
        f(t)
    })
}
fn canonical(t: &Type) -> Type {
    let mut t = t.clone();
    walk_type(&mut t, &mut |t| match t {
        Type::Enum(e) => *t = Type::Named(e.name.clone()),
        Type::EnumRef(_, name) => *t = Type::Named(name.clone()),
        Type::Record(n, _) | Type::RecordRef(n) => *t = Type::Named(key(n).into()),
        Type::Named(n) | Type::TraitObject(n) => *n = key(n).into(),
        _ => {}
    });
    t
}
fn substitute(t: &mut Type, bindings: &HashMap<String, Type>) {
    walk_type(t, &mut |t| {
        if let Type::Named(n) = t
            && let Some(value) = bindings.get(&key(n))
        {
            *t = value.clone()
        }
    })
}
fn substitute_body(body: &mut [Stmt], bindings: &HashMap<String, Type>) -> Result<()> {
    crate::composites::visit_block(
        body,
        &mut |e| {
            match &mut e.kind {
                ExprKind::Cast(_, t) => substitute(t, bindings),
                ExprKind::IntrinsicCall(_, ts, _)
                | ExprKind::GenericCall(_, ts, _)
                | ExprKind::GenericRecord(_, ts, _) => {
                    for t in ts {
                        substitute(t, bindings)
                    }
                }
                ExprKind::Closure(f, _) => substitute_function(f, bindings)?,
                _ => {}
            }
            Ok(())
        },
        &mut |t, _| {
            substitute(t, bindings);
            Ok(())
        },
    )
}
fn substitute_function(f: &mut Function, bindings: &HashMap<String, Type>) -> Result<()> {
    for p in &mut f.params {
        substitute(&mut p.ty, bindings)
    }
    substitute(&mut f.result, bindings);
    substitute_body(&mut f.body, bindings)
}
struct Specializer {
    enumerations: HashMap<String, EnumDeclaration>,
    generated_enums: Vec<EnumDeclaration>,
    owners: HashMap<String, usize>,
    records: HashMap<String, Record>,
    functions: HashMap<String, Function>,
    record_arguments: HashMap<String, (String, Vec<Type>)>,
    generated_records: Vec<Record>,
    generated_functions: Vec<Function>,
    function_instances: HashMap<String, Box<str>>,
    instances: usize,
}
impl Specializer {
    fn reserve(&mut self, span: Span) -> Result<()> {
        self.instances += 1;
        if self.instances > 256 {
            Err(invalid_generic(
                GenericError::SpecializationLimit { limit: 256 },
                span,
            ))
        } else {
            Ok(())
        }
    }
    fn bindings(params: &[Box<str>], args: &[Type], span: Span) -> Result<HashMap<String, Type>> {
        if params.len() != args.len() {
            return Err(invalid_generic(
                GenericError::TypeArity {
                    expected: params.len(),
                    actual: args.len(),
                },
                span,
            ));
        }
        Ok(params
            .iter()
            .zip(args)
            .map(|(n, t)| (key(n), canonical(t)))
            .collect())
    }
    fn record(&mut self, name: &str, args: &[Type], span: Span) -> Result<Box<str>> {
        if self.enumerations.contains_key(name) {
            return self.enumeration(name, args, span);
        }
        let args: Vec<_> = args.iter().map(canonical).collect();
        let id = format!("$generic${}<{args:?}>", key(name));
        if self.record_arguments.contains_key(&key(&id)) {
            return Ok(id.into());
        }
        let mut record = self.records.get(&key(name)).cloned().ok_or_else(|| {
            invalid_generic(GenericError::UnknownRecord { name: name.into() }, span)
        })?;
        let bindings = Self::bindings(&record.type_params, &args, span)?;
        self.reserve(span)?;
        self.record_arguments
            .insert(key(&id), (key(name), args.clone()));
        for (_, t) in &mut record.fields {
            substitute(t, &bindings);
            self.ty(t, span)?
        }
        if let Some(&owner) = self.owners.get(name) {
            self.owners.insert(id.clone(), owner);
        }
        record.name = id.clone().into();
        record.type_params.clear();
        self.generated_records.push(record);
        Ok(id.into())
    }
    fn function(&mut self, name: &str, args: &[Type], span: Span) -> Result<Box<str>> {
        let args: Vec<_> = args.iter().map(canonical).collect();
        let id = format!("$generic${}<{args:?}>", key(name));
        if let Some(name) = self.function_instances.get(&id) {
            return Ok(name.clone());
        }
        let mut f = self.functions.get(&key(name)).cloned().ok_or_else(|| {
            invalid_generic(GenericError::UnknownRoutine { name: name.into() }, span)
        })?;
        let bindings = Self::bindings(&f.type_params, &args, span)?;
        self.reserve(span)?;
        self.function_instances
            .insert(id.clone(), id.clone().into());
        if let Some(&owner) = self.owners.get(name) {
            self.owners.insert(id.clone(), owner);
        }
        f.type_params.clear();
        f.name = id.clone().into();
        substitute_function(&mut f, &bindings)?;
        self.generated_functions.push(f);
        Ok(id.into())
    }
    fn ty(&mut self, t: &mut Type, span: Span) -> Result<()> {
        cyallo_ast::with_stack(|| {
            match t {
                Type::Applied(n, ts) => {
                    for t in ts.iter_mut() {
                        self.ty(t, span)?
                    }
                    let n = self.record(n, ts, span)?;
                    *t = Type::Named(n);
                }
                Type::Reference { pointee: t, .. }
                | Type::Managed { pointee: t, .. }
                | Type::Slice(t)
                | Type::Array(_, t)
                | Type::SparseArray(t)
                | Type::Borrowed(t)
                | Type::ServiceKey(t)
                | Type::ServiceRef(t)
                | Type::Storage(_, t) => self.ty(t, span)?,
                Type::Tuple(ts) | Type::Opaque(_, ts, _) => {
                    for t in ts {
                        self.ty(t, span)?
                    }
                }
                Type::AnonymousRecord(fs) | Type::Record(_, fs) | Type::Object(fs) => {
                    for (_, t) in fs {
                        self.ty(t, span)?
                    }
                }
                Type::Function(_, ps, r) => {
                    for (t, _) in ps {
                        self.ty(t, span)?
                    }
                    self.ty(r, span)?
                }
                _ => {}
            }
            Ok(())
        })
    }
    fn explicit_function(&mut self, f: &mut Function) -> Result<()> {
        for p in &mut f.params {
            self.ty(&mut p.ty, p.span)?
        }
        self.ty(&mut f.result, f.span)?;
        self.constructor_context(&mut f.body, Some(&f.result))?;
        self.explicit_body(&mut f.body)
    }
    fn explicit_body(&mut self, body: &mut [Stmt]) -> Result<()> {
        crate::composites::visit_block(body, &mut |_| Ok(()), &mut |t, s| self.ty(t, s))?;
        crate::composites::visit_block(
            body,
            &mut |e| {
                match &mut e.kind {
                    ExprKind::GenericCall(n, ts, args) => {
                        for t in ts.iter_mut() {
                            self.ty(t, e.span)?
                        }
                        let n = self.function(n, ts, e.span)?;
                        e.kind = ExprKind::Call(n, std::mem::take(args));
                    }
                    ExprKind::GenericRecord(n, ts, fields) => {
                        for t in ts.iter_mut() {
                            self.ty(t, e.span)?
                        }
                        let n = self.record(n, ts, e.span)?;
                        e.kind = ExprKind::Record(n, std::mem::take(fields));
                    }
                    ExprKind::Cast(_, t) => self.ty(t, e.span)?,
                    ExprKind::IntrinsicCall(_, types, _) => {
                        for ty in types {
                            self.ty(ty, e.span)?;
                        }
                    }
                    ExprKind::Closure(f, _) => self.explicit_function(f)?,
                    _ => {}
                }
                Ok(())
            },
            &mut |_, _| Ok(()),
        )
    }
    fn unify(
        &self,
        pattern: &Type,
        actual: &Type,
        params: &[Box<str>],
        bindings: &mut HashMap<String, Type>,
    ) -> bool {
        if let Type::Named(n) = pattern
            && params.iter().any(|p| p.eq_identifier(n))
        {
            let actual = canonical(actual);
            return match bindings.get(&key(n)) {
                Some(old) => *old == actual,
                None => {
                    bindings.insert(key(n), actual);
                    true
                }
            };
        }
        match (pattern, actual) {
            (Type::Applied(name, arguments), Type::AnonymousRecord(fields))
                if self.records.contains_key(name.as_ref()) =>
            {
                let record = &self.records[name.as_ref()];
                let Ok(substitutions) =
                    Self::bindings(&record.type_params, arguments, Span::default())
                else {
                    return false;
                };
                let mut expected = Type::AnonymousRecord(record.fields.clone());
                substitute(&mut expected, &substitutions);
                if let Type::AnonymousRecord(fields) = &mut expected {
                    fields.sort_by(|(a, _), (b, _)| a.cmp(b));
                }
                self.unify(
                    &expected,
                    &Type::AnonymousRecord(fields.clone()),
                    params,
                    bindings,
                )
            }
            (
                Type::Managed {
                    kind: p_kind,
                    locality: p_locality,
                    lifetime: p_lifetime,
                    pointee: p,
                },
                Type::Managed {
                    kind: a_kind,
                    locality: a_locality,
                    lifetime: a_lifetime,
                    pointee: a,
                },
            ) if p_kind == a_kind
                && p_locality == a_locality
                && p_lifetime.is_some() == a_lifetime.is_some() =>
            {
                self.unify(p, a, params, bindings)
            }
            (Type::Applied(..), Type::Enum(actual)) => {
                self.unify(pattern, &Type::Named(actual.name.clone()), params, bindings)
            }
            (Type::Applied(n, ps), Type::Applied(m, args)) if n == m && ps.len() == args.len() => {
                ps.iter()
                    .zip(args)
                    .all(|(p, a)| self.unify(p, a, params, bindings))
            }
            (Type::Applied(n, ps), Type::Record(an, _))
            | (Type::Applied(n, ps), Type::Named(an)) => self
                .record_arguments
                .get(&key(an))
                .is_some_and(|(name, ts)| {
                    key(n) == *name
                        && ps.len() == ts.len()
                        && ps
                            .iter()
                            .zip(ts)
                            .all(|(p, a)| self.unify(p, a, params, bindings))
                }),
            (Type::Storage(n, p), Type::Storage(m, a)) if n == m => {
                self.unify(p, a, params, bindings)
            }
            // The marker and the declared enum denote the same core type.
            (Type::Storage(..), Type::Enum(_)) | (Type::Enum(_), Type::Storage(..)) => {
                match (
                    crate::enums::core_shape(pattern),
                    crate::enums::core_shape(actual),
                ) {
                    (Some((n, ps)), Some((m, args))) if n == m && ps.len() == args.len() => ps
                        .iter()
                        .zip(&args)
                        .all(|(p, a)| self.unify(p, a, params, bindings)),
                    _ => false,
                }
            }
            (Type::Opaque(n, ps, _), Type::Opaque(m, args, _))
                if n == m && ps.len() == args.len() =>
            {
                ps.iter()
                    .zip(args)
                    .all(|(p, a)| self.unify(p, a, params, bindings))
            }
            (
                Type::Reference {
                    mutable: p_mut,
                    pointee: p,
                    ..
                },
                Type::Reference {
                    mutable: a_mut,
                    pointee: a,
                    ..
                },
            ) if !p_mut || *a_mut => match (p.as_ref(), a.as_ref()) {
                (Type::Slice(p), Type::Array(_, a)) => self.unify(p, a, params, bindings),
                (Type::Slice(p), other)
                    if !other.is_double_ended() && other.collection_element().is_some() =>
                {
                    self.unify(p, other.collection_element().unwrap(), params, bindings)
                }
                _ => self.unify(p, a, params, bindings),
            },
            (Type::Slice(p), Type::Slice(a)) => self.unify(p, a, params, bindings),
            (Type::SparseArray(p), Type::SparseArray(a)) => self.unify(p, a, params, bindings),
            (Type::Array(pb, p), Type::Array(ab, a)) if pb == ab => {
                self.unify(p, a, params, bindings)
            }
            (Type::Borrowed(p), Type::Borrowed(a))
            | (Type::ServiceKey(p), Type::ServiceKey(a))
            | (Type::ServiceRef(p), Type::ServiceRef(a)) => self.unify(p, a, params, bindings),
            (Type::Function(pk, ps, pr), Type::Function(ak, ts, ar))
                if (pk == ak
                    || (*pk == RoutineKind::Procedure && *ak == RoutineKind::Function))
                    && ps.len() == ts.len() =>
            {
                ps.iter().zip(ts).all(|((p, pref), (a, aref))| {
                    pref == aref && self.unify(p, a, params, bindings)
                }) && self.unify(pr, ar, params, bindings)
            }
            (Type::AnonymousRecord(ps), Type::AnonymousRecord(ts)) if ps.len() == ts.len() => ps
                .iter()
                .zip(ts)
                .all(|((pn, p), (tn, t))| pn == tn && self.unify(p, t, params, bindings)),
            (Type::Tuple(ps), Type::Tuple(ts)) if ps.len() == ts.len() => ps
                .iter()
                .zip(ts)
                .all(|(p, a)| self.unify(p, a, params, bindings)),
            _ => crate::declarations::equivalent_types(&canonical(pattern), &canonical(actual)),
        }
    }
    fn infer_body(
        &mut self,
        body: &mut [Stmt],
        known: &HashMap<String, Type>,
        routines: &crate::declarations::TypeEnvironment,
    ) -> Result<bool> {
        let mut changed = false;
        crate::composites::visit_block(
            body,
            &mut |e| {
                match &mut e.kind {
                    ExprKind::Invoke(callee,args) => {
                        if let ExprKind::Field(base,name)|ExprKind::ExtensionField(base,name,_)=&callee.kind
                            && let Ok(receiver)=crate::inference::expression_type(base,known,routines)
                            && !matches!(receiver.pointee_or_self(),Type::Record(_,fields)|Type::AnonymousRecord(fields) if fields.iter().any(|(field,_)|field==name))
                            && let Some(parameters)=crate::native_contract::parameter_types(routines,&receiver,name,e.span)
                        {
                            for (argument,expected) in args.iter_mut().zip(parameters){self.context_expression(argument,Some(&expected))?;}
                        }
                    }
                    ExprKind::Binary(Operator::Equal | Operator::NotEqual, left, right) => {
                        if let Ok(ty) = crate::inference::expression_type(left, known, routines) {
                            self.context_expression(right, Some(&ty))?;
                        }
                        if let Ok(ty) = crate::inference::expression_type(right, known, routines) {
                            self.context_expression(left, Some(&ty))?;
                        }
                    }
                    ExprKind::Call(name, args) if !self.functions.contains_key(name.as_ref()) => {
                        if let Some(Type::Function(_, params, _)) = routines.get(name.as_ref()) {
                            for (arg, (ty, _)) in args.iter_mut().zip(params) {
                                self.context_expression(arg, Some(ty))?;
                            }
                        }
                    }
                    _ => {}
                }
                if let ExprKind::Closure(f, _) = &mut e.kind {
                    let mut scope = known.clone();
                    scope.extend(f.params.iter().map(|p| (key(&p.name), p.ty.clone())));
                    changed |= self.infer_body(&mut f.body, &scope, routines)?;
                }
                if let ExprKind::Call(n, args) = &mut e.kind
                    && let Some(template) = self.functions.get(&key(n)).cloned()
                {
                    if args.len() != template.params.len() {
                        return Err(invalid_generic(
                            GenericError::ArgumentArity {
                                expected: template.params.len(),
                                actual: args.len(),
                            },
                            e.span,
                        ));
                    }
                    let actual = args
                        .iter()
                        .map(|a| crate::inference::expression_type(a, known, routines))
                        .collect::<Result<Vec<_>>>();
                    if let Ok(actual) = actual {
                        let mut bindings = HashMap::new();
                        let mut matches = true;
                        // Resolve ordinary operands first so callback inputs
                        // can use the generic types they establish.
                        for (parameter, actual) in template.params.iter().zip(&actual) {
                            if matches!(
                                (&parameter.ty, actual),
                                (Type::Function(..), Type::NativeCallable(_))
                            ) {
                                continue;
                            }
                            matches &= self.unify(
                                &parameter.ty,
                                actual,
                                &template.type_params,
                                &mut bindings,
                            );
                        }
                        for (parameter, actual) in template.params.iter().zip(&actual) {
                            if let (Type::Function(..), Type::NativeCallable(declaration)) =
                                (&parameter.ty, actual)
                            {
                                let mut expected = parameter.ty.clone();
                                substitute(&mut expected, &bindings);
                                let callback = crate::native_contract::as_callable(
                                    routines,
                                    *declaration,
                                    &expected,
                                    e.span,
                                )?;
                                matches &= self.unify(
                                    &parameter.ty,
                                    &callback.callable_type,
                                    &template.type_params,
                                    &mut bindings,
                                );
                            }
                        }
                        if !matches {
                            return Err(invalid_generic(
                                GenericError::IncompatibleArguments,
                                e.span,
                            ));
                        }
                        if let Some(ts) = template
                            .type_params
                            .iter()
                            .map(|p| bindings.get(&key(p)).cloned())
                            .collect::<Option<Vec<_>>>()
                        {
                            *n = self.function(n, &ts, e.span)?;
                            changed = true;
                        }
                    }
                }
                Ok(())
            },
            &mut |_, _| Ok(()),
        )?;
        Ok(changed)
    }
}
pub fn specialize(ast: &mut Ast, options: crate::CompileOptions) -> Result<()> {
    let native = ast.native.clone();
    specialize_modules(std::slice::from_mut(ast), &native, options)
}

/// Instantiate declarations in their defining modules. The shared catalogue
/// contains generic templates and signatures, never a combined executable AST.
pub fn specialize_modules(
    asts: &mut [Ast],
    native: &NativeDeclarations,
    options: crate::CompileOptions,
) -> Result<()> {
    crate::enums::validate_templates(asts)?;
    if !asts.iter().any(|a| {
        a.enums.iter().any(|e| !e.type_params.is_empty())
            || a.records.iter().any(|r| !r.type_params.is_empty())
            || a.functions.iter().any(|f| !f.type_params.is_empty())
    }) {
        return Ok(());
    }
    let mut s = Specializer {
        enumerations: HashMap::new(),
        generated_enums: vec![],
        owners: HashMap::new(),
        records: HashMap::new(),
        functions: HashMap::new(),
        record_arguments: HashMap::new(),
        generated_records: vec![],
        generated_functions: vec![],
        function_instances: HashMap::new(),
        instances: 0,
    };
    let mut owners = HashMap::new();
    for (owner, ast) in asts.iter_mut().enumerate() {
        ast.enums.retain(|e| {
            if e.type_params.is_empty() {
                true
            } else {
                owners.insert(e.name.to_string(), owner);
                s.enumerations.insert(e.name.to_string(), e.clone());
                false
            }
        });
        ast.records.retain(|r| {
            if r.type_params.is_empty() {
                true
            } else {
                owners.insert(r.name.to_string(), owner);
                s.records.insert(key(&r.name), r.clone());
                false
            }
        });
        ast.functions.retain(|f| {
            if f.type_params.is_empty() {
                true
            } else {
                owners.insert(f.name.to_string(), owner);
                s.functions.insert(key(&f.name), f.clone());
                false
            }
        });
    }
    // Generation records their template identity independently of instance names.
    s.owners = owners;
    for ast in asts.iter_mut() {
        for declaration in &mut ast.enums {
            for variant in &mut declaration.variants {
                if let Some(payload) = &mut variant.payload {
                    s.ty(payload, variant.span)?;
                }
            }
        }
        for r in &mut ast.records {
            for (_, ty) in &mut r.fields {
                s.ty(ty, r.span)?
            }
        }
        s.constructor_context(&mut ast.body, None)?;
        s.explicit_body(&mut ast.body)?;
        for f in &mut ast.functions {
            s.explicit_function(f)?
        }
        for t in &mut ast.traits {
            for f in &mut t.methods {
                s.explicit_function(f)?
            }
        }
        for imp in &mut ast.implementations {
            s.ty(&mut imp.receiver, imp.span)?;
            for f in &mut imp.methods {
                s.explicit_function(f)?
            }
        }
        for property in &mut ast.properties {
            s.ty(&mut property.receiver, property.span)?;
            s.explicit_function(&mut property.getter)?
        }
    }
    for _ in 0..256 {
        while !s.generated_functions.is_empty() {
            let fs = std::mem::take(&mut s.generated_functions);
            for mut f in fs {
                s.explicit_function(&mut f)?;
                let owner = s.owners[&f.name.to_string()];
                asts[owner].functions.push(f);
            }
        }
        for e in std::mem::take(&mut s.generated_enums) {
            let owner = s.owners[&e.name.to_string()];
            asts[owner].enums.push(e);
        }
        for r in std::mem::take(&mut s.generated_records) {
            let owner = s.owners[&r.name.to_string()];
            asts[owner].records.push(r);
        }
        let mut types = crate::declarations::TypeEnvironment::new(
            crate::composites::resolve_modules(asts, &native)?,
            native.clone(),
        );
        for ast in asts.iter() {
            types.extend(
                ast.functions
                    .iter()
                    .map(|f| (key(&f.name), crate::composites::function_type(f))),
            );
            for p in &ast.properties {
                types.insert(
                    key(&p.getter.name),
                    crate::composites::function_type(&p.getter),
                );
            }
            for t in &ast.traits {
                for m in &t.methods {
                    types.insert(
                        format!("$METHOD${}.{}", key(&t.name), key(&m.name)),
                        crate::composites::function_type(m),
                    );
                }
            }
        }
        let globals = crate::module_graph::infer_globals(asts, &types, options)?;
        let known: HashMap<_, _> = globals
            .iter()
            .flatten()
            .map(|l| (key(&l.name), l.ty.clone()))
            .collect();
        let imported: Vec<_> = globals.iter().flatten().cloned().collect();
        let mut changed = false;
        for ast in asts.iter_mut() {
            changed |= s.infer_body(&mut ast.body, &known, &types)?;
            let mut infer = |f: &mut Function| -> Result<()> {
                let mut locals = crate::analysis::collect(&f.body, &f.params)?;
                let _ = crate::inference::infer(&f.body, &mut locals, &imported, &types, options);
                let mut scope = known.clone();
                scope.extend(locals.iter().map(|l| (key(&l.name), l.ty.clone())));
                changed |= s.infer_body(&mut f.body, &scope, &types)?;
                Ok(())
            };
            for f in &mut ast.functions {
                infer(f)?
            }
            for imp in &mut ast.implementations {
                for f in &mut imp.methods {
                    infer(f)?
                }
            }
            for p in &mut ast.properties {
                infer(&mut p.getter)?
            }
        }
        if !changed && s.generated_functions.is_empty() {
            return Ok(());
        }
    }
    Err(invalid_generic(
        GenericError::InferenceDidNotConverge { iterations: 256 },
        Span::default(),
    ))
}
