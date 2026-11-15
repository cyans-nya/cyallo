//! Monomorphization of records and routines before ordinary checked lowering.
use cyallo_ast::IdentifierEq as _;
use cyallo_ast::*;
use std::collections::HashMap;
type Result<T> = std::result::Result<T, Diagnostic>;
fn key(s: &str) -> String {
    s.to_owned()
}
fn error(span: Span, message: &str) -> Diagnostic {
    Diagnostic::new("E151", message, span)
}
fn walk_type(t: &mut Type, f: &mut impl FnMut(&mut Type)) {
    match t {
        Type::Reference { pointee: t, .. }
        | Type::Managed { pointee: t, .. }
        | Type::Slice(t)
        | Type::Array(_, t)
        | Type::SparseArray(t)
        | Type::Borrowed(t)
        | Type::ServiceKey(t)
        | Type::ServiceRef(t)
        | Type::Storage(_, t) => walk_type(t, f),
        Type::Tuple(ts) | Type::Applied(_, ts) => {
            for t in ts {
                walk_type(t, f)
            }
        }
        Type::Record(_, fs) | Type::Object(fs) => {
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
}
fn canonical(t: &Type) -> Type {
    let mut t = t.clone();
    walk_type(&mut t, &mut |t| match t {
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
            Err(error(span, "generic specialization limit exceeded"))
        } else {
            Ok(())
        }
    }
    fn bindings(params: &[Box<str>], args: &[Type], span: Span) -> Result<HashMap<String, Type>> {
        if params.len() != args.len() {
            return Err(error(span, "wrong number of type arguments"));
        }
        Ok(params
            .iter()
            .zip(args)
            .map(|(n, t)| (key(n), canonical(t)))
            .collect())
    }
    fn record(&mut self, name: &str, args: &[Type], span: Span) -> Result<Box<str>> {
        let args: Vec<_> = args.iter().map(canonical).collect();
        let id = format!("$generic${}<{args:?}>", key(name));
        if self.record_arguments.contains_key(&key(&id)) {
            return Ok(id.into());
        }
        let mut record = self
            .records
            .get(&key(name))
            .cloned()
            .ok_or_else(|| error(span, "unknown generic record"))?;
        let bindings = Self::bindings(&record.type_params, &args, span)?;
        self.reserve(span)?;
        self.record_arguments
            .insert(key(&id), (key(name), args.clone()));
        for (_, t) in &mut record.fields {
            substitute(t, &bindings);
            self.ty(t, span)?
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
        let mut f = self
            .functions
            .get(&key(name))
            .cloned()
            .ok_or_else(|| error(span, "unknown generic routine"))?;
        let bindings = Self::bindings(&f.type_params, &args, span)?;
        self.reserve(span)?;
        self.function_instances
            .insert(id.clone(), id.clone().into());
        f.type_params.clear();
        f.name = id.clone().into();
        substitute_function(&mut f, &bindings)?;
        self.generated_functions.push(f);
        Ok(id.into())
    }
    fn ty(&mut self, t: &mut Type, span: Span) -> Result<()> {
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
            Type::Tuple(ts) => {
                for t in ts {
                    self.ty(t, span)?
                }
            }
            Type::Record(_, fs) | Type::Object(fs) => {
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
    }
    fn explicit_function(&mut self, f: &mut Function) -> Result<()> {
        for p in &mut f.params {
            self.ty(&mut p.ty, p.span)?
        }
        self.ty(&mut f.result, f.span)?;
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
                (Type::Slice(p), Type::Array(_, a) | Type::Storage(StorageKind::Vec, a)) => {
                    self.unify(p, a, params, bindings)
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
                if let ExprKind::Closure(f, _) = &mut e.kind {
                    let mut scope = known.clone();
                    scope.extend(f.params.iter().map(|p| (key(&p.name), p.ty.clone())));
                    changed |= self.infer_body(&mut f.body, &scope, routines)?;
                }
                if let ExprKind::Call(n, args) = &mut e.kind
                    && let Some(template) = self.functions.get(&key(n)).cloned()
                {
                    if args.len() != template.params.len() {
                        return Err(error(e.span, "wrong number of generic routine arguments"));
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
                            return Err(error(e.span, "generic arguments do not agree"));
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
    if !ast.records.iter().any(|r| !r.type_params.is_empty())
        && !ast.functions.iter().any(|f| !f.type_params.is_empty())
    {
        return Ok(());
    }
    let mut s = Specializer {
        records: HashMap::new(),
        functions: HashMap::new(),
        record_arguments: HashMap::new(),
        generated_records: vec![],
        generated_functions: vec![],
        function_instances: HashMap::new(),
        instances: 0,
    };
    let mut records = vec![];
    for r in std::mem::take(&mut ast.records) {
        if r.type_params.is_empty() {
            records.push(r)
        } else if s.records.insert(key(&r.name), r.clone()).is_some() {
            return Err(error(r.span, "duplicate generic record"));
        }
    }
    ast.records = records;
    let mut functions = vec![];
    for f in std::mem::take(&mut ast.functions) {
        if f.type_params.is_empty() {
            functions.push(f)
        } else if s.functions.insert(key(&f.name), f.clone()).is_some() {
            return Err(error(f.span, "duplicate generic routine"));
        }
    }
    ast.functions = functions;
    for r in &mut ast.records {
        for (_, t) in &mut r.fields {
            s.ty(t, r.span)?
        }
    }
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
    for prop in &mut ast.properties {
        s.ty(&mut prop.receiver, prop.span)?;
        s.explicit_function(&mut prop.getter)?
    }
    for _ in 0..256 {
        while !s.generated_functions.is_empty() {
            let mut fs = std::mem::take(&mut s.generated_functions);
            for f in &mut fs {
                s.explicit_function(f)?
            }
            ast.functions.extend(fs);
        }
        ast.records.append(&mut s.generated_records);
        let mut routines = crate::declarations::TypeEnvironment::new(
            crate::composites::resolve(ast)?,
            ast.native.clone(),
        );
        routines.extend(
            ast.functions
                .iter()
                .map(|f| (key(&f.name), crate::composites::function_type(f))),
        );
        for prop in &ast.properties {
            routines.insert(
                key(&prop.getter.name),
                crate::composites::function_type(&prop.getter),
            );
        }
        for t in &ast.traits {
            for m in &t.methods {
                routines.insert(
                    format!("$METHOD${}.{}", key(&t.name), key(&m.name)),
                    crate::composites::function_type(m),
                );
            }
        }
        let mut globals = crate::analysis::collect(&ast.body, &[])?;
        let _ = crate::inference::infer(&ast.body, &mut globals, &[], &routines, options);
        let known = globals
            .iter()
            .map(|l| (key(&l.name), l.ty.clone()))
            .collect();
        let mut changed = s.infer_body(&mut ast.body, &known, &routines)?;
        let mut infer_function = |f: &mut Function| -> Result<()> {
            let mut locals = crate::analysis::collect(&f.body, &f.params)?;
            let _ = crate::inference::infer(&f.body, &mut locals, &globals, &routines, options);
            let scope = globals
                .iter()
                .chain(&locals)
                .map(|l| (key(&l.name), l.ty.clone()))
                .collect();
            changed |= s.infer_body(&mut f.body, &scope, &routines)?;
            Ok(())
        };
        for f in &mut ast.functions {
            infer_function(f)?
        }
        for imp in &mut ast.implementations {
            for f in &mut imp.methods {
                infer_function(f)?
            }
        }
        for prop in &mut ast.properties {
            infer_function(&mut prop.getter)?
        }
        if !changed && s.generated_functions.is_empty() {
            return Ok(());
        }
    }
    Err(error(Span::default(), "generic inference did not converge"))
}
