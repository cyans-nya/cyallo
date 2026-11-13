//! Immutable service interfaces shared by all execution tiers.
use cyallo_ast::IdentifierEq as _;
use cyallo_ast::{RoutineKind, Type};
#[derive(Debug, Clone, Default)]
pub struct Metadata {
    pub extensions: Vec<Extension>,
    pub traits: Vec<Trait>,
    pub implementations: Vec<Implementation>,
    pub keys: Vec<Key>,
}
#[derive(Debug, Clone)]
pub struct Extension {
    pub name: Box<str>,
    pub receiver: Type,
    pub result: Type,
}
#[derive(Debug, Clone)]
pub struct Trait {
    pub origin: cyallo_ast::TraitOrigin,
    pub name: Box<str>,
    pub methods: Vec<Method>,
}
#[derive(Debug, Clone)]
pub struct Method {
    pub name: Box<str>,
    pub kind: RoutineKind,
    pub params: Vec<(Type, bool)>,
    pub result: Type,
    pub receiver: Receiver,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Receiver {
    Value,
    Shared,
    Mutable,
}
fn receiver(ty: &Type) -> Receiver {
    match ty {
        Type::Reference { mutable: true, .. } => Receiver::Mutable,
        Type::Reference { .. } => Receiver::Shared,
        _ => Receiver::Value,
    }
}
#[derive(Debug, Clone)]
pub struct Implementation {
    pub trait_id: usize,
    pub receiver: Type,
    pub methods: Vec<usize>,
}
#[derive(Debug, Clone)]
pub struct Key {
    pub span: u32,
    pub label: Box<str>,
    pub ty: Type,
}
impl Metadata {
    pub fn concrete_method(&self, receiver: &Type, name: &str) -> Option<(usize, usize)> {
        let mut found = None;
        for (i, imp) in self
            .implementations
            .iter()
            .enumerate()
            .filter(|(_, i)| &i.receiver == receiver)
        {
            if let Some(m) = self.traits[imp.trait_id]
                .methods
                .iter()
                .position(|m| m.name.eq_identifier(name))
            {
                if found.is_some() {
                    return None;
                }
                found = Some((i, m));
            }
        }
        found
    }
    pub fn allocator(&self, ty: &Type) -> Option<&Implementation> {
        self.implementations.iter().find(|implementation| {
            self.traits[implementation.trait_id].origin == cyallo_ast::TraitOrigin::Allocator
                && implementation.receiver == *ty
        })
    }
    pub fn trait_id(&self, name: &str) -> Option<usize> {
        self.traits.iter().position(|t| t.name.eq_identifier(name))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Context,
    Extend,
    Isolate,
    Provide,
    Reference,
    Dispose,
    Inject,
    OnDispose,
    Available,
}

pub fn prepare(ast: &mut cyallo_ast::Ast) -> Result<Metadata, cyallo_ast::Diagnostic> {
    use cyallo_ast::*;
    let mut out = Metadata::default();
    for t in &ast.traits {
        if out.trait_id(&t.name).is_some() {
            return Err(Diagnostic::new("E130", "duplicate trait", t.span));
        }
        let mut methods = vec![];
        for f in &t.methods {
            let Some(receiver) = f.params.first().filter(|p| p.receiver) else {
                return Err(Diagnostic::new(
                    "E130",
                    "trait methods require a self receiver",
                    f.span,
                ));
            };
            if methods
                .iter()
                .any(|m: &Method| m.name.eq_identifier(&f.name))
            {
                return Err(Diagnostic::new("E130", "duplicate trait method", f.span));
            }
            methods.push(Method {
                name: f.name.clone(),
                kind: f.kind,
                params: f.params[1..]
                    .iter()
                    .map(|p| (p.ty.clone(), p.by_ref))
                    .collect(),
                result: f.result.clone(),
                receiver: self::receiver(&receiver.ty),
            });
        }
        out.traits.push(crate::services::Trait {
            origin: t.origin,
            name: t.name.clone(),
            methods,
        });
    }
    for (index, implementation) in ast.implementations.drain(..).enumerate() {
        let trait_id = out
            .trait_id(&implementation.trait_name)
            .ok_or_else(|| Diagnostic::new("E130", "unknown trait", implementation.span))?;
        if out
            .implementations
            .iter()
            .any(|i| i.trait_id == trait_id && i.receiver == implementation.receiver)
        {
            return Err(Diagnostic::new(
                "E130",
                "duplicate trait implementation",
                implementation.span,
            ));
        }
        if implementation.methods.len() != out.traits[trait_id].methods.len() {
            return Err(Diagnostic::new(
                "E130",
                "implementation must define every trait method exactly once",
                implementation.span,
            ));
        }
        let mut methods = vec![];
        for declared in &out.traits[trait_id].methods {
            let found: Vec<_> = implementation
                .methods
                .iter()
                .filter(|f| f.name.eq_identifier(&declared.name))
                .collect();
            if found.len() != 1 {
                return Err(Diagnostic::new(
                    "E130",
                    "missing or duplicate trait method",
                    implementation.span,
                ));
            }
            let mut f = found[0].clone();
            let params: Vec<_> = f
                .params
                .iter()
                .skip(1)
                .map(|p| (p.ty.clone(), p.by_ref))
                .collect();
            if f.kind != declared.kind
                || f.result != declared.result
                || params != declared.params
                || f.params
                    .first()
                    .is_none_or(|p| !p.receiver || receiver(&p.ty) != declared.receiver)
            {
                return Err(Diagnostic::new(
                    "E130",
                    "trait implementation signature differs from declaration",
                    f.span,
                ));
            }
            f.origin = cyallo_ast::RoutineOrigin::Implementation;
            f.name = format!("$impl{index}${}", declared.name).into();
            methods.push(ast.functions.len() + 1);
            ast.functions.push(f);
        }
        out.implementations.push(crate::services::Implementation {
            trait_id,
            receiver: implementation.receiver,
            methods,
        });
    }
    for property in ast.properties.drain(..) {
        out.extensions.push(Extension {
            name: property.getter.name.clone(),
            receiver: property.receiver,
            result: property.getter.result.clone(),
        });
        ast.functions.push(property.getter);
    }
    fn validate(t: &Type, metadata: &Metadata, span: cyallo_ast::Span) -> Result<(), Diagnostic> {
        match t {
            Type::TraitObject(name) if metadata.trait_id(name).is_none() => {
                return Err(Diagnostic::new(
                    "E130",
                    format!("unknown trait `{}`", cyallo_ast::source_name(name)),
                    span,
                ));
            }
            Type::ServiceKey(inner) | Type::ServiceRef(inner) => {
                if !matches!(inner.as_ref(), Type::TraitObject(_)) {
                    return Err(Diagnostic::new(
                        "E131",
                        "service types require a dyn trait interface",
                        span,
                    ));
                }
                validate(inner, metadata, span)?;
            }
            Type::Array(_, t) | Type::SparseArray(t) | Type::Borrowed(t) | Type::Storage(_, t) => {
                validate(t, metadata, span)?
            }
            Type::Tuple(ts) => {
                for t in ts {
                    validate(t, metadata, span)?;
                }
            }
            Type::Record(_, fs) => {
                for (_, t) in fs {
                    validate(t, metadata, span)?;
                }
            }
            Type::Function(_, ps, r) => {
                for (t, _) in ps {
                    validate(t, metadata, span)?;
                }
                validate(r, metadata, span)?;
            }
            _ => {}
        }
        Ok(())
    }
    for t in &out.traits {
        for m in &t.methods {
            for (ty, _) in &m.params {
                validate(ty, &out, cyallo_ast::Span::default())?;
            }
            validate(&m.result, &out, cyallo_ast::Span::default())?;
        }
    }
    for f in &ast.functions {
        for p in &f.params {
            validate(&p.ty, &out, p.span)?;
        }
        validate(&f.result, &out, f.span)?;
    }
    cyallo_ast::visit::visit_block(&mut ast.body, &mut |_| Ok(()), &mut |t, span| {
        validate(t, &out, span)
    })?;
    fn scan(body: &mut [Stmt], out: &mut Metadata) -> Result<(), Diagnostic> {
        cyallo_ast::visit::visit_block(
            body,
            &mut |e| {
                if let ExprKind::Closure(f, _) = &mut e.kind {
                    scan(&mut f.body, out)?;
                }
                if let ExprKind::IntrinsicCall(id, types, args) = &e.kind
                    && *id == crate::intrinsic::Intrinsic::ServiceKeyNew.id()
                {
                    let [ty @ Type::TraitObject(_)] = types.as_slice() else {
                        return Err(Diagnostic::new(
                            "E131",
                            "service key requires a dyn trait type",
                            e.span,
                        ));
                    };
                    let label = args
                        .first()
                        .filter(|_| args.len() == 1)
                        .and_then(cyallo_ast::constant_text)
                        .ok_or_else(|| {
                            Diagnostic::new(
                                "E131",
                                "ServiceKey requires a compile-time string diagnostic label",
                                e.span,
                            )
                        })?;
                    if !out.keys.iter().any(|key| key.span == e.span.start) {
                        out.keys.push(Key {
                            span: e.span.start,
                            label: label.into(),
                            ty: ty.clone(),
                        });
                    }
                }
                Ok(())
            },
            &mut |_, _| Ok(()),
        )
    }
    scan(&mut ast.body, &mut out)?;
    for f in &mut ast.functions {
        scan(&mut f.body, &mut out)?;
    }
    Ok(out)
}

pub fn intrinsic(receiver: &Type, name: &str, args: &[Type]) -> Option<(Operation, Type)> {
    use Operation::*;
    let n = name;
    match (receiver.pointee_or_self(), n, args) {
        (Type::Context, "extend", []) => Some((Extend, Type::Context)),
        (Type::Context, "isolate", [Type::ServiceKey(_)]) => Some((Isolate, Type::Context)),
        (Type::Context, "service", [Type::ServiceKey(t)]) => {
            Some((Reference, Type::ServiceRef(t.clone())))
        }
        (Type::Context, "provide", [Type::ServiceKey(t), value]) if t.as_ref() == value => {
            Some((Provide, Type::Registration))
        }
        (Type::Context, "available", [Type::ServiceKey(_)]) => Some((Available, Type::Boolean)),
        (Type::Context | Type::Registration, "dispose", []) => Some((Dispose, Type::Void)),
        (
            Type::Context,
            "inject",
            [
                Type::ServiceKey(_),
                Type::Function(RoutineKind::Procedure, p, r),
            ],
        ) if p == &[(Type::Context, false)] && r.as_ref() == &Type::Void => {
            Some((Inject, Type::Void))
        }
        (Type::Context, "on_dispose", [Type::Function(RoutineKind::Procedure, p, r)])
            if p.is_empty() && r.as_ref() == &Type::Void =>
        {
            Some((OnDispose, Type::Void))
        }
        _ => None,
    }
}
