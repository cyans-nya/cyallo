use super::*;
use crate::patterns::{self, Checked, Kind, Mode};
impl Lower<'_> {
    fn match_result(&mut self, value: &Expr, result: Reg, returned: &Type) -> Result<()> {
        if *returned == Type::Void {
            self.statement(&Stmt {
                kind: StmtKind::Evaluate(value.clone()),
                span: value.span,
            })?;
            self.emit(Op::Aggregate(result, vec![]));
        } else {
            let (source, actual) = self.expr_expected(value, returned)?;
            self.require(returned, &actual, value.span)?;
            self.emit(Op::Move(result, source));
        }
        Ok(())
    }
    fn pattern_tests(
        &mut self,
        pattern: &Checked,
        address: Reg,
        failed: &mut Vec<usize>,
        bindings: &mut Vec<(patterns::Binding, Reg, bool)>,
        by_value: bool,
    ) -> Result<()> {
        cyallo_ast::with_stack(|| {
            match &pattern.kind {
                Kind::Wildcard => {}
                Kind::Binding(binding) => bindings.push((binding.clone(), address, false)),
                Kind::Variant(variant, payload) => {
                    let contract = crate::enums::contract(&pattern.ty, &self.types.native)
                        .expect("checked enum pattern");
                    let operation = contract
                        .variants
                        .iter()
                        .find(|v| v.declaration == *variant)
                        .expect("checked variant")
                        .operation();
                    let condition = self.reg(Type::Boolean);
                    self.emit(Op::EnumTest(condition, address, operation));
                    let branch = self.emit(Op::Branch(
                        condition,
                        self.function.code.len() + 1,
                        usize::MAX,
                    ));
                    failed.push(branch);
                    if let Some(payload) = payload
                        && by_value
                        && let Kind::Binding(binding) = &payload.kind
                        && crate::ownership::copy(&payload.ty)
                    {
                        // A copied payload is read as a value: no payload cell is needed.
                        let child = self.reg(payload.ty.clone());
                        self.emit(Op::EnumPayload(
                            child,
                            address,
                            operation,
                            crate::enums::Projection::Value,
                            Box::new(payload.ty.clone()),
                        ));
                        bindings.push((binding.clone(), child, true));
                    } else if let Some(payload) = payload {
                        let child = self.reg(payload.ty.clone());
                        self.emit(Op::EnumPayload(
                            child,
                            address,
                            operation,
                            crate::enums::Projection::Address,
                            Box::new(payload.ty.clone()),
                        ));
                        self.pattern_tests(payload, child, failed, bindings, by_value)?;
                    }
                }
                Kind::Tuple(values) => {
                    for (index, value) in values.iter().enumerate() {
                        let child = self.reg(value.ty.clone());
                        self.emit(Op::Field(child, address, index));
                        self.pattern_tests(value, child, failed, bindings, by_value)?;
                    }
                }
                Kind::Record(fields) => {
                    for (index, value) in fields {
                        let child = self.reg(value.ty.clone());
                        self.emit(Op::Field(child, address, *index));
                        self.pattern_tests(value, child, failed, bindings, by_value)?;
                    }
                }
                Kind::Literal(literal) => {
                    let value = self.reg(pattern.ty.clone());
                    self.emit(Op::Load(value, address));
                    let literal = Expr {
                        kind: ExprKind::Literal(literal.clone()),
                        span: self.span,
                    };
                    let (expected, actual) = self.expr_expected(&literal, &pattern.ty)?;
                    self.require(&pattern.ty, &actual, literal.span)?;
                    let condition = self.reg(Type::Boolean);
                    self.emit(Op::Binary(condition, Operator::Equal, value, expected));
                    let branch = self.emit(Op::Branch(
                        condition,
                        self.function.code.len() + 1,
                        usize::MAX,
                    ));
                    failed.push(branch);
                }
                Kind::Or(_) => {
                    return Err(patterns::invalid_pattern(
                        patterns::PatternError::UnresolvedPattern,
                        self.span,
                    ));
                }
            }
            Ok(())
        })
    }
    fn patch_pattern_failures(&mut self, failed: Vec<usize>, target: usize) {
        for pc in failed {
            if let Op::Branch(_, _, no) = &mut self.function.code[pc].op {
                *no = target;
            } else {
                self.function.code[pc].op = Op::Jump(target);
            }
        }
    }
    pub(super) fn match_expression(
        &mut self,
        value: &Expr,
        arms: &[MatchArm],
        span: Span,
    ) -> Result<(Reg, Type)> {
        let known = self
            .globals
            .iter()
            .chain(&self.function.locals)
            .map(|l| (l.name.to_string(), l.ty.clone()))
            .collect();
        let scrutinee = patterns::scrutinee_type(value, &known, self.types)?;
        let returned = patterns::expression_type(value, arms, &known, self.types, span)?;
        let (mode, ty) = Mode::split(&scrutinee);
        let (address, temporary) = if mode != Mode::Value
            && matches!(
                self.expression_type(value)?,
                Type::Reference { .. } | Type::Borrowed(_)
            ) {
            (self.expr(value)?.0, false)
        } else if assignable(value) {
            (self.address(value, mode == Mode::Mutable)?.0, false)
        } else {
            let (source, actual) = self.expr(value)?;
            let index = self.function.locals.len();
            self.function.locals.push(ir::Local {
                display_name: None,
                name: format!("$match_value${index}").into(),
                ty: actual.clone(),
                constant: true,
                span,
            });
            let address = self.reg(actual);
            self.emit(Op::Address(
                address,
                Slot {
                    global: false,
                    index,
                },
            ));
            self.emit(Op::Store(address, source));
            (address, true)
        };
        let result = self.reg(returned.clone());
        let mut exits = vec![];
        for arm in arms {
            let checked = patterns::check(&arm.pattern, &scrutinee, self.types)?;
            let mut next_arm = vec![];
            for alternative in patterns::alternatives(&checked)? {
                let mut failed = vec![];
                let mut bindings = vec![];
                self.span = arm.pattern.span;
                self.pattern_tests(
                    &alternative,
                    address,
                    &mut failed,
                    &mut bindings,
                    mode == Mode::Value && arm.guard.is_none(),
                )?;
                // Guard bindings alias existing storage: no owner is copied or moved.
                let previous = self.guard_aliases.clone();
                if let Some(guard) = &arm.guard {
                    for (binding, source, _) in &bindings {
                        if mode == Mode::Value {
                            self.guard_aliases
                                .insert(binding.name.to_string(), (*source, binding.ty.clone()));
                        } else {
                            let reference = self.reg(binding.ty.clone());
                            self.emit(Op::Move(reference, *source));
                            let (slot, local) = self.lookup(&binding.name)?;
                            let target = self.reg(local.ty);
                            self.emit(Op::Address(target, slot));
                            self.emit(Op::Store(target, reference));
                        }
                    }
                    let condition = self.condition(guard)?;
                    let branch = self.emit(Op::Branch(
                        condition,
                        self.function.code.len() + 1,
                        usize::MAX,
                    ));
                    next_arm.push(branch);
                }
                self.guard_aliases = previous;
                self.drop_scopes.push(vec![]);
                for (binding, source, direct) in &bindings {
                    let value = if *direct {
                        *source
                    } else if mode == Mode::Value {
                        let value = self.reg(binding.ty.clone());
                        self.emit(if crate::ownership::copy(&binding.ty) {
                            Op::Load(value, *source)
                        } else {
                            Op::Take(value, *source)
                        });
                        value
                    } else {
                        let reference = self.reg(binding.ty.clone());
                        self.emit(Op::Move(reference, *source));
                        reference
                    };
                    let (slot, local) = self.lookup(&binding.name)?;
                    let target = self.reg(local.ty);
                    self.emit(Op::Address(target, slot));
                    self.emit(Op::Store(target, value));
                    if mode == Mode::Value && crate::ownership::needs_drop(&binding.ty) {
                        self.drop_scopes
                            .last_mut()
                            .unwrap()
                            .push((slot, binding.ty.clone()));
                    }
                }
                if mode == Mode::Value && alternative.consumes() {
                    self.emit(Op::Drop(address));
                }
                match &arm.body {
                    MatchBody::Expression(value) => self.match_result(value, result, &returned)?,
                    MatchBody::Block(body, tail) => {
                        // A block tail is evaluated before its locals are dropped.
                        self.block_depth += 1;
                        for statement in body {
                            self.statement(statement)?;
                        }
                        if let Some(tail) = tail {
                            self.match_result(tail, result, &returned)?;
                        } else if !returns(body) {
                            self.emit(Op::Aggregate(result, vec![]));
                        }
                        self.block_depth -= 1;
                    }
                }
                let slots = self.drop_scopes.pop().unwrap();
                self.drop_slots(slots);
                exits.push(self.emit(Op::Jump(usize::MAX)));
                self.patch_pattern_failures(failed, self.function.code.len());
            }
            self.patch_pattern_failures(next_arm, self.function.code.len());
        }
        self.emit(Op::Unreachable);
        let end = self.function.code.len();
        for exit in exits {
            self.function.code[exit].op = Op::Jump(end);
        }
        if temporary && crate::ownership::needs_drop(ty) {
            self.emit(Op::Drop(address));
        }
        Ok((result, returned))
    }
}
