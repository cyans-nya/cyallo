//! Checked patterns shared by inference, ownership and decision-tree lowering.
use crate::declarations::TypeEnvironment;
use cyallo_ast::*;
use std::collections::{BTreeMap, HashSet};
type Result<T> = std::result::Result<T, Diagnostic>;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternError {
    UnknownVariant { name: Box<str> },
    DuplicateBinding { name: Box<str> },
    AlternativeBindings,
    ExpectedEnum { actual: Type },
    WrongVariant { enumeration: Box<str> },
    PayloadArity,
    ExpectedTuple { actual: Type, fields: usize },
    ExpectedRecord { actual: Type },
    RecordFields,
    LiteralType { expected: Type, actual: Type },
    GuardType { actual: Type },
    ArmType { expected: Type, actual: Type },
    NonExhaustive { missing: Vec<Box<str>> },
    OpenEnumWildcard,
    ComplexityLimit,
    UnresolvedPattern,
    GuardMove,
    GuardMutation,
}
impl std::fmt::Display for PatternError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownVariant { name } => {
                write!(f, "unknown enum variant `{}`", source_name(name))
            }
            Self::DuplicateBinding { name } => {
                write!(f, "duplicate pattern binding `{}`", source_name(name))
            }
            Self::AlternativeBindings => {
                f.write_str("pattern alternatives must bind the same names with the same types")
            }
            Self::ExpectedEnum { actual } => {
                write!(f, "variant pattern requires an enum, found `{actual}`")
            }
            Self::WrongVariant { enumeration } => write!(
                f,
                "variant does not belong to enum `{}`",
                source_name(enumeration)
            ),
            Self::PayloadArity => f.write_str("variant pattern must match its declared payload"),
            Self::ExpectedTuple { actual, fields } => write!(
                f,
                "pattern requires a {fields}-field tuple, found `{actual}`"
            ),
            Self::ExpectedRecord { actual } => {
                write!(f, "record pattern requires a record, found `{actual}`")
            }
            Self::RecordFields => f.write_str(
                "record pattern must name existing fields and cover every field or use `..`",
            ),
            Self::LiteralType { expected, actual } => write!(
                f,
                "pattern literal has type `{actual}`, expected `{expected}`"
            ),
            Self::GuardType { actual } => write!(f, "match guard must be bool, found `{actual}`"),
            Self::ArmType { expected, actual } => write!(
                f,
                "match arms must agree: expected `{expected}`, found `{actual}`"
            ),
            Self::NonExhaustive { missing } => {
                f.write_str("non-exhaustive match")?;
                if !missing.is_empty() {
                    write!(
                        f,
                        ": missing {}",
                        missing
                            .iter()
                            .map(|n| n.as_ref())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )?;
                }
                Ok(())
            }
            Self::OpenEnumWildcard => {
                f.write_str("matching an open enum from another package requires a wildcard arm")
            }
            Self::ComplexityLimit => {
                f.write_str("pattern coverage complexity limit exceeded; simplify the patterns")
            }
            Self::GuardMove => {
                f.write_str("a guard cannot move a matched value before its arm is selected")
            }
            Self::GuardMutation => {
                f.write_str("a guard cannot mutate matched storage before its arm is selected")
            }
            Self::UnresolvedPattern => f.write_str("pattern has not passed declaration resolution"),
        }
    }
}
impl std::error::Error for PatternError {}
pub fn invalid_pattern(error: PatternError, span: Span) -> Diagnostic {
    Diagnostic::new("E185", error.to_string(), span)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Value,
    Shared,
    Mutable,
}
impl Mode {
    pub fn split(ty: &Type) -> (Self, &Type) {
        match ty {
            Type::Reference {
                mutable, pointee, ..
            } => (
                if *mutable {
                    Self::Mutable
                } else {
                    Self::Shared
                },
                pointee,
            ),
            Type::Borrowed(ty) => (Self::Shared, ty),
            _ => (Self::Value, ty),
        }
    }
    pub fn binding(self, ty: &Type) -> Type {
        match self {
            Self::Value => ty.clone(),
            Self::Shared | Self::Mutable => Type::Reference {
                mutable: self == Self::Mutable,
                lifetime: None,
                pointee: Box::new(ty.clone()),
            },
        }
    }
}
#[derive(Debug, Clone)]
pub struct Binding {
    pub name: Box<str>,
    pub ty: Type,
    pub span: Span,
}
#[derive(Debug, Clone)]
pub struct Checked {
    pub ty: Type,
    pub kind: Kind,
}
#[derive(Debug, Clone)]
pub enum Kind {
    Wildcard,
    Binding(Binding),
    Variant(VariantId, Option<Box<Checked>>),
    Tuple(Vec<Checked>),
    Record(Vec<(usize, Checked)>),
    Literal(Literal),
    Or(Vec<Checked>),
}
impl Checked {
    pub fn bindings(&self) -> Vec<Binding> {
        let mut out = vec![];
        self.gather(&mut out);
        out
    }
    fn gather(&self, out: &mut Vec<Binding>) {
        cyallo_ast::with_stack(|| match &self.kind {
            Kind::Binding(binding) => out.push(binding.clone()),
            Kind::Variant(_, Some(payload)) => payload.gather(out),
            Kind::Tuple(values) => {
                for value in values {
                    value.gather(out)
                }
            }
            Kind::Record(fields) => {
                for (_, value) in fields {
                    value.gather(out)
                }
            }
            Kind::Or(values) => {
                if let Some(value) = values.first() {
                    value.gather(out)
                }
            }
            _ => {}
        })
    }
    pub fn consumes(&self) -> bool {
        self.bindings()
            .iter()
            .any(|b| !crate::ownership::copy(&b.ty))
    }
}
pub fn check(pattern: &Pattern, ty: &Type, env: &TypeEnvironment) -> Result<Checked> {
    let (mode, ty) = Mode::split(ty);
    let result = check_inner(pattern, ty, mode, env)?;
    let mut names = HashSet::new();
    for b in result.bindings() {
        if !names.insert(b.name.clone()) {
            return Err(invalid_pattern(
                PatternError::DuplicateBinding { name: b.name },
                b.span,
            ));
        }
    }
    Ok(result)
}
fn check_inner(pattern: &Pattern, ty: &Type, mode: Mode, env: &TypeEnvironment) -> Result<Checked> {
    cyallo_ast::with_stack(|| {
        let ty = crate::composites::type_view(ty, env);
        let kind = match &pattern.kind {
            PatternKind::Wildcard => Kind::Wildcard,
            PatternKind::Binding(name) => Kind::Binding(Binding {
                name: name.clone(),
                ty: mode.binding(&ty),
                span: pattern.span,
            }),
            PatternKind::ResolvedVariant(variant, payload) => {
                let Some(contract) = crate::enums::contract(&ty, &env.native) else {
                    return Err(invalid_pattern(
                        PatternError::ExpectedEnum { actual: ty },
                        pattern.span,
                    ));
                };
                let declaration = contract
                    .variants
                    .iter()
                    .find(|v| v.declaration == *variant)
                    .ok_or_else(|| {
                        invalid_pattern(
                            PatternError::WrongVariant {
                                enumeration: contract.name.clone(),
                            },
                            pattern.span,
                        )
                    })?;
                let payload = match (&declaration.payload, payload) {
                    (Some(ty), Some(pattern)) => {
                        Some(Box::new(check_inner(pattern, ty, mode, env)?))
                    }
                    (None, None) => None,
                    _ => return Err(invalid_pattern(PatternError::PayloadArity, pattern.span)),
                };
                Kind::Variant(*variant, payload)
            }
            PatternKind::Variant(..) => {
                return Err(invalid_pattern(
                    PatternError::UnresolvedPattern,
                    pattern.span,
                ));
            }
            PatternKind::Tuple(patterns) => {
                let fields = match &ty {
                    Type::Tuple(types) if types.len() == patterns.len() => types.as_slice(),
                    Type::Void if patterns.is_empty() => &[],
                    _ => {
                        return Err(invalid_pattern(
                            PatternError::ExpectedTuple {
                                actual: ty,
                                fields: patterns.len(),
                            },
                            pattern.span,
                        ));
                    }
                };
                Kind::Tuple(
                    patterns
                        .iter()
                        .zip(fields)
                        .map(|(p, t)| check_inner(p, t, mode, env))
                        .collect::<Result<_>>()?,
                )
            }
            PatternKind::Record(patterns, rest) => {
                let (Type::Record(_, fields) | Type::AnonymousRecord(fields)) = &ty else {
                    return Err(invalid_pattern(
                        PatternError::ExpectedRecord { actual: ty },
                        pattern.span,
                    ));
                };
                if (!*rest && fields.len() != patterns.len())
                    || patterns
                        .iter()
                        .any(|(name, _)| !fields.iter().any(|(field, _)| field == name))
                {
                    return Err(invalid_pattern(PatternError::RecordFields, pattern.span));
                }
                Kind::Record(
                    patterns
                        .iter()
                        .map(|(name, p)| {
                            let (index, (_, ty)) = fields
                                .iter()
                                .enumerate()
                                .find(|(_, (field, _))| field == name)
                                .unwrap();
                            Ok((index, check_inner(p, ty, mode, env)?))
                        })
                        .collect::<Result<_>>()?,
                )
            }
            PatternKind::Literal(value) => {
                let actual = crate::analysis::literal_type(value);
                if !crate::declarations::equivalent_types(&ty, &actual)
                    && !(ty.number_kind().is_some() && actual.number_kind().is_some())
                {
                    return Err(invalid_pattern(
                        PatternError::LiteralType {
                            expected: ty,
                            actual,
                        },
                        pattern.span,
                    ));
                }
                Kind::Literal(value.clone())
            }
            PatternKind::Or(patterns) => {
                let values = patterns
                    .iter()
                    .map(|p| check_inner(p, &ty, mode, env))
                    .collect::<Result<Vec<_>>>()?;
                let bindings = |value: &Checked| {
                    value
                        .bindings()
                        .into_iter()
                        .map(|b| (b.name, b.ty))
                        .collect::<BTreeMap<_, _>>()
                };
                if let Some(first) = values.first()
                    && values
                        .iter()
                        .skip(1)
                        .any(|v| bindings(v) != bindings(first))
                {
                    return Err(invalid_pattern(
                        PatternError::AlternativeBindings,
                        pattern.span,
                    ));
                }
                Kind::Or(values)
            }
        };
        Ok(Checked { ty, kind })
    })
}
#[derive(Clone)]
enum Constructor {
    Variant(VariantId, Option<Type>),
    Boolean(bool),
    Product(Vec<Type>),
}
fn constructors(ty: &Type, env: &TypeEnvironment) -> Option<Vec<Constructor>> {
    if let Some(contract) = crate::enums::contract(ty, &env.native) {
        return Some(
            contract
                .variants
                .iter()
                .map(|v| Constructor::Variant(v.declaration, v.payload.clone()))
                .collect(),
        );
    }
    Some(match crate::composites::type_view(ty, env) {
        Type::Enum(contract) => contract
            .variants
            .iter()
            .map(|v| Constructor::Variant(v.declaration, v.payload.clone()))
            .collect(),
        Type::Boolean => vec![Constructor::Boolean(false), Constructor::Boolean(true)],
        Type::Void => vec![Constructor::Product(vec![])],
        Type::Tuple(fields) => vec![Constructor::Product(fields)],
        Type::AnonymousRecord(fields) | Type::Record(_, fields) => vec![Constructor::Product(
            fields.into_iter().map(|(_, ty)| ty).collect(),
        )],
        _ => return None,
    })
}
impl Constructor {
    fn fields(&self) -> Vec<Type> {
        match self {
            Self::Variant(_, ty) => ty.iter().cloned().collect(),
            Self::Boolean(_) => vec![],
            Self::Product(fields) => fields.clone(),
        }
    }
    fn specialize(&self, pattern: &Checked) -> Option<Vec<Checked>> {
        let fields = self.fields();
        let wild = || {
            fields
                .iter()
                .map(|ty| Checked {
                    ty: ty.clone(),
                    kind: Kind::Wildcard,
                })
                .collect()
        };
        Some(match (&self, &pattern.kind) {
            (_, Kind::Wildcard | Kind::Binding(_)) => wild(),
            (Self::Variant(id, _), Kind::Variant(actual, payload)) if id == actual => {
                payload.iter().map(|p| *p.clone()).collect()
            }
            (Self::Boolean(b), Kind::Literal(Literal::Boolean(actual))) if b == actual => vec![],
            (Self::Product(_), Kind::Tuple(values)) => values.clone(),
            (Self::Product(_), Kind::Record(values)) => {
                let mut result: Vec<Checked> = wild();
                for (i, value) in values {
                    result[*i] = value.clone();
                }
                result
            }
            _ => return None,
        })
    }
}
fn expand(rows: Vec<Vec<Checked>>) -> Vec<Vec<Checked>> {
    let mut output = vec![];
    let mut pending = rows;
    while let Some(mut row) = pending.pop() {
        if let Some(Checked {
            kind: Kind::Or(alternatives),
            ..
        }) = row.first().cloned()
        {
            row.remove(0);
            for value in alternatives {
                let mut expanded = vec![value];
                expanded.extend(row.clone());
                pending.push(expanded);
            }
        } else {
            output.push(row)
        }
    }
    output
}
fn covered(
    rows: Vec<Vec<Checked>>,
    types: &[Type],
    env: &TypeEnvironment,
    budget: &mut usize,
) -> Result<bool> {
    cyallo_ast::with_stack(|| covered_inner(rows, types, env, budget))
}
fn covered_inner(
    rows: Vec<Vec<Checked>>,
    types: &[Type],
    env: &TypeEnvironment,
    budget: &mut usize,
) -> Result<bool> {
    if *budget == 0 {
        return Err(invalid_pattern(
            PatternError::ComplexityLimit,
            Span::default(),
        ));
    }
    *budget -= 1;
    if types.iter().any(|ty| {
        crate::enums::contract(ty, &env.native).is_some_and(|contract| contract.variants.is_empty())
    }) {
        return Ok(true);
    }
    if rows.is_empty() {
        return Ok(false);
    }
    if types.is_empty()
        || rows.iter().any(|row| {
            row.iter()
                .all(|p| matches!(p.kind, Kind::Wildcard | Kind::Binding(_)))
        })
    {
        return Ok(true);
    }
    let rows = expand(rows);
    if let Some(constructors) = constructors(&types[0], env) {
        for constructor in constructors {
            let specialized = rows
                .iter()
                .filter_map(|row| {
                    let mut fields = constructor.specialize(&row[0])?;
                    fields.extend(row[1..].iter().cloned());
                    Some(fields)
                })
                .collect();
            let mut fields = constructor.fields();
            fields.extend_from_slice(&types[1..]);
            if !covered(specialized, &fields, env, budget)? {
                return Ok(false);
            }
        }
        Ok(true)
    } else {
        covered(
            rows.into_iter()
                .filter(|row| matches!(row[0].kind, Kind::Wildcard | Kind::Binding(_)))
                .map(|row| row[1..].to_vec())
                .collect(),
            &types[1..],
            env,
            budget,
        )
    }
}
pub fn exhaustive(ty: &Type, arms: &[MatchArm], env: &TypeEnvironment, span: Span) -> Result<()> {
    let (_, ty) = Mode::split(ty);
    let ty = crate::composites::type_view(ty, env);
    let patterns = arms
        .iter()
        .filter(|arm| arm.guard.is_none())
        .map(|arm| check(&arm.pattern, &ty, env))
        .collect::<Result<Vec<_>>>()?;
    if let Some(contract) = crate::enums::contract(&ty, &env.native)
        && contract.open
        && arms.first().and_then(|arm| arm.package) != Some(contract.package)
        && !patterns
            .iter()
            .any(|p| matches!(p.kind, Kind::Wildcard | Kind::Binding(_)))
    {
        return Err(invalid_pattern(PatternError::OpenEnumWildcard, span));
    }
    let rows: Vec<_> = patterns.into_iter().map(|p| vec![p]).collect();
    let mut budget = 4096;
    if covered(rows.clone(), std::slice::from_ref(&ty), env, &mut budget)? {
        return Ok(());
    }
    let mut missing = vec![];
    if let Type::Enum(contract) = &ty {
        for variant in &contract.variants {
            let constructor = Constructor::Variant(variant.declaration, variant.payload.clone());
            let rows = expand(rows.clone())
                .iter()
                .filter_map(|row| constructor.specialize(&row[0]))
                .collect();
            if !covered(rows, &constructor.fields(), env, &mut budget)? {
                missing.push(variant.name.clone());
            }
        }
    }
    Err(invalid_pattern(
        PatternError::NonExhaustive { missing },
        span,
    ))
}

pub fn scrutinee_type(
    value: &Expr,
    known: &std::collections::HashMap<String, Type>,
    env: &TypeEnvironment,
) -> Result<Type> {
    let ty = crate::inference::expression_type(value, known, env)?;
    if let ExprKind::Unary(Operator::Dereference, base) = &value.kind
        && matches!(
            crate::inference::expression_type(base, known, env)?,
            Type::Managed { .. }
        )
    {
        return Ok(Mode::Shared.binding(&ty));
    }
    Ok(ty)
}
pub fn expression_type(
    value: &Expr,
    arms: &[MatchArm],
    known: &std::collections::HashMap<String, Type>,
    env: &TypeEnvironment,
    span: Span,
) -> Result<Type> {
    let ty = scrutinee_type(value, known, env)?;
    exhaustive(&ty, arms, env, span)?;
    let mut result = None;
    for arm in arms {
        let mut scope = known.clone();
        for binding in check(&arm.pattern, &ty, env)?.bindings() {
            scope.insert(binding.name.to_string(), binding.ty);
        }
        if let Some(guard) = &arm.guard {
            let ty = crate::inference::expression_type(guard, &scope, env)?;
            if ty != Type::Boolean {
                return Err(invalid_pattern(
                    PatternError::GuardType { actual: ty },
                    guard.span,
                ));
            }
        }
        let actual = match &arm.body {
            MatchBody::Expression(value) => {
                Some(crate::inference::expression_type(value, &scope, env)?)
            }
            MatchBody::Block(body, tail) => {
                let globals = scope
                    .iter()
                    .map(|(name, ty)| crate::ir::Local {
                        name: name.clone().into(),
                        display_name: None,
                        ty: ty.clone(),
                        constant: true,
                        span,
                    })
                    .collect::<Vec<_>>();
                let mut locals = crate::analysis::collect(body, &[])?;
                crate::inference::infer(
                    body,
                    &mut locals,
                    &globals,
                    env,
                    crate::CompileOptions::default(),
                )?;
                scope.extend(locals.into_iter().map(|l| (l.name.to_string(), l.ty)));
                if let Some(tail) = tail {
                    Some(crate::inference::expression_type(tail, &scope, env)?)
                } else if crate::analysis::returns(body) {
                    None
                } else {
                    Some(Type::Void)
                }
            }
        };
        if let Some(actual) = actual.filter(|ty| *ty != Type::Never) {
            if let Some(expected) = &result {
                if !crate::declarations::equivalent_types(expected, &actual) {
                    return Err(invalid_pattern(
                        PatternError::ArmType {
                            expected: expected.clone(),
                            actual,
                        },
                        arm.span,
                    ));
                }
            } else {
                result = Some(actual)
            }
        }
    }
    Ok(result.unwrap_or(Type::Never))
}

/// Expand alternatives before lowering, preserving the source arm/guard boundary.
pub fn alternatives(pattern: &Checked) -> Result<Vec<Checked>> {
    cyallo_ast::with_stack(|| {
        fn product(columns: Vec<Vec<Checked>>) -> Result<Vec<Vec<Checked>>> {
            let mut rows = vec![vec![]];
            for column in columns {
                let mut next = vec![];
                for row in rows {
                    for value in &column {
                        if next.len() >= 1024 {
                            return Err(invalid_pattern(
                                PatternError::ComplexityLimit,
                                Span::default(),
                            ));
                        }
                        let mut row = row.clone();
                        row.push(value.clone());
                        next.push(row);
                    }
                }
                rows = next;
            }
            Ok(rows)
        }
        let kinds = match &pattern.kind {
            Kind::Or(values) => {
                let mut result = vec![];
                for value in values {
                    result.extend(alternatives(value)?);
                    if result.len() > 1024 {
                        return Err(invalid_pattern(
                            PatternError::ComplexityLimit,
                            Span::default(),
                        ));
                    }
                }
                return Ok(result);
            }
            Kind::Variant(id, Some(payload)) => alternatives(payload)?
                .into_iter()
                .map(|p| Kind::Variant(*id, Some(Box::new(p))))
                .collect(),
            Kind::Tuple(values) => {
                product(values.iter().map(alternatives).collect::<Result<_>>()?)?
                    .into_iter()
                    .map(Kind::Tuple)
                    .collect()
            }
            Kind::Record(fields) => {
                let indices: Vec<_> = fields.iter().map(|(i, _)| *i).collect();
                product(
                    fields
                        .iter()
                        .map(|(_, p)| alternatives(p))
                        .collect::<Result<_>>()?,
                )?
                .into_iter()
                .map(|row| Kind::Record(indices.iter().copied().zip(row).collect()))
                .collect()
            }
            kind => vec![kind.clone()],
        };
        Ok(kinds
            .into_iter()
            .map(|kind| Checked {
                ty: pattern.ty.clone(),
                kind,
            })
            .collect())
    })
}
