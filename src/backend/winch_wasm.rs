//! Winch's supported input is Wasm; the register-bytecode adapter supplies per-routine modules.
use super::CompileError;
use super::abi;
use crate::{
    ir::Program,
    runtime::{EngineError, Session},
};
use std::sync::Arc;
use wasmtime::{Caller, Config, Engine, Linker, Module, Store, Strategy, TypedFunc};
/// Each artifact owns its Wasmtime engine; nothing is shared across programs.
fn engine() -> Result<Engine, CompileError> {
    let mut config = Config::new();
    config.strategy(Strategy::Winch);
    Ok(Engine::new(&config)?)
}
pub(super) struct WinchUnit {
    store: Store<usize>,
    entry: TypedFunc<i64, ()>,
}
impl WinchUnit {
    fn compile(
        engine: &Engine,
        program: Arc<Program>,
        function: usize,
    ) -> Result<Self, CompileError> {
        let wasm = program.bytecode.functions[function]
            .wasm_text(function, &program.functions[function].registers);
        let module = Module::new(engine, wasm)?;
        let mut linker = Linker::<usize>::new(engine);
        // The slot holds an exclusively borrowed Session only during run(). It is cleared
        // before run returns, including trap paths; no guest memory contains a host pointer.
        linker.func_wrap("caie", "state", |caller: Caller<'_, usize>| unsafe {
            abi::state(*caller.data() as *mut Session)
        })?;
        linker.func_wrap("caie", "step", |caller: Caller<'_, usize>| unsafe {
            abi::step(*caller.data() as *mut Session)
        })?;
        linker.func_wrap("caie", "get", |caller: Caller<'_, usize>, r: i64| unsafe {
            abi::get(*caller.data() as *mut Session, r)
        })?;
        linker.func_wrap(
            "caie",
            "put",
            |caller: Caller<'_, usize>, r: i64, bits: i64, overflow: i64| unsafe {
                abi::put(*caller.data() as *mut Session, r, bits, overflow)
            },
        )?;
        let mut store = Store::new(engine, 0);
        let instance = linker.instantiate(&mut store, &module)?;
        let entry = instance.get_typed_func::<i64, ()>(&mut store, "run")?;
        Ok(Self { store, entry })
    }
    fn run(&mut self, session: &mut Session, budget: usize) -> Result<(), EngineError> {
        let previous = session.select_execution_engine(crate::metrics::ExecutionEngine::Winch);
        *self.store.data_mut() = session as *mut Session as usize;
        let result = self.entry.call(&mut self.store, budget as i64);
        *self.store.data_mut() = 0;
        session.pause_reporting();
        session.select_execution_engine(previous);
        // Helpers report guest failures through the Session; a trap is an engine fault.
        result.map_err(|_| EngineError::Trap)
    }
}

/// Legacy Wasmtime adapter, retained for fixed-tier comparisons only.
pub struct WasmBackend;
pub struct WasmArtifact {
    program: Arc<Program>,
    units: std::cell::RefCell<Vec<Option<WinchUnit>>>,
}
impl super::NativeBackend for WasmBackend {
    type Artifact = WasmArtifact;
    fn compile(&self, program: Arc<Program>) -> Result<WasmArtifact, CompileError> {
        program.validate_all()?;
        let engine = engine()?;
        let units = (0..program.functions.len())
            .map(|fi| {
                if program.functions[fi].attributes.compilation
                    == cyallo_ast::CompileHint::AlwaysLazy
                {
                    Ok(None)
                } else {
                    WinchUnit::compile(&engine, program.clone(), fi).map(Some)
                }
            })
            .collect::<Result<_, _>>()?;
        Ok(WasmArtifact {
            program,
            units: std::cell::RefCell::new(units),
        })
    }
}
impl super::Executable for WasmArtifact {
    fn run(&self, session: &mut Session, budget: usize) -> Result<(), EngineError> {
        if !Arc::ptr_eq(&self.program, &session.program) {
            return Err(EngineError::ForeignProgram);
        }
        let start = session.instruction_count();
        while session.instruction_count() - start < budget as u64 {
            if matches!(
                session.status(),
                crate::Status::Finished | crate::Status::Failed(_)
            ) || (*session.status() == crate::Status::WaitingForInput
                && session.context.input.is_empty())
            {
                break;
            }
            let Some(fi) = session.active_function() else {
                break;
            };
            let remaining = budget as u64 - (session.instruction_count() - start);
            if let Some(unit) = &mut self.units.borrow_mut()[fi] {
                unit.run(session, remaining.min(1024) as usize)?;
            } else {
                session.step();
            }
        }
        Ok(())
    }
}
