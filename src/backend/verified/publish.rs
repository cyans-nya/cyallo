//! Instance-owned publication and transitive executable entry pins.

use super::{
    CodeVersion, CraneliftProfile, FunctionVersion, ImageError, NativeTarget, NativeTier, Origin,
    PrepareError, PreparedFunction, UnlinkedImage, ValidatedImage, platform::Executable,
};
use crate::bytecode::verified::{FunctionId, InstructionId, VerifiedModule};
use std::{
    cell::RefCell,
    collections::HashMap,
    fmt,
    rc::{Rc, Weak},
    sync::Arc,
};

type RuntimeEntries = Rc<RefCell<HashMap<FunctionId, Weak<Published>>>>;

pub struct CodeStore {
    target: NativeTarget,
    module: VerifiedModule,
    version: CodeVersion,
    shared_runtime: bool,
    plans: HashMap<FunctionId, Arc<PreparedFunction>>,
    entries: HashMap<FunctionId, EntryPin>,
    runtime_entries: RuntimeEntries,
}

impl CodeStore {
    pub fn new(module: VerifiedModule) -> Result<Self, PublishError> {
        let shared_runtime = PreparedFunction::requires_shared_runtime(&module);
        Self::new_with_runtime(module, shared_runtime)
    }

    pub(crate) fn for_osr(module: VerifiedModule) -> Result<Self, PublishError> {
        Self::new_with_runtime(module, true)
    }

    fn new_with_runtime(
        module: VerifiedModule,
        shared_runtime: bool,
    ) -> Result<Self, PublishError> {
        let target = NativeTarget::host().ok_or(PublishError::UnsupportedHost)?;
        Ok(Self {
            target,
            shared_runtime,
            module,
            version: CodeVersion(0),
            plans: HashMap::new(),
            entries: HashMap::new(),
            runtime_entries: RuntimeEntries::default(),
        })
    }

    /// Switch the store to another snapshot. Existing entry pins retain the
    /// immutable old snapshot and its linkage for suspended invocations.
    pub fn replace(&mut self, module: VerifiedModule) -> Result<(), PublishError> {
        let version = self
            .version
            .0
            .checked_add(1)
            .ok_or(PublishError::VersionExhausted)?;
        self.version = CodeVersion(version);
        self.shared_runtime = PreparedFunction::requires_shared_runtime(&module);
        self.module = module;
        self.entries.clear();
        self.runtime_entries = RuntimeEntries::default();
        self.plans.clear();
        Ok(())
    }

    pub fn prepare(&mut self, function: FunctionId) -> Result<Arc<PreparedFunction>, PrepareError> {
        if let Some(plan) = self.plans.get(&function) {
            return Ok(plan.clone());
        }
        let plan = Arc::new(PreparedFunction::prepare_in_mode(
            &self.module,
            function,
            self.version,
            self.target,
            self.shared_runtime,
        )?);
        self.plans.insert(function, plan.clone());
        Ok(plan)
    }

    /// Eagerly compile routines that can be published on this host. Word
    /// plans need their linked callees first. Shared-runtime plans can call
    /// unpublished routines through the interpreter.
    pub(crate) fn compile_available(&mut self) {
        let mut pending = (0..)
            .map(FunctionId)
            .take_while(|&id| self.module.function(id).is_some())
            .collect::<Vec<_>>()
            .into_iter()
            .filter_map(|id| self.prepare(id).ok())
            .collect::<Vec<_>>();
        pending.reverse();
        loop {
            let before = pending.len();
            pending.retain(|plan| {
                if plan
                    .relocations()
                    .any(|link| !self.entries.contains_key(&link.target.function))
                {
                    return true;
                }
                let image = NativeTier::Winch.emit(plan.clone()).or_else(|_| {
                    NativeTier::Cranelift(CraneliftProfile::LowLatency).emit(plan.clone())
                });
                if let Ok(image) = image {
                    let _ = self.publish(image);
                }
                false
            });
            if pending.is_empty() || pending.len() == before {
                break;
            }
        }
    }

    /// Resolve the exact plan and active dependency pins before mapping. A
    /// failed publication leaves the previously selected entry available.
    pub fn publish(&mut self, image: UnlinkedImage) -> Result<EntryPin, PublishError> {
        let function = image.plan().function();
        if image.plan().version() != self.version {
            return Err(PublishError::StaleVersion {
                expected: self.version,
                actual: image.plan().version(),
            });
        }
        if image.plan().target() != self.target {
            return Err(PublishError::WrongTarget(image.plan().target()));
        }
        let expected = self
            .plans
            .get(&function)
            .ok_or(PublishError::UnknownPlan(function))?;
        let image = ValidatedImage::validate(expected, image).map_err(PublishError::Image)?;
        let linkage = self.link(&image)?;
        let executable = Executable::map(&image).map_err(PublishError::Mapping)?;
        let pin = EntryPin(Rc::new(Published {
            image,
            executable,
            linkage,
            module: self.module.clone(),
            runtime_entries: self.runtime_entries.clone(),
        }));
        if pin.plan().uses_shared_runtime() {
            self.runtime_entries
                .borrow_mut()
                .insert(function, Rc::downgrade(&pin.0));
        }
        self.entries.insert(function, pin.clone());
        Ok(pin)
    }

    fn link(&self, image: &ValidatedImage) -> Result<Linkage, PublishError> {
        let raw = image.image();
        let mut words = Vec::with_capacity(raw.relocations().len());
        let mut dependencies = Vec::with_capacity(raw.relocations().len());
        for relocation in raw.relocations() {
            let target = relocation.target;
            let dependency = self
                .entries
                .get(&target.function)
                .filter(|entry| entry.version() == target)
                .ok_or(PublishError::MissingDependency(target))?;
            let call = raw
                .plan()
                .call(relocation.instruction)
                .ok_or(PublishError::DependencyAbi(target))?;
            let callee = dependency.plan();
            if callee.target() != self.target
                || callee.abi() != raw.plan().abi()
                || call.result != callee.result()
                || call.result_layout.as_ref() != callee.result_layout()
                || !call
                    .arguments
                    .iter()
                    .map(|argument| argument.callee)
                    .eq(callee.arguments().iter().copied())
            {
                return Err(PublishError::DependencyAbi(target));
            }
            let address = dependency.0.executable.address();
            words.push(u64::try_from(address).map_err(|_| PublishError::AddressWidth)?);
            dependencies.push(dependency.clone());
        }
        Ok(Linkage {
            words: words.into_boxed_slice(),
            dependencies: dependencies.into_boxed_slice(),
        })
    }

    pub fn entry(&self, function: FunctionId) -> Option<EntryPin> {
        self.entries.get(&function).cloned()
    }

    pub fn retire(&mut self, function: FunctionId) {
        self.entries.remove(&function);
        self.runtime_entries.borrow_mut().remove(&function);
    }

    pub fn module(&self) -> &VerifiedModule {
        &self.module
    }
}

struct Published {
    image: ValidatedImage,
    executable: Executable,
    linkage: Linkage,
    module: VerifiedModule,
    runtime_entries: RuntimeEntries,
}

struct Linkage {
    words: Box<[u64]>,
    dependencies: Box<[EntryPin]>,
}

/// A retained callable entry with its exact prepared plan and transitive links.
#[derive(Clone)]
pub struct EntryPin(Rc<Published>);

impl fmt::Debug for EntryPin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EntryPin")
            .field("version", &self.version())
            .field("tier", &self.tier())
            .finish_non_exhaustive()
    }
}

impl EntryPin {
    /// Capture a version's available entries once for a new activation.
    /// Weak references in the registry avoid cycles between executable pins.
    pub(super) fn runtime_snapshot(&self) -> HashMap<FunctionId, EntryPin> {
        let mut entries = self
            .0
            .runtime_entries
            .borrow()
            .iter()
            .filter_map(|(&function, pin)| pin.upgrade().map(|pin| (function, EntryPin(pin))))
            .collect::<HashMap<_, _>>();
        entries.insert(self.plan().function(), self.clone());
        entries
    }

    pub fn tier(&self) -> NativeTier {
        self.0.image.image().tier()
    }

    pub fn version(&self) -> FunctionVersion {
        FunctionVersion {
            function: self.plan().function(),
            version: self.plan().version(),
        }
    }

    pub fn plan(&self) -> &PreparedFunction {
        self.0.image.image().plan()
    }

    pub fn machine_address(&self, instruction: InstructionId) -> Option<usize> {
        let range = self
            .0
            .image
            .image()
            .ranges()
            .iter()
            .find(|range| range.instruction == instruction)?;
        self.0
            .executable
            .address()
            .checked_add(usize::try_from(range.start).ok()?)
    }

    pub fn origin_at(&self, address: usize) -> Option<Origin> {
        let offset = address.checked_sub(self.0.executable.address())?;
        self.0.image.origin_at(u32::try_from(offset).ok()?)
    }

    pub(super) fn module(&self) -> &VerifiedModule {
        &self.0.module
    }

    pub(super) fn executable(&self) -> &Executable {
        &self.0.executable
    }

    pub(super) fn links(&self) -> &[u64] {
        &self.0.linkage.words
    }

    pub(super) fn dependency_at(&self, instruction: InstructionId, address: u64) -> Option<&Self> {
        let index = self
            .0
            .image
            .image()
            .relocations()
            .binary_search_by_key(&instruction, |relocation| relocation.instruction)
            .ok()?;
        let linkage = &self.0.linkage;
        if *linkage.words.get(index)? == address {
            linkage.dependencies.get(index)
        } else {
            None
        }
    }
}

#[derive(Debug)]
pub enum PublishError {
    UnsupportedHost,
    WrongTarget(NativeTarget),
    VersionExhausted,
    StaleVersion {
        expected: CodeVersion,
        actual: CodeVersion,
    },
    UnknownPlan(FunctionId),
    Image(ImageError),
    MissingDependency(FunctionVersion),
    DependencyAbi(FunctionVersion),
    AddressWidth,
    Mapping(Box<cranelift_module::ModuleError>),
}

impl fmt::Display for PublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedHost => f.write_str("native publication is unavailable on this host"),
            Self::WrongTarget(target) => {
                write!(f, "cannot publish image for {target:?} on this host")
            }
            Self::VersionExhausted => f.write_str("native code store exhausted its version range"),
            Self::StaleVersion { expected, actual } => {
                write!(f, "image version {actual:?} is stale against {expected:?}")
            }
            Self::UnknownPlan(function) => write!(f, "code store has no plan for {function:?}"),
            Self::Image(error) => error.fmt(f),
            Self::MissingDependency(target) => {
                write!(f, "image dependency {target:?} is unavailable")
            }
            Self::DependencyAbi(target) => {
                write!(f, "image dependency {target:?} has an incompatible ABI")
            }
            Self::AddressWidth => f.write_str("native address exceeds linkage width"),
            Self::Mapping(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for PublishError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Image(error) => Some(error),
            Self::Mapping(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}
