use super::*;
use crate::patterns::{self, Checked, Kind, Mode, PatternError, invalid_pattern};
impl Checker<'_> {
    fn pattern_bindings(
        &mut self,
        pattern: &Checked,
        mode: Mode,
        info: &ValueInfo,
        place: Option<&Place>,
    ) {
        fn walk(
            pattern: &Checked,
            mode: Mode,
            info: &ValueInfo,
            place: Option<&Place>,
            bindings: &mut HashMap<String, Binding>,
        ) {
            cyallo_ast::with_stack(|| match &pattern.kind {
                Kind::Binding(binding) => {
                    let value = if mode == Mode::Value {
                        info.clone()
                    } else {
                        let mut value = ValueInfo::default();
                        if let Some(place) = place {
                            value.loans.push(Loan {
                                place: place.clone(),
                                mutable: mode == Mode::Mutable,
                                view: false,
                                span: binding.span,
                            });
                        }
                        value
                    };
                    bindings
                        .entry(binding.name.to_string())
                        .or_default()
                        .value
                        .extend(value);
                }
                Kind::Variant(_, Some(payload)) => {
                    let mut place = place.cloned();
                    if let Some(place) = &mut place {
                        place.projections.push(Projection::Field(0));
                    }
                    walk(payload, mode, info, place.as_ref(), bindings);
                }
                Kind::Tuple(values) => {
                    for (i, value) in values.iter().enumerate() {
                        let mut place = place.cloned();
                        if let Some(place) = &mut place {
                            place.projections.push(Projection::Field(i));
                        }
                        walk(value, mode, info, place.as_ref(), bindings);
                    }
                }
                Kind::Record(fields) => {
                    for (i, value) in fields {
                        let mut place = place.cloned();
                        if let Some(place) = &mut place {
                            place.projections.push(Projection::Field(*i));
                        }
                        walk(value, mode, info, place.as_ref(), bindings);
                    }
                }
                Kind::Or(values) => {
                    for value in values {
                        walk(value, mode, info, place, bindings);
                    }
                }
                _ => {}
            })
        }
        walk(pattern, mode, info, place, &mut self.bindings)
    }
    pub(super) fn matching(
        &mut self,
        value: &Expr,
        arms: &[MatchArm],
        usage: Use,
    ) -> Result<ValueInfo> {
        let ty = patterns::scrutinee_type(value, &self.types, self.routines)?;
        let (mode, _) = Mode::split(&ty);
        let info = self.expression(value, Use::Read)?;
        let place = info
            .loans
            .first()
            .map(|loan| loan.place.clone())
            .or_else(|| self.place(value).ok());
        let before = self.bindings.clone();
        let mut states = vec![];
        let mut result = ValueInfo::default();
        for arm in arms {
            self.bindings = before.clone();
            let pattern = patterns::check(&arm.pattern, &ty, self.routines)?;
            let bindings = pattern.bindings();
            for binding in &bindings {
                self.bindings.remove(binding.name.as_ref());
            }
            self.pattern_bindings(&pattern, mode, &info, place.as_ref());
            if let Some(guard) = &arm.guard {
                let previous_names = self.guard_bindings.clone();
                let previous_places = self.guard_places.clone();
                self.guard_bindings
                    .extend(bindings.iter().map(|b| b.name.to_string()));
                if let Some(place) = &place {
                    self.guard_places.push(place.clone());
                }
                self.expression(guard, Use::Read)?;
                self.guard_bindings = previous_names;
                self.guard_places = previous_places;
            }
            if mode == Mode::Value && pattern.consumes() {
                self.expression(value, Use::Move)?;
            }
            match &arm.body {
                MatchBody::Expression(value) => result.extend(self.expression(value, usage)?),
                MatchBody::Block(body, tail) => {
                    self.block(body)?;
                    if let Some(tail) = tail {
                        result.extend(self.expression(tail, usage)?);
                    }
                }
            }
            for binding in bindings {
                self.bindings.remove(binding.name.as_ref());
            }
            states.push(self.bindings.clone());
        }
        self.bindings = before;
        for state in states {
            for (name, state) in state {
                let current = self.bindings.entry(name).or_default();
                current.value.extend(state.value);
                current.moved = current.moved.or(state.moved);
            }
        }
        Ok(result)
    }
    pub(super) fn guard_move(&self, name: &str, place: &Place, span: Span) -> Result<()> {
        if self.guard_bindings.contains(name)
            || self.guard_places.iter().any(|guard| guard.overlaps(place))
        {
            return Err(invalid_pattern(PatternError::GuardMove, span));
        }
        Ok(())
    }
}
