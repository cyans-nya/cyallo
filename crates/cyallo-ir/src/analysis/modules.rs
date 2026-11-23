//! Independent checked module jobs and executable linking.
use super::*;
use cyallo_startup_profile::{Counter, Stage, count, stage};

/// A checked module is retained after linking. Its routines refer to the frozen
/// declaration catalogue; only the executable linker assigns storage/entry work.
#[derive(Debug, Clone)]
pub struct CheckedModule {
    pub build: ModuleBuild,
    pub id: crate::module_graph::ModuleId,
    pub path: Box<str>,
    pub bindings: Vec<crate::module_graph::ModuleBinding>,
    pub globals: Vec<usize>,
    pub initializer: Option<usize>,
    pub routines: Vec<(usize, ir::Routine)>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleBuild {
    Compiled,
    Reused,
}
#[derive(Debug, Clone)]
pub struct CheckedProgram {
    pub modules: Vec<std::sync::Arc<CheckedModule>>,
    pub executable: ir::Module,
}

pub fn analyze_modules(
    source_len: usize,
    graph: &mut crate::module_graph::ResolvedProgram,
    options: CompileOptions,
    deferred: bool,
) -> Result<CheckedProgram> {
    // No cache survives this call, so no key could ever match or be looked up:
    // keys are not computed at all.
    analyze_modules_with(source_len, graph, options, deferred, None)
}
pub fn analyze_modules_cached(
    source_len: usize,
    graph: &mut crate::module_graph::ResolvedProgram,
    options: CompileOptions,
    deferred: bool,
    cache: &crate::module_cache::ModuleCache,
) -> Result<CheckedProgram> {
    analyze_modules_with(source_len, graph, options, deferred, Some(cache))
}
fn analyze_modules_with(
    source_len: usize,
    graph: &mut crate::module_graph::ResolvedProgram,
    options: CompileOptions,
    deferred: bool,
    cache: Option<&crate::module_cache::ModuleCache>,
) -> Result<CheckedProgram> {
    let _analysis = stage(Stage::ModuleAnalysis);
    let identity = crate::module_graph::ResolvedProgram {
        worlds: graph.worlds.clone(),
        interface: graph.interface.clone(),
        interface_declarations: graph.interface_declarations.clone(),
        source_files: graph.source_files.clone(),
        modules: graph
            .modules
            .iter()
            .map(|m| crate::module_graph::ResolvedModule {
                id: m.id,
                path: m.path.clone(),
                source: Ast::default(),
                bindings: m.bindings.clone(),
            })
            .collect(),
        entry: graph.entry,
        native: graph.native.clone(),
        source_names: graph.source_names.clone(),
        exports: graph.exports.clone(),
        indexed_bodies: graph.indexed_bodies.clone(),
    };
    let entry = graph.entry_index();
    let native = graph.native.clone();
    let source_names = graph.source_names.clone();
    graph
        .with_sources(|asts| {
            let declarations = stage(Stage::ModuleDeclarations);
            let mut types = crate::declarations::TypeEnvironment::new(
                crate::composites::resolve_modules(asts, &native)?,
                native,
            );
            let records = std::sync::Arc::new(types.record_types());
            let (mut metadata, method_symbols) = crate::services::prepare_modules(asts)?;
            for t in &metadata.traits {
                for m in &t.methods {
                    types.insert(
                        format!("$METHOD${}.{}", t.name, m.name),
                        Type::Function(m.kind, m.params.clone(), Box::new(m.result.clone())),
                    );
                }
            }
            for imp in &metadata.implementations {
                for m in &metadata.traits[imp.trait_id].methods {
                    let name = format!("$CONCRETE${:?}.{}", imp.receiver, m.name);
                    let ty = Type::Function(m.kind, m.params.clone(), Box::new(m.result.clone()));
                    if types.insert(name.clone(), ty).is_some() {
                        types.insert(name, Type::Void);
                    }
                }
            }
            types.extend(
                asts.iter()
                    .flat_map(|a| a.functions.iter())
                    .map(|f| (key(&f.name), crate::composites::function_type(f))),
            );
            let by_module = crate::module_graph::infer_globals(asts, &types, options)?;
            let globals: Vec<_> = by_module.iter().flatten().cloned().collect();
            for (module, ast) in asts.iter_mut().enumerate() {
                crate::native_thunk::rewrite_with_globals(ast, &types, options, &globals)?;
                crate::composites::lift_module(
                    ast,
                    &globals,
                    &types,
                    options,
                    &identity.modules[module].path,
                )?;
            }
            types.extend(
                asts.iter()
                    .flat_map(|a| a.functions.iter())
                    .map(|f| (key(&f.name), crate::composites::function_type(f))),
            );
            let by_module = crate::module_graph::infer_globals(asts, &types, options)?;
            let mut globals: Vec<_> = by_module.iter().flatten().cloned().collect();
            for l in &mut globals {
                if let Some(name) = source_names.get(&l.name) {
                    l.display_name = Some(name.clone());
                }
            }
            // All inference failures are now reported in defining-module order.
            for (ast, locals) in asts.iter().zip(&by_module) {
                let mut locals = locals.clone();
                crate::inference::infer(&ast.body, &mut locals, &globals, &types, options)?;
            }
            let mut signatures = HashMap::new();
            let mut headers = vec![];
            let mut routine_ids = vec![];
            for ast in asts.iter() {
                let mut ids = vec![];
                for f in &ast.functions {
                    collect(&[], &f.params)?;
                    if signatures.contains_key(&key(&f.name))
                        || globals.iter().any(|l| l.name == f.name)
                    {
                        return Err(crate::module_graph::module_diagnostic(
                            crate::module_graph::ModuleError::DuplicateSymbol {
                                symbol: f.name.clone(),
                                span: f.span,
                            },
                        ));
                    }
                    if f.kind == RoutineKind::Function
                        && f.origin == RoutineOrigin::Source
                        && f.params.iter().any(|p| p.by_ref)
                    {
                        return Err(crate::module_graph::module_diagnostic(
                            crate::module_graph::ModuleError::FunctionReferenceParameter {
                                span: f.span,
                            },
                        ));
                    }
                    let id = headers.len() + 1;
                    signatures.insert(
                        key(&f.name),
                        Signature {
                            kind: f.kind,
                            params: f.params.clone(),
                            result: f.result.clone(),
                            id,
                        },
                    );
                    headers.push(blank_routine(f));
                    ids.push(id);
                }
                routine_ids.push(ids);
            }
            for (imp, targets) in metadata.implementations.iter_mut().zip(method_symbols) {
                imp.methods = targets
                    .iter()
                    .map(|target| signatures[target.as_ref()].id)
                    .collect();
            }
            let services = std::sync::Arc::new(metadata);
            drop(declarations);
            let initializers: Vec<_> = asts
                .iter()
                .enumerate()
                .filter(|(i, a)| *i != entry && !a.body.is_empty())
                .map(|(i, _)| i)
                .collect();
            let source_count = headers.len();
            let mut init_ids = HashMap::new();
            for (offset, &module) in initializers.iter().enumerate() {
                let id = source_count + offset + 1;
                init_ids.insert(module, id);
                let mut header = empty_entry(source_len);
                header.symbol = format!("$module_init{}", module).into();
                header.name = "module initialization".into();
                headers.push(header);
            }
            let context = std::sync::Arc::new(DeferredContext {
                source_names,
                globals: globals.clone(),
                signatures,
                routine_types: types,
                options,
                services: services.clone(),
                headers,
            });
            // Each generic template is checked once, with its type parameters opaque, before
            // any instance: a parameter's type tells the body nothing about the instance.
            {
                let mut templates: Vec<&IndexedRoutineBody> = identity
                    .indexed_bodies
                    .values()
                    .filter(|body| body.is_generic())
                    .collect();
                templates.sort_by_key(|body| body.placed_span().start);
                for body in templates {
                    body.check_generic(
                        &context.signatures,
                        &context.source_names,
                        &context.routine_types,
                    )?;
                }
            }
            let layout = std::sync::Arc::new(crate::module_cache::LinkLayout {
                routines: std::iter::once("<main>".into())
                    .chain(context.headers.iter().map(|h| h.symbol.clone()))
                    .collect(),
                globals: globals.clone(),
                services: services.clone(),
                source_files: identity.source_files.clone(),
                source_len,
            });
            let keys = match cache {
                Some(_) => {
                    let _keys = stage(Stage::ModuleKeys);
                    crate::module_cache::keys(
                        &identity,
                        asts,
                        &context.routine_types,
                        &globals,
                        &services,
                    )
                }
                None => vec![],
            };
            let cached: Vec<_> = match cache {
                Some(cache) => keys.iter().map(|key| cache.get(key, &layout)).collect(),
                None => vec![None; asts.len()],
            };
            // One wave spans all routine bodies, so a large single module can
            // use every worker while results retain module ownership and order.
            #[derive(Clone, Copy)]
            enum BodyJob {
                Initializer,
                Routine(usize),
            }
            let mut work = vec![];
            for (module, ast) in asts.iter().enumerate() {
                if cached[module].is_some() {
                    continue;
                }
                if module == entry || init_ids.contains_key(&module) {
                    work.push((module, BodyJob::Initializer));
                }
                work.extend(
                    (0..ast.functions.len()).map(|function| (module, BodyJob::Routine(function))),
                );
            }
            let wave = stage(Stage::ModuleBodyWave);
            let lowered = crate::Jobs(options.jobs).map(
                &work,
                2,
                |_, _, &(module, job)| -> Result<(usize, usize, ir::Routine)> {
                    let ast = &asts[module];
                    // Lower only this module's initializer. Global slots are references
                    // into the link layout; there is no imported statement list here.
                    if let BodyJob::Initializer = job {
                        let _entry = stage(Stage::EntryLowering);
                        let id = if module == entry {
                            0
                        } else {
                            init_ids[&module]
                        };
                        let mut function = empty_entry(source_len);
                        if module == entry {
                            function.locals = globals.clone();
                        } else {
                            function.symbol = format!("$module_init{module}").into();
                            function.name = "module initialization".into();
                        }
                        let mut lower = Lower {
                            guard_aliases: HashMap::new(),
                            function,
                            globals: &globals,
                            signatures: &context.signatures,
                            types: &context.routine_types,
                            services: &services,
                            span: Span::default(),
                            options,
                            loops: vec![],
                            moves: Default::default(),
                            entry: true,
                            block_depth: 0,
                            drop_scopes: vec![],
                        };
                        lower.moves = crate::ownership::check(
                            &ast.body,
                            &globals,
                            &[],
                            &context.routine_types,
                            &[],
                            false,
                            &Type::Void,
                        )?;
                        lower.block(&ast.body)?;
                        lower.emit(Op::Return(None));
                        let mut function = lower.function;
                        crate::effects::analyze_one(
                            &mut function,
                            id,
                            &globals,
                            &context.headers,
                            &services,
                        )?;
                        conservative_calls(&mut function);
                        finish_lowering(&mut function, id == 0);
                        return Ok((module, id, ir::Routine::ready(function)));
                    }

                    let BodyJob::Routine(function) = job else {
                        unreachable!()
                    };
                    let _setup = stage(Stage::DeferredSetup);
                    let f = &ast.functions[function];
                    let id = routine_ids[module][function];
                    let indexed = identity.indexed_bodies.get(&f.name);
                    // A body with reference sites keeps its owned body, whose estimate
                    // also records its direct-call edges for admission.
                    let owned_body = indexed.is_none() || !f.body.is_empty();
                    let estimate = indexed
                        .filter(|_| !owned_body)
                        .map(IndexedRoutineBody::estimate)
                        .unwrap_or_else(|| crate::admission::estimate(f));
                    let body = DeferredBody {
                        function: f.clone(),
                        indexed: indexed.cloned(),
                        context: context.clone(),
                        index: id,
                    };
                    let routine = if deferred {
                        if owned_body {
                            count(Counter::BodiesDeferredCopied, 1);
                        }
                        ir::Routine::deferred(
                            body,
                            estimate.eager_candidate(),
                            estimate.shape,
                            estimate.nodes.saturating_mul(2),
                            estimate
                                .calls
                                .iter()
                                .filter_map(|n| context.signatures.get(n.as_ref()).map(|s| s.id))
                                .collect(),
                        )
                    } else {
                        let function = body.analyze();
                        if let Some(error) = &function.analysis_error {
                            return Err(error.clone());
                        }
                        ir::Routine::ready(function)
                    };

                    Ok((module, id, routine))
                },
            );
            drop(wave);
            let _link = stage(Stage::ModuleLink);
            let mut owned: Vec<Vec<_>> = cached
                .iter()
                .map(|unit| {
                    unit.as_ref()
                        .map_or_else(Vec::new, |unit| unit.routines.clone())
                })
                .collect();
            for result in lowered {
                let (module, id, routine) = result?;
                owned[module].push((id, routine));
            }
            let mut routines: Vec<Option<ir::Routine>> = vec![None; context.headers.len() + 1];
            let mut modules = vec![];
            let mut global_base = 0;
            for (i, unit) in owned.into_iter().enumerate() {
                for (id, routine) in &unit {
                    routines[*id] = Some(routine.clone());
                }
                let count = by_module[i].len();
                modules.push((
                    unit,
                    (global_base..global_base + count).collect::<Vec<_>>(),
                    init_ids.get(&i).copied(),
                ));
                global_base += count;
            }
            let mut functions: Vec<_> = routines
                .into_iter()
                .map(|r| r.expect("assigned routine slot"))
                .collect();
            // Link initialization calls as IR. Imported AST bodies never enter main.
            let mut main = functions[0].checked().map_err(Clone::clone)?.clone();
            let prefix = initializers.len();
            for instruction in &mut main.code {
                match &mut instruction.op {
                    Op::Jump(to) => *to += prefix,
                    Op::Branch(_, yes, no) => {
                        *yes += prefix;
                        *no += prefix
                    }
                    _ => {}
                }
            }
            let mut code: Vec<_> = initializers
                .iter()
                .map(|m| ir::Instruction {
                    op: Op::Call(None, init_ids[m], vec![]),
                    span: Span::default(),
                })
                .collect();
            code.append(&mut main.code);
            main.code = code;
            main.gc = crate::gc_metadata::GcMetadata::build(&main);
            main.resume = crate::osr::ResumeMetadata::build(&main);
            functions[0] = ir::Routine::ready(main);
            if !deferred {
                let mut checked: Vec<_> = functions
                    .iter()
                    .map(|r| r.checked().cloned().map_err(Clone::clone))
                    .collect::<Result<_>>()?;
                crate::effects::analyze(&mut checked, &services)?;
                functions = checked.into_iter().map(ir::Routine::ready).collect();
            }
            let requires_tracing = {
                let _tracing = stage(Stage::RequiresTracing);
                asts.iter().any(requires_tracing)
            };
            Ok((
                ir::Module {
                    records,
                    requires_tracing,
                    services,
                    functions,
                },
                modules,
                keys,
                layout,
                cached,
            ))
        })
        .map(|(executable, units, keys, layout, cached)| {
            let modules = graph
                .modules
                .iter()
                .zip(units)
                .enumerate()
                .map(|(i, (m, (routines, globals, initializer)))| {
                    let unit = std::sync::Arc::new(CheckedModule {
                        build: if cached[i].is_some() {
                            ModuleBuild::Reused
                        } else {
                            ModuleBuild::Compiled
                        },
                        id: m.id,
                        path: m.path.clone(),
                        bindings: m.bindings.clone(),
                        globals,
                        initializer,
                        routines,
                    });
                    if let (Some(cache), None) = (cache, &cached[i]) {
                        cache.insert(keys[i].clone(), unit.clone(), layout.clone());
                    }
                    unit
                })
                .collect();
            CheckedProgram {
                modules,
                executable,
            }
        })
}
fn empty_entry(source_len: usize) -> ir::Function {
    ir::Function {
        generator: None,
        symbol: "<main>".into(),
        origin: RoutineOrigin::Source,
        attributes: Default::default(),
        analysis_error: None,
        resume: Default::default(),
        gc: Default::default(),
        kind: RoutineKind::Procedure,
        span: Span::new(0, source_len as u32),
        pure: false,
        effects: Default::default(),
        name: "<main>".into(),
        locals: vec![],
        local_storage: Default::default(),
        params: vec![],
        result: Type::Void,
        registers: vec![],
        code: vec![],
    }
}
fn requires_tracing(ast: &Ast) -> bool {
    let mut found = false;
    let mut visit = |body: &[Stmt]| {
        // Read-only: bodies are inspected in place, never copied.
        cyallo_ast::visit::inspect_block(
            body,
            &mut |e| {
                found |= matches!(
                    e.kind,
                    ExprKind::ManagedNew(..) | ExprKind::ObjectLiteral(..)
                ) || matches!(&e.kind, ExprKind::IntrinsicCall(id, ..) if Intrinsic::from_id(*id) == Some(Intrinsic::RcTryNewIn));
            },
            &mut |_, _| {},
        );
    };
    visit(&ast.body);
    for f in &ast.functions {
        visit(&f.body)
    }
    found
}
