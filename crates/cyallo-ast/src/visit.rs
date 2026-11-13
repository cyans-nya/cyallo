use crate::*;
type Result<T> = std::result::Result<T, Diagnostic>;
pub fn visit_expr(e: &mut Expr, f: &mut impl FnMut(&mut Expr) -> Result<()>) -> Result<()> {
    crate::with_stack(|| {
        match &mut e.kind {
            ExprKind::ManagedNew(_, allocator, value) => {
                if let Some(allocator) = allocator {
                    visit_expr(allocator, f)?;
                }
                visit_expr(value, f)?;
            }
            ExprKind::ArrayLiteral(es)
            | ExprKind::IntrinsicCall(_, _, es)
            | ExprKind::Tuple(es)
            | ExprKind::Call(_, es)
            | ExprKind::TypedCollection(_, _, es)
            | ExprKind::GenericCall(_, _, es) => {
                for e in es {
                    visit_expr(e, f)?;
                }
            }
            ExprKind::Record(_, fs) | ExprKind::GenericRecord(_, _, fs) => {
                for (_, e) in fs {
                    visit_expr(e, f)?;
                }
            }
            ExprKind::Yield(a, _)
            | ExprKind::Parenthesized(a)
            | ExprKind::Unary(_, a)
            | ExprKind::Field(a, _)
            | ExprKind::ExtensionField(a, _, _)
            | ExprKind::Cast(a, _)
            | ExprKind::TupleField(a, _) => visit_expr(a, f)?,
            ExprKind::Binary(_, a, b) => {
                visit_expr(a, f)?;
                visit_expr(b, f)?;
            }
            ExprKind::Index(a, es) | ExprKind::Invoke(a, es) => {
                visit_expr(a, f)?;
                for e in es {
                    visit_expr(e, f)?;
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
                    visit_expr(e, f)?;
                }
                StmtKind::Constant(_, e)
                | StmtKind::Evaluate(e)
                | StmtKind::Return(e)
                | StmtKind::Input(e) => visit_expr(e, f)?,
                StmtKind::Assign(a, b) | StmtKind::Compound(a, _, b) => {
                    visit_expr(a, f)?;
                    visit_expr(b, f)?;
                }
                StmtKind::Output(es) | StmtKind::Call(_, es) => {
                    for e in es {
                        visit_expr(e, f)?;
                    }
                }
                StmtKind::If(e, a, b) => {
                    visit_expr(e, f)?;
                    visit_block(a, f, t)?;
                    visit_block(b, f, t)?;
                }
                StmtKind::While(e, b) | StmtKind::Repeat(b, e) => {
                    visit_expr(e, f)?;
                    visit_block(b, f, t)?;
                }
                StmtKind::For(_, a, b, step, body) => {
                    visit_expr(a, f)?;
                    visit_expr(b, f)?;
                    if let Some(e) = step {
                        visit_expr(e, f)?;
                    }
                    visit_block(body, f, t)?;
                }
                StmtKind::ForIn(_, iterable, body) => {
                    visit_expr(iterable, f)?;
                    visit_block(body, f, t)?;
                }
                StmtKind::Case(e, arms, b) => {
                    visit_expr(e, f)?;
                    for (label, body) in arms {
                        visit_expr(&mut label.lower, f)?;
                        if let Some(e) = &mut label.upper {
                            visit_expr(e, f)?;
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
