//! Validates an extension and freezes existing code/identity mappings.
use crate::ir::{self, Op, Routine, Slot};
use cyallo_ast::Type;
use cyallo_modules::{LoadError, PublicationError as P};
use std::{collections::HashMap, path::PathBuf, sync::Arc};

pub(super) fn publish(
    old: &Arc<ir::Program>,
    compiled: ir::Program,
    factory_name: &str,
) -> Result<(ir::Program, Option<usize>, usize), LoadError> {
    let fail = |error| LoadError::Publication(error);
    if old
        .records
        .iter()
        .any(|(name, ty)| compiled.records.get(name) != Some(ty))
    {
        return Err(fail(P::ChangedExistingRecord));
    }
    if compiled.services.traits.len() < old.services.traits.len() {
        return Err(fail(P::ChangedExistingTrait));
    }
    if compiled.services.implementations.len() < old.services.implementations.len() {
        return Err(fail(P::ChangedExistingImplementation));
    }
    let mut keys = old.services.keys.clone();
    let mut key_remap = Vec::new();
    for key in &compiled.services.keys {
        let index = if let Some(index) = keys.iter().position(|old| old == key) {
            index
        } else {
            let index = keys.len();
            keys.push(key.clone());
            index
        };
        key_remap.push(index);
    }
    if old
        .services
        .keys
        .iter()
        .any(|key| !compiled.services.keys.contains(key))
    {
        return Err(fail(P::ChangedExistingKey));
    }
    let mut functions = old.functions.clone();
    let mut routines = HashMap::new();
    for (index, routine) in functions.iter().enumerate() {
        routines.insert(routine.symbol.clone(), index);
    }
    let mut remap = Vec::new();
    for routine in &compiled.functions {
        let next = routines.len();
        let index = *routines.entry(routine.symbol.clone()).or_insert(next);
        remap.push(index);
    }
    let mut main = old.functions[0]
        .checked()
        .map_err(|diagnostic| LoadError::Check {
            path: PathBuf::new(),
            diagnostic: diagnostic.clone(),
        })?
        .clone();
    let mut globals = Vec::new();
    for local in &compiled.functions[0].locals {
        if let Some(index) = main.locals.iter().position(|old| old.name == local.name) {
            if main.locals[index].ty != local.ty || main.locals[index].constant != local.constant {
                return Err(fail(P::ChangedExistingGlobal));
            }
            globals.push(index);
        } else {
            globals.push(main.locals.len());
            main.locals.push(local.clone());
        }
    }
    let old_globals = old.functions[0].locals.len();
    let mut storage = main.local_storage.to_vec();
    let cells = storage
        .iter()
        .filter(|s| matches!(s, ir::LocalStorage::Cell(_)))
        .count();
    storage.extend(
        (0..main.locals.len() - old_globals).map(|index| ir::LocalStorage::Cell(cells + index)),
    );
    main.local_storage = storage.into_boxed_slice();
    functions[0] = Routine::ready(main);
    let map = |function: &mut ir::Function| {
        for instruction in &mut function.code {
            match &mut instruction.op {
                Op::Call(_, id, _) | Op::FunctionValue(_, id, _) => *id = remap[*id],
                Op::Address(_, slot) if slot.global => slot.index = globals[slot.index],
                Op::ServiceKey(_, key) => *key = key_remap[*key],
                Op::Lookup(_, cyallo_ir::lookup::Operation::RcNewIn { deallocate, .. }, _) => {
                    *deallocate = remap[*deallocate]
                }
                _ => {}
            }
        }
    };
    for (index, routine) in compiled.functions.iter().enumerate().skip(1) {
        if remap[index] < old.functions.len() {
            continue;
        }
        let mut function = routine
            .checked()
            .map_err(|diagnostic| LoadError::Check {
                path: PathBuf::new(),
                diagnostic: diagnostic.clone(),
            })?
            .clone();
        map(&mut function);
        if functions.len() != remap[index] {
            return Err(fail(P::MissingExistingRoutine));
        }
        functions.push(Routine::ready(function));
    }
    let factory = *routines
        .get(factory_name)
        .ok_or(fail(P::MissingExistingRoutine))?;
    let source = &compiled.functions[0];
    let mut initializer = source
        .checked()
        .map_err(|diagnostic| LoadError::Check {
            path: PathBuf::new(),
            diagnostic: diagnostic.clone(),
        })?
        .clone();
    initializer.symbol = format!("$dynamicImportInit{}", functions.len()).into();
    initializer.name = "dynamic module initialization".into();
    initializer.locals.clear();
    initializer.local_storage = Box::default();
    initializer.params.clear();
    initializer.code.clear();
    initializer.result = Type::Void;
    let mut include = false;
    for instruction in &source.code {
        if matches!(instruction.op, Op::Statement) {
            include = source.locals.iter().enumerate().any(|(index, local)| {
                globals[index] >= old_globals && local.span == instruction.span
            });
        }
        if include {
            let mut instruction = instruction.clone();
            match &mut instruction.op {
                Op::Return(_) => {
                    include = false;
                    continue;
                }
                Op::Jump(_) | Op::Branch(..) | Op::FrameLoad(..) | Op::FrameStore(..) => {
                    return Err(fail(P::InvalidInitializer));
                }
                Op::Address(_, slot) => {
                    *slot = Slot {
                        global: true,
                        index: slot.index,
                    };
                }
                _ => {}
            }
            initializer.code.push(instruction);
        }
    }
    map(&mut initializer);
    initializer.code.push(ir::Instruction {
        op: Op::Return(None),
        span: initializer.span,
    });
    initializer.gc = cyallo_ir::gc_metadata::GcMetadata::build(&initializer);
    initializer.resume = cyallo_ir::osr::ResumeMetadata::build(&initializer);
    let entry = if initializer.code.len() > 1 {
        let entry = functions.len();
        functions.push(Routine::ready(initializer));
        Some(entry)
    } else {
        None
    };
    let mut extension = old.as_ref().clone();
    let mut services = compiled.services.as_ref().clone();
    for implementation in &mut services.implementations {
        for method in &mut implementation.methods {
            *method = remap[*method];
        }
    }
    if !services.traits.starts_with(&old.services.traits) {
        return Err(fail(P::ChangedExistingTrait));
    }
    if !services
        .implementations
        .starts_with(&old.services.implementations)
    {
        return Err(fail(P::ChangedExistingImplementation));
    }
    services.keys = keys;
    extension.services = Arc::new(services);
    extension.records = compiled.records;
    extension.requires_tracing |= compiled.requires_tracing;
    extension.source = compiled.source;
    extension.functions = functions;
    extension.bytecode = crate::bytecode::Module::lower(&extension.functions, extension.jobs);
    extension.parent = Some(old.clone());
    Ok((extension, entry, factory))
}
