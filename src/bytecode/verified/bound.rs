//! Synchronous scalar callbacks supplied by the embedder for one SDK bundle.

use super::{Scalar, ScalarType};
use cyallo_ast::DeclarationId;
use cyallo_ir::{
    contract::Effects,
    registry::{BindingContract, Registry},
    sdk::Digest,
};
use std::{collections::BTreeMap, fmt, sync::Arc};

/// A synchronous host operation declined this invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostFailure {
    Rejected,
}

impl fmt::Display for HostFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected => f.write_str("host rejected the invocation"),
        }
    }
}

impl std::error::Error for HostFailure {}

type Callback = Arc<dyn Fn(&[Scalar]) -> Result<Scalar, HostFailure> + Send + Sync>;

/// The bindings and synchronous host callbacks supplied by one provider.
/// Installation validates the declarations before any callback can run.
#[derive(Default)]
pub struct ScalarSdk {
    pub(super) registry: Registry,
    pub(super) callbacks: BTreeMap<Box<str>, Callback>,
}

impl ScalarSdk {
    pub fn new(registry: Registry) -> Self {
        Self {
            registry,
            callbacks: BTreeMap::new(),
        }
    }

    pub fn host_call(
        mut self,
        path: &str,
        digest: Digest,
        contract: BindingContract,
        callback: impl Fn(&[Scalar]) -> Result<Scalar, HostFailure> + Send + Sync + 'static,
    ) -> Self {
        self.registry = self.registry.host_call(path, digest, contract);
        self.callbacks.insert(path.into(), Arc::new(callback));
        self
    }
}

pub(super) struct ScalarHostBinding {
    pub declaration: DeclarationId,
    pub parameters: Box<[ScalarType]>,
    pub result: ScalarType,
    pub effects: Effects,
    pub callback: Callback,
}

impl fmt::Debug for ScalarHostBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScalarHostBinding")
            .field("declaration", &self.declaration)
            .field("parameters", &self.parameters)
            .field("result", &self.result)
            .field("effects", &self.effects)
            .finish_non_exhaustive()
    }
}
