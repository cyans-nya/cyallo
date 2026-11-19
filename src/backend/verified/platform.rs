//! Executable mapping and the scalar entry thunk for the host platform.

use super::{ValidatedImage, abi::ScalarContext};
use cranelift::prelude::{AbiParam, types};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Module, ModuleError, default_libcall_names};

type ScalarEntry = unsafe extern "C" fn(*mut ScalarContext, *const (), *mut u64) -> u64;

struct Allocation(Option<JITModule>);

impl Drop for Allocation {
    fn drop(&mut self) {
        if let Some(module) = self.0.take() {
            // SAFETY: the module is owned by the allocation. Published entries
            // keep it pinned during every invocation and never expose its thunk.
            unsafe { module.free_memory() };
        }
    }
}

pub(super) struct Executable {
    _allocation: Allocation,
    entry: ScalarEntry,
    address: usize,
}

impl Executable {
    pub(super) fn map(image: &ValidatedImage) -> Result<Self, Box<ModuleError>> {
        let builder = JITBuilder::new(default_libcall_names())?;
        let mut allocation = Allocation(Some(JITModule::new(builder)));
        let Some(module) = &mut allocation.0 else {
            unreachable!("new executable allocation")
        };
        let mut signature = module.make_signature();
        let pointer_type = module.target_config().pointer_type();
        for _ in [
            super::AbiArgument::Context,
            super::AbiArgument::Environment,
            super::AbiArgument::ResultPlace,
        ] {
            signature.params.push(AbiParam::new(pointer_type));
        }
        signature.returns.push(AbiParam::new(types::I64));
        let id = module.declare_anonymous_function(&signature)?;
        // Text is position-independent. The publisher resolves linkage-table
        // relocations before this mapping, and Cranelift's platform provider
        // changes permissions and synchronizes the instruction cache.
        module.define_function_bytes(id, 1, image.image().bytes(), &[])?;
        module.finalize_definitions()?;
        let ptr = module.get_finalized_function(id);
        // SAFETY: all accepted images originate in a trusted emitter using
        // ScalarContext and ScalarEntry for the checked host target. This pointer
        // comes from the finalized executable allocation retained below.
        let entry = unsafe { std::mem::transmute::<*const u8, ScalarEntry>(ptr) };
        Ok(Self {
            _allocation: allocation,
            entry,
            address: ptr.addr(),
        })
    }

    pub(super) fn address(&self) -> usize {
        self.address
    }

    pub(super) fn invoke(&self, call: Invocation<'_>) -> u64 {
        let Invocation {
            context,
            slots,
            links,
            result,
        } = call;
        context.slots = slots.as_mut_ptr();
        context.links = links.as_ptr();
        // SAFETY: the invocation holds exclusive storage for the context,
        // frame slots and result, and retains the immutable validated linkage
        // table throughout the call. Only the native dispatcher constructs it
        // with the entry plan's validated dimensions and logical PC.
        unsafe { (self.entry)(context, std::ptr::null(), result) }
    }
}

pub(super) struct Invocation<'a> {
    pub context: &'a mut ScalarContext,
    pub slots: &'a mut [u64],
    pub links: &'a [u64],
    pub result: &'a mut u64,
}
