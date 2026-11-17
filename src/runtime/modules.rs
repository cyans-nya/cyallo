//! Runtime import admission, publication and namespace construction in the same C.
use super::{
    RunResult, Session, Value,
    concurrent::CompletedOperation,
    error::{ValueKind, expected},
};
use crate::{
    dynamic_modules::CompiledLoad,
    ir::{Program, Reg},
};
use cyallo_ast::Type;
use cyallo_modules::{LoadError, PublicationError};
use std::sync::Arc;

pub(super) struct CachedNamespace {
    pub scope: super::gc::RootScope,
    pub value: super::gc::RootId,
}
fn failure(error: LoadError) -> Value {
    Value::result(Err(Box::new(Value::LoadError(Arc::new(error)))))
}

impl Session {
    pub(super) fn begin_load(
        &mut self,
        destination: Reg,
        path: &Value,
        result: &Type,
    ) -> RunResult<()> {
        let Value::String(path) = path else {
            return Err(expected(ValueKind::String));
        };
        if cyallo_ir::lookup::result_payload(result).is_none() {
            return Err(expected(ValueKind::Result));
        }
        let admitted = (|| {
            let loader = self
                .program
                .module_loader
                .clone()
                .ok_or(LoadError::NoReadGrant)?;
            let plan = self
                .program
                .dynamic_plan
                .as_ref()
                .ok_or(LoadError::Publication(PublicationError::MissingCompilePlan))?;
            let frame = self.frames.last().unwrap();
            let span = self.program.functions[frame.function].code[frame.pc - 1].span;
            let request = plan.request(span, path)?;
            Ok((loader, request))
        })();
        let (loader, request) = match admitted {
            Ok(request) => request,
            Err(error) => {
                self.set(destination, failure(error));
                return Ok(());
            }
        };
        self.ensure_concurrency()?;
        let spawner = self.import_spawner();
        let program = self.program.clone();
        // Submission is lazy: park_operation saves the continuation before the
        // first poll can register and queue any owned host work.
        self.park_operation(
            destination,
            Box::pin(async move {
                let imported = match spawner
                    .blocking(move || crate::dynamic_modules::load(program, loader, request))
                {
                    Ok(operation) => match operation.await {
                        Ok(Ok(result)) => result,
                        Ok(Err(error)) => Err(LoadError::Host(error)),
                        Err(_) => Err(LoadError::Cancelled),
                    },
                    Err(error) => Err(LoadError::Host(error)),
                };
                Ok(CompletedOperation::Load(imported))
            }),
        );
        Ok(())
    }

    pub(super) fn finish_load(
        &mut self,
        destination: Reg,
        imported: Result<CompiledLoad, LoadError>,
    ) -> RunResult<()> {
        let imported = match imported {
            Ok(imported) => imported,
            Err(error) => {
                self.set(destination, failure(error));
                return Ok(());
            }
        };
        if let Some(program) = imported.program {
            self.publish_code(program)?;
        }
        if let Some(cached) = self.module_namespaces.get(&imported.key) {
            self.set(
                destination,
                Value::result(Ok(Box::new(cached.scope.value_copy(cached.value)))),
            );
            return Ok(());
        }
        self.call(imported.factory, vec![], Some(destination))?;
        self.frames.last_mut().unwrap().namespace = Some(imported.key);
        for initializer in imported.initializers.into_iter().rev() {
            if self.initialized_modules.insert(initializer) {
                self.call(initializer, vec![], None)?;
            }
        }
        Ok(())
    }

    pub(super) fn publish_code(&mut self, mut program: Arc<Program>) -> RunResult<()> {
        if let Some(observer) = program.inspection.clone() {
            Arc::make_mut(&mut program).snapshot_id = observer.next_snapshot();
        }
        if program.requires_tracing && !self.gc_config.enabled {
            self.gc_config.enabled = true;
            self.context.gc.enabled = true;
            self.collector.reserve(self.heap.len());
            self.free_cells.clear();
            self.parked_children.clear();
            self.cell_owners.clear();
            self.register_globals();
            for frame in &mut self.frames {
                let function = &self.program.functions[frame.function];
                // SAFETY: stopped frames retain their fixed slot allocations.
                frame.roots = unsafe {
                    self.collector
                        .frame(&frame.locals, &frame.registers, &function.gc.registers)
                };
            }
        }
        let staged = self.root_scope();
        let mut roots: Vec<_> = self.globals.iter().map(|cell| staged.cell(*cell)).collect();
        // New record references resolve against the admitted snapshot. Existing
        // frames keep identical IDs/layouts during GC; failure restores the old
        // snapshot before returning to the caller.
        self.bind_native_entries(std::ptr::null());
        self.bind_native_control(std::ptr::null(), 0, std::ptr::null(), std::ptr::null(), 1);
        let previous = std::mem::replace(&mut self.program, program.clone());
        let owner = self.allocation_owner.take();
        let allocated = (|| {
            for local in program.functions[0].locals.iter().skip(roots.len()) {
                roots.push(staged.cell(self.alloc(&local.ty)?));
            }
            Ok::<_, super::RuntimeError>(
                roots
                    .iter()
                    .map(|root| staged.cell_id(*root))
                    .collect::<Vec<_>>(),
            )
        })();
        self.allocation_owner = owner;
        self.program = previous;
        let globals = allocated?;
        self.global_roots = None;
        self.globals = globals.into();
        self.register_globals();
        if let Some(report) = &mut self.report {
            report
                .functions
                .resize(program.functions.len(), Default::default());
        }
        self.function_work.resize(program.functions.len(), 0);
        self.function_calls.resize(program.functions.len(), 0);
        self.services.relink(
            &(0..self.program.functions.len()).collect::<Vec<_>>(),
            &(0..self.program.services.keys.len()).collect::<Vec<_>>(),
            program.services.keys.len(),
        );
        self.reset_lookup_caches();
        self.program = program;
        self.program.emit_inspection();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Context, Status,
        module_loader::{ModuleLoader, ReadGrant},
    };
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };

    fn session(source: &str, queue: usize) -> Session {
        let program = crate::compile(source)
            .unwrap()
            .with_module_loader(ModuleLoader::new(
                ReadGrant::directory(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap(),
            ));
        Session::with_context(
            Arc::new(program),
            Context {
                concurrency: cyallo_scheduler::Config {
                    processors: 1,
                    blocking_threads: 1,
                    max_blocking_jobs: queue,
                    quantum: 100_000,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap()
    }
    struct Release(mpsc::Sender<()>);
    impl Drop for Release {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }
    fn hold_host(
        session: &mut Session,
    ) -> (
        Release,
        cyallo_scheduler::Operation<Result<(), cyallo_scheduler::HostError>>,
    ) {
        session.ensure_concurrency().unwrap();
        let (release, wait) = mpsc::channel();
        let (started, ready) = mpsc::channel();
        let operation = session
            .import_spawner()
            .blocking(move || {
                started.send(()).unwrap();
                let _ = wait.recv();
            })
            .unwrap();
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        (Release(release), operation)
    }
    fn drive(session: &mut Session) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while matches!(session.status(), Status::Paused | Status::Waiting) {
            session.run(1);
            if session.status() == &Status::Waiting {
                session.wait_for_wakeup(Duration::from_millis(1));
            }
            assert!(Instant::now() < deadline);
        }
        assert_eq!(session.status(), &Status::Finished);
    }
    #[test]
    fn saturated_host_queue_returns_a_typed_import_failure() {
        let mut session = session(
            "type Plugin = object { answer: fn() -> i64 }\nlet result = std::module::load(\"not-admitted.ya\")",
            1,
        );
        let (_release, _active) = hold_host(&mut session);
        let _queued = session.import_spawner().blocking(|| ()).unwrap();
        drive(&mut session);
        let index = session.program.functions[0]
            .locals
            .iter()
            .position(|local| local.display_name.as_deref() == Some("result"))
            .unwrap();
        let Ok(Err(error)) = session.read(session.globals[index]).unwrap().into_result() else {
            panic!("import result")
        };
        assert!(
            matches!(error.as_ref(),Value::LoadError(error) if matches!(error.as_ref(),LoadError::Host(cyallo_scheduler::HostError::QueueFull)))
        );
    }
    #[test]
    fn cancelling_a_coroutine_with_a_queued_import_does_not_publish() {
        let mut session = session(
            r#"
type Plugin = object { answer: fn() -> i64 }
let endpoints = std::channel::bounded<i64>(0 as usize)
let sender = endpoints.0.clone()
let worker = std::task::spawn(proc() -> i64 {
    sender.send(1).unwrap()
    let plugin = std::module::load("not-admitted.ya").unwrap()
    return plugin.answer()
})
endpoints.1.recv().unwrap()
worker.cancel()
std::io::print(worker.join().is_cancelled())
"#,
            8,
        );
        let original = session.program.clone();
        let (_release, _active) = hold_host(&mut session);
        drive(&mut session);
        assert_eq!(session.context.output, "true\n");
        assert!(Arc::ptr_eq(&original, &session.program));
    }
}
