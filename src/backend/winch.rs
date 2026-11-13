//! Native bytecode frontend using the vendored Winch assembler, without Wasm.
use crate::{
    ir::Program,
    metrics::ExecutionEngine,
    runtime::{Session, native::NativeView},
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
    pub(super) fn compile(program: Arc<Program>, function: usize) -> Result<Self, String> {
        program.functions[function]
            .checked()
            .map_err(ToString::to_string)?;
        #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
        {
            let _ = (program, function);
            return Err("native Winch frontend does not support this target".into());
        }
        #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
        {
            let bytes = emit::compile(&program, function, super::next_code_version())?;
            let builder = JITBuilder::with_flags(&[("opt_level", "none")], default_libcall_names())
                .map_err(|e| e.to_string())?;
            let mut module = JITModule::new(builder);
            let ptr = module.target_config().pointer_type();
            if ptr != types::I64 {
                return Err("native baseline ABI requires 64-bit pointers".into());
            }
            let mut signature: Signature = module.make_signature();
            signature.params = vec![
                AbiParam::new(ptr),
                AbiParam::new(ptr),
                AbiParam::new(types::I64),
            ];
            signature.returns = vec![AbiParam::new(types::I64)];
            let id = module
                .declare_function("caie_winch_native", Linkage::Local, &signature)
                .map_err(|e| e.to_string())?;
            module
                .define_function_bytes(id, 32, &bytes, &[])
                .map_err(|e| e.to_string())?;
            module.finalize_definitions().map_err(|e| e.to_string())?;
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
    pub(super) fn run(&self, session: &mut Session, budget: usize) -> Result<(), String> {
        if !Arc::ptr_eq(&self.program, &session.program) {
            return Err("artifact belongs to a different program".into());
        }
        if budget == 0 {
            return Ok(());
        }
        let previous = session.select_execution_engine(ExecutionEngine::Winch);
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
