//! Native bytecode frontend using the vendored Winch assembler, without Wasm.
use crate::{
    backend::CompileError,
    ir::Program,
    metrics::ExecutionEngine,
    runtime::{EngineError, Session, native::NativeView},
};
use cranelift::{codegen::ir::Signature, prelude::*};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module, default_libcall_names};
use std::sync::Arc;
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
mod emit;

pub(super) struct WinchUnit {
    program: Arc<Program>,
    module: Option<JITModule>,
    entry: unsafe extern "C" fn(*mut Session, *mut NativeView, u64) -> u64,
}
impl WinchUnit {
    pub(super) fn compile(program: Arc<Program>, function: usize) -> Result<Self, CompileError> {
        program.functions[function].checked()?;
        #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
        {
            let _ = (program, function);
            return Err(CompileError::UnsupportedTarget);
        }
        #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
        {
            let bytes = emit::compile(&program, function, program.next_code_version());
            let builder =
                JITBuilder::with_flags(&[("opt_level", "none")], default_libcall_names())?;
            let mut module = JITModule::new(builder);
            let ptr = module.target_config().pointer_type();
            if ptr != types::I64 {
                return Err(CompileError::PointerWidth);
            }
            let mut signature: Signature = module.make_signature();
            signature.params = vec![
                AbiParam::new(ptr),
                AbiParam::new(ptr),
                AbiParam::new(types::I64),
            ];
            signature.returns = vec![AbiParam::new(types::I64)];
            let id = module.declare_function("caie_winch_native", Linkage::Local, &signature)?;
            module.define_function_bytes(id, 32, &bytes, &[])?;
            module.finalize_definitions()?;
            // SAFETY: emit's prologue/epilogue implement this exact C ABI.
            let entry = unsafe {
                std::mem::transmute::<
                    *const u8,
                    unsafe extern "C" fn(*mut Session, *mut NativeView, u64) -> u64,
                >(module.get_finalized_function(id))
            };
            Ok(Self {
                program,
                module: Some(module),
                entry,
            })
        }
    }
    pub(super) fn entry_address(&self) -> usize {
        self.entry as usize
    }
    pub(super) fn run(&self, session: &mut Session, budget: usize) -> Result<(), EngineError> {
        if !Arc::ptr_eq(&self.program, &session.program) {
            if session.program.extends(&self.program) {
                session.run(budget);
                return Ok(());
            }

            return Err(EngineError::ForeignProgram);
        }
        if budget == 0 {
            return Ok(());
        }
        let previous = session.select_execution_engine(ExecutionEngine::Winch);
        session.enter_execution();
        if !session.poll_concurrent() {
            session.select_execution_engine(previous);
            session.leave_execution();
            return Ok(());
        }
        session.reset_native_exit();
        let view = session.prepare_native();
        // SAFETY: code and view are pinned by exclusive execution/artifact borrows.
        unsafe {
            (self.entry)(session, view, budget as u64);
        }
        session.finish_native();
        session.observe_native_exit();
        session.pause_reporting();
        session.select_execution_engine(previous);
        session.leave_execution();
        Ok(())
    }
}
impl Drop for WinchUnit {
    fn drop(&mut self) {
        if let Some(module) = self.module.take() {
            unsafe {
                module.free_memory();
            }
        }
    }
}
