//! Relocation of checked routines across executable link layouts.
use crate::{
    ir::{self, Op},
    module_graph::{ModuleError, module_diagnostic},
};
use std::sync::Arc;
#[derive(Debug, Clone)]
pub struct Relocation {
    pub routines: Vec<Option<usize>>,
    pub globals: Vec<Option<usize>>,
    pub implementations: Vec<Option<usize>>,
    pub keys: Vec<Option<usize>>,
    pub routine_names: Vec<Box<str>>,
    pub global_names: Vec<Box<str>>,
    pub globals_now: Arc<[ir::Local]>,
    pub sources_before: Vec<crate::module_graph::SourceFile>,
    pub sources_now: Vec<crate::module_graph::SourceFile>,
    pub source_len_before: usize,
    pub source_len_now: usize,
}
impl Relocation {
    pub fn span(&self, span: crate::Span) -> crate::Span {
        if span.start == 0 && span.end as usize == self.source_len_before {
            return crate::Span::new(0, self.source_len_now as u32);
        }
        if let Some(old) = self
            .sources_before
            .iter()
            .find(|f| span.start >= f.span.start && span.end <= f.span.end)
            && let Some(new) = self.sources_now.iter().find(|f| f.id == old.id)
        {
            return crate::Span::new(
                new.span.start + span.start - old.span.start,
                new.span.start + span.end - old.span.start,
            );
        }
        span
    }
    fn routine(&self, id: usize, span: crate::Span) -> Result<usize, crate::Diagnostic> {
        self.routines.get(id).copied().flatten().ok_or_else(|| {
            module_diagnostic(ModuleError::MissingSymbol {
                symbol: self.routine_names.get(id).cloned().unwrap_or_default(),
                span,
            })
        })
    }
    pub fn apply(&self, function: &mut ir::Function, entry: bool) -> Result<(), crate::Diagnostic> {
        for instruction in &mut function.code {
            let span = instruction.span;
            match &mut instruction.op {
                Op::Call(_, id, _) | Op::FunctionValue(_, id, _) => {
                    *id = self.routine(*id, span)?
                }
                Op::Lookup(_, crate::lookup::Operation::RcNewIn { deallocate, .. }, _) => {
                    *deallocate = self.routine(*deallocate, span)?
                }
                Op::Address(_, slot) if slot.global || entry => {
                    slot.index = if entry && slot.index >= self.globals.len() {
                        self.globals_now.len() + slot.index - self.globals.len()
                    } else {
                        self.globals
                            .get(slot.index)
                            .copied()
                            .flatten()
                            .ok_or_else(|| {
                                module_diagnostic(ModuleError::MissingSymbol {
                                    symbol: self
                                        .global_names
                                        .get(slot.index)
                                        .cloned()
                                        .unwrap_or_default(),
                                    span,
                                })
                            })?
                    };
                }
                Op::TraitBox(_, _, implementation) => {
                    *implementation = self
                        .implementations
                        .get(*implementation)
                        .copied()
                        .flatten()
                        .ok_or_else(|| {
                            module_diagnostic(ModuleError::MissingSymbol {
                                symbol: "trait implementation".into(),
                                span,
                            })
                        })?
                }
                Op::ServiceKey(_, key) => {
                    *key = self.keys.get(*key).copied().flatten().ok_or_else(|| {
                        module_diagnostic(ModuleError::MissingSymbol {
                            symbol: "service key".into(),
                            span,
                        })
                    })?
                }
                _ => {}
            }
        }
        // These locals still use the previous archive. Replacement global
        // declarations below already carry the current archive coordinates.
        for local in &mut function.locals {
            local.span = self.span(local.span)
        }
        if entry {
            let temps = function.locals.split_off(self.globals.len());
            function.locals = self.globals_now.to_vec();
            function.locals.extend(temps);
            function.local_storage = (0..function.locals.len())
                .map(ir::LocalStorage::Cell)
                .collect();
        }
        function.span = self.span(function.span);
        for instruction in &mut function.code {
            instruction.span = self.span(instruction.span)
        }
        for span in [
            &mut function.attributes.heat_span,
            &mut function.attributes.compilation_span,
            &mut function.attributes.inline_span,
        ]
        .into_iter()
        .flatten()
        {
            *span = self.span(*span)
        }
        function.gc = crate::gc_metadata::GcMetadata::build(function);
        function.resume = crate::osr::ResumeMetadata::build(function);
        Ok(())
    }
    pub fn header_calls(&self, calls: &[usize]) -> Box<[usize]> {
        calls
            .iter()
            .filter_map(|&id| self.routines.get(id).copied().flatten())
            .collect()
    }
}
#[derive(Debug, Clone)]
pub struct RelocatedBody {
    pub source: ir::Routine,
    pub relocation: Arc<Relocation>,
    pub entry: bool,
}
impl RelocatedBody {
    pub fn analyze(&self) -> ir::Function {
        let mut function = (*self.source).clone();
        if let Some(error) = &mut function.analysis_error {
            error.span = self.relocation.span(error.span);
            for (span, _) in &mut error.related {
                *span = self.relocation.span(*span)
            }
        }
        if function.analysis_error.is_none()
            && let Err(error) = self.relocation.apply(&mut function, self.entry)
        {
            function.analysis_error = Some(error)
        }
        function
    }
}
