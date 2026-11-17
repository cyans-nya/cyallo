use crate::*;
type Result<T> = std::result::Result<T, Diagnostic>;
pub fn visit_expr(e: &mut Expr, f: &mut impl FnMut(&mut Expr) -> Result<()>) -> Result<()> {
    visit_expr_typed(e, f, &mut |_, _| Ok(()))
}
fn visit_expr_typed(
    e: &mut Expr,
    f: &mut impl FnMut(&mut Expr) -> Result<()>,
    t: &mut impl FnMut(&mut Type, Span) -> Result<()>,
) -> Result<()> {
    crate::with_stack(|| {
        match &mut e.kind {
            ExprKind::EnumConstruct(_, ty, payload) => {
                t(ty, e.span)?;
                if let Some(payload) = payload {
                    visit_expr_typed(payload, f, t)?;
                }
            }
            ExprKind::Match(value, arms) => {
                visit_expr_typed(value, f, t)?;
                for arm in arms {
                    if let Some(guard) = &mut arm.guard {
                        visit_expr_typed(guard, f, t)?;
                    }
                    match &mut arm.body {
                        MatchBody::Expression(value) => visit_expr_typed(value, f, t)?,
                        MatchBody::Block(body, tail) => {
                            visit_block(body, f, t)?;
                            if let Some(tail) = tail {
                                visit_expr_typed(tail, f, t)?;
                            }
                        }
                    }
                }
            }
            ExprKind::ManagedNew(_, _, allocator, value) => {
                if let Some(allocator) = allocator {
                    visit_expr_typed(allocator, f, t)?;
                }
                visit_expr_typed(value, f, t)?;
            }
            ExprKind::ArrayLiteral(es)
            | ExprKind::IntrinsicCall(_, _, es)
            | ExprKind::Tuple(es)
            | ExprKind::Call(_, es)
            | ExprKind::TypedCollection(_, _, es)
            | ExprKind::GenericCall(_, _, es) => {
                for e in es {
                    visit_expr_typed(e, f, t)?;
                }
            }
            ExprKind::AnonymousRecord(fs)
            | ExprKind::VariantPayload(fs)
            | ExprKind::Record(_, fs)
            | ExprKind::GenericRecord(_, _, fs)
            | ExprKind::ObjectLiteral(_, fs) => {
                for (_, e) in fs {
                    visit_expr_typed(e, f, t)?;
                }
            }
            ExprKind::DeleteProperty(a)
            | ExprKind::Yield(a, _)
            | ExprKind::Parenthesized(a)
            | ExprKind::Unary(_, a)
            | ExprKind::Field(a, _)
            | ExprKind::ExtensionField(a, _, _)
            | ExprKind::Cast(a, _)
            | ExprKind::TupleField(a, _) => visit_expr_typed(a, f, t)?,
            ExprKind::Binary(_, a, b) | ExprKind::NativeCoerce(a, b) => {
                visit_expr_typed(a, f, t)?;
                visit_expr_typed(b, f, t)?;
            }
            ExprKind::Index(a, es) | ExprKind::Invoke(a, es) => {
                visit_expr_typed(a, f, t)?;
                for e in es {
                    visit_expr_typed(e, f, t)?;
                }
            }
            _ => {}
        }
        f(e)
    })
}
pub fn visit_block(
    body: &mut [Stmt],
    f: &mut impl FnMut(&mut Expr) -> Result<()>,
    t: &mut impl FnMut(&mut Type, Span) -> Result<()>,
) -> Result<()> {
    crate::with_stack(|| {
        for s in body {
            match &mut s.kind {
                StmtKind::Declare(_, ty) => t(ty, s.span)?,
                StmtKind::Binding(_, ty, _, _, e) => {
                    if let Some(ty) = ty {
                        t(ty, s.span)?;
                    }
                    visit_expr_typed(e, f, t)?;
                }
                StmtKind::Constant(_, e)
                | StmtKind::Evaluate(e)
                | StmtKind::Return(e)
                | StmtKind::Input(e) => visit_expr_typed(e, f, t)?,
                StmtKind::Assign(a, b) | StmtKind::Compound(a, _, b) => {
                    visit_expr_typed(a, f, t)?;
                    visit_expr_typed(b, f, t)?;
                }
                StmtKind::Output(es) | StmtKind::Call(_, es) => {
                    for e in es {
                        visit_expr_typed(e, f, t)?;
                    }
                }
                StmtKind::If(e, a, b) | StmtKind::IfLet(_, e, a, b) => {
                    visit_expr_typed(e, f, t)?;
                    visit_block(a, f, t)?;
                    visit_block(b, f, t)?;
                }
                StmtKind::While(e, b) | StmtKind::Repeat(b, e) => {
                    visit_expr_typed(e, f, t)?;
                    visit_block(b, f, t)?;
                }
                StmtKind::For(_, a, b, step, body) => {
                    visit_expr_typed(a, f, t)?;
                    visit_expr_typed(b, f, t)?;
                    if let Some(e) = step {
                        visit_expr_typed(e, f, t)?;
                    }
                    visit_block(body, f, t)?;
                }
                StmtKind::ForIn(_, iterable, body) => {
                    visit_expr_typed(iterable, f, t)?;
                    visit_block(body, f, t)?;
                }
                StmtKind::Case(e, arms, b) => {
                    visit_expr_typed(e, f, t)?;
                    for (label, body) in arms {
                        visit_expr_typed(&mut label.lower, f, t)?;
                        if let Some(e) = &mut label.upper {
                            visit_expr_typed(e, f, t)?;
                        }
                        visit_block(body, f, t)?;
                    }
                    visit_block(b, f, t)?;
                }
                _ => {}
            }
        }
        Ok(())
    })
}

/// Postorder traversal of a type contract, shared by declaration instantiation
/// and native-family resolution. No interpretation of source names occurs here.
pub fn visit_type(ty: &mut Type, f: &mut impl FnMut(&mut Type)) {
    crate::with_stack(|| {
        match ty {
            Type::Enum(value) => {
                let value = std::sync::Arc::make_mut(value);
                for t in &mut value.identity.arguments {
                    visit_type(t, f);
                }
                for v in &mut value.variants {
                    if let Some(t) = &mut v.payload {
                        visit_type(t, f);
                    }
                }
            }
            Type::EnumRef(identity, _) => {
                for t in &mut identity.arguments {
                    visit_type(t, f);
                }
            }
            Type::Applied(_, arguments)
            | Type::Opaque(_, arguments, _)
            | Type::Dependent(_, arguments)
            | Type::Tuple(arguments) => {
                for argument in arguments {
                    visit_type(argument, f);
                }
            }
            Type::Array(_, t)
            | Type::SparseArray(t)
            | Type::Borrowed(t)
            | Type::Slice(t)
            | Type::Storage(_, t)
            | Type::ServiceKey(t)
            | Type::ServiceRef(t)
            | Type::Reference { pointee: t, .. }
            | Type::Managed { pointee: t, .. } => visit_type(t, f),
            Type::AnonymousRecord(fields) | Type::Record(_, fields) | Type::Object(fields) => {
                for (_, ty) in fields {
                    visit_type(ty, f);
                }
            }
            Type::Function(_, parameters, result) => {
                for (ty, _) in parameters {
                    visit_type(ty, f);
                }
                visit_type(result, f);
            }
            _ => {}
        }
        f(ty);
    });
}
