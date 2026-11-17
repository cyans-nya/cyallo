use super::*;
use crate::patterns::{PatternError, invalid_pattern};
use std::collections::BTreeSet;
impl Resolver {
    pub(super) fn pattern(
        &mut self,
        module: usize,
        pattern: &mut Pattern,
        bindings: &mut HashMap<String, Local>,
    ) -> Result<()> {
        fn walk(
            resolver: &mut Resolver,
            module: usize,
            pattern: &mut Pattern,
            bindings: &mut HashMap<String, Local>,
            seen: &mut BTreeSet<String>,
        ) -> Result<()> {
            cyallo_ast::with_stack(|| {
                if let PatternKind::Binding(name) = &pattern.kind
                    && let Some(symbol) = resolver.path(module, name, Some(Kind::Value))
                    && resolver.variant_ids.contains(&symbol.id)
                {
                    pattern.kind = PatternKind::Variant(name.clone(), None);
                }
                match &mut pattern.kind {
                    PatternKind::Binding(name) => {
                        if !seen.insert(name.to_string()) {
                            return Err(invalid_pattern(
                                PatternError::DuplicateBinding { name: name.clone() },
                                pattern.span,
                            ));
                        }
                        let target = if let Some(local) = bindings.get(name.as_ref()) {
                            local.target.clone()
                        } else {
                            let target = resolver.fresh(name);
                            bindings.insert(
                                name.to_string(),
                                Local {
                                    target: target.clone(),
                                    writable: true,
                                },
                            );
                            target
                        };
                        *name = target;
                    }
                    PatternKind::Variant(name, payload) => {
                        let symbol = resolver
                            .path(module, name, Some(Kind::Value))
                            .filter(|s| resolver.variant_ids.contains(&s.id))
                            .ok_or_else(|| {
                                invalid_pattern(
                                    PatternError::UnknownVariant { name: name.clone() },
                                    pattern.span,
                                )
                            })?;
                        resolver.check_import(module, &symbol, pattern.span)?;
                        if let Some(payload) = payload {
                            walk(resolver, module, payload, bindings, seen)?;
                        }
                        pattern.kind =
                            PatternKind::ResolvedVariant(VariantId(symbol.id), payload.take());
                    }
                    PatternKind::Tuple(values) => {
                        for value in values {
                            walk(resolver, module, value, bindings, seen)?;
                        }
                    }
                    PatternKind::Record(fields, _) => {
                        for (_, value) in fields {
                            walk(resolver, module, value, bindings, seen)?;
                        }
                    }
                    PatternKind::Or(values) => {
                        let before = seen.clone();
                        let mut expected = None;
                        for value in values {
                            let mut current = before.clone();
                            walk(resolver, module, value, bindings, &mut current)?;
                            if expected
                                .as_ref()
                                .is_some_and(|expected| expected != &current)
                            {
                                return Err(invalid_pattern(
                                    PatternError::AlternativeBindings,
                                    pattern.span,
                                ));
                            }
                            expected = Some(current);
                        }
                        if let Some(expected) = expected {
                            *seen = expected;
                        }
                    }
                    _ => {}
                }
                Ok(())
            })
        }
        walk(self, module, pattern, bindings, &mut BTreeSet::new())
    }
}
