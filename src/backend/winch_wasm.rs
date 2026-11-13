//! Winch's supported input is Wasm; the register-bytecode adapter supplies per-routine modules.
use super::abi;
use crate::{ir::Program, runtime::Session};
use std::sync::{Arc, OnceLock};
use wasmtime::{Caller, Config, Engine, Linker, Module, Store, Strategy, TypedFunc};
fn engine() -> Result<Engine, String> {
    static ENGINE: OnceLock<Result<Engine, String>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let mut config = Config::new();
            config.strategy(Strategy::Winch);
            Engine::new(&config).map_err(|e| e.to_string())
        })
        .clone()
}
pub(super) struct WinchUnit {
    store: Store<usize>,
    entry: TypedFunc<i64, ()>,
}
impl WinchUnit {
    pub(super) fn compile(program: Arc<Program>, function: usize) -> Result<Self, String> {
        let engine = engine()?;
        let wasm = program.bytecode.functions[function]
            .wasm_text(function, &program.functions[function].registers);
        let module = Module::new(&engine, wasm).map_err(|e| e.to_string())?;
        let mut linker = Linker::<usize>::new(&engine);
        // The slot holds an exclusively borrowed Session only during run(). It is cleared
        // before run returns, including trap paths; no guest memory contains a host pointer.
        linker
            .func_wrap("caie", "state", |caller: Caller<'_, usize>| unsafe {
                abi::state(*caller.data() as *mut Session)
            })
            .map_err(|e| e.to_string())?;
        linker
            .func_wrap("caie", "step", |caller: Caller<'_, usize>| unsafe {
                abi::step(*caller.data() as *mut Session)
            })
            .map_err(|e| e.to_string())?;
        linker
            .func_wrap("caie", "get", |caller: Caller<'_, usize>, r: i64| unsafe {
                abi::get(*caller.data() as *mut Session, r)
            })
            .map_err(|e| e.to_string())?;
        linker
            .func_wrap(
                "caie",
                "put",
                |caller: Caller<'_, usize>, r: i64, bits: i64, overflow: i64| unsafe {
                    abi::put(*caller.data() as *mut Session, r, bits, overflow)
                },
            )
            .map_err(|e| e.to_string())?;
        let mut store = Store::new(&engine, 0);
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| e.to_string())?;
        let entry = instance
            .get_typed_func::<i64, ()>(&mut store, "run")
            .map_err(|e| e.to_string())?;
        Ok(Self { store, entry })
    }
    pub(super) fn run(&mut self, session: &mut Session, budget: usize) -> Result<(), String> {
        let previous = session.select_execution_engine(crate::metrics::ExecutionEngine::Winch);
        *self.store.data_mut() = session as *mut Session as usize;
        let result = self.entry.call(&mut self.store, budget as i64);
        *self.store.data_mut() = 0;
        session.pause_reporting();
        session.select_execution_engine(previous);
        result.map_err(|e| e.to_string())
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
    fn compile(&self, program: Arc<Program>) -> Result<WasmArtifact, String> {
        program.validate_all().map_err(|e| e.to_string())?;
        let units = (0..program.functions.len())
            .map(|fi| {
                if program.functions[fi].attributes.compilation
                    == cyallo_ast::CompileHint::AlwaysLazy
                {
                    Ok(None)
                } else {
                    WinchUnit::compile(program.clone(), fi).map(Some)
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
    fn run(&self, session: &mut Session, budget: usize) -> Result<(), String> {
        if !Arc::ptr_eq(&self.program, &session.program) {
            return Err("artifact belongs to a different program".into());
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
