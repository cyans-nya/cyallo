//! Per-instance registration of implementations for an SDK's bodyless items.
//!
//! An SDK declares signatures; whoever implements it supplies the rest. A
//! [`Registry`] collects those implementations and [`Registry::install`] validates
//! them against the SDK's [`InterfaceDigests`]:
//!
//! * every registration names an item that exists, is bindable and has the right kind;
//! * the digest the implementer was built against equals the SDK's current digest;
//! * every bindable item is bound exactly once;
//! * every contract is internally consistent (see `docs/intrinsic-contracts.md`).
//!
//! A registry is a plain value owned by one runtime or session. There is no global
//! table: two registries may bind the same item differently without interference.
//! The result, [`Installed`], is immutable; the compiler and backends query contracts
//! from it and trust only what installation validated.
use crate::contract::{Disposition, MemberContract, Safepoint};
use crate::module_graph::ResolvedProgram;
use crate::sdk::{Digest, InterfaceDigests, InterfaceItem, ItemKind};
use cyallo_ast::DeclarationId;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
mod reference;
pub use reference::{LinkedScalarHost, LinkedScalarReference, ReferenceLinkError, ScalarLinks};
/// The contract of a bound routine: disposition, safepoint, effects and operand passing.
pub type BindingContract = MemberContract;
/// Engines a lowering can be supplied for. The interpreter always runs the reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Engine {
    Winch,
    Cranelift,
    CraneliftSpeed,
}
/// What defines an engine intrinsic's meaning. Native lowerings must agree with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reference {
    /// A Cyallo body, named by its item path in the implementing component.
    Source(Box<str>),
    /// A declarative IR body registered under this name.
    Declarative(Box<str>),
}
/// What the compiler must know about an opaque host type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OpaqueContract {
    /// Duplicating the value is a bitwise copy with no ownership effect.
    pub copyable: bool,
    /// The value can move between tasks and sessions.
    pub transferable: bool,
    /// Dropping the value can run guest code.
    pub drop_runs_guest: bool,
    /// The value holds references to managed values the collector must trace.
    pub holds_managed_references: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Implementation {
    /// A call across the host boundary: opaque to the optimizer.
    HostCall(BindingContract),
    /// An engine-level operation with reference semantics and optional lowerings.
    Intrinsic {
        contract: BindingContract,
        reference: Option<Reference>,
        lowerings: Vec<Engine>,
    },
    /// The representation of an opaque record.
    Opaque(OpaqueContract),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub digest: Digest,
    pub implementation: Implementation,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractViolation {
    /// Parking the coroutine requires a re-entry safepoint.
    ParkingNeedsReentry,
    /// Calling guest code requires a re-entry safepoint.
    GuestCallNeedsReentry,
    /// Allocation can collect, so the safepoint cannot be `Never`.
    AllocationNeedsSafepoint,
    /// A host call must declare the host-call disposition.
    HostCallDisposition,
    /// An intrinsic is an engine primitive; source candidates are written in source
    /// and host calls use the host-call registration.
    IntrinsicDisposition,
    /// An intrinsic needs reference semantics: the checked fallback.
    MissingReference,
    /// Operand `index` is passed by move or by address but the routine has `arity`.
    OperandOutOfRange { index: usize, arity: usize },
}
impl fmt::Display for ContractViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ParkingNeedsReentry => {
                f.write_str("an operation that parks must have a re-entry safepoint")
            }
            Self::GuestCallNeedsReentry => {
                f.write_str("an operation that calls guest code must have a re-entry safepoint")
            }
            Self::AllocationNeedsSafepoint => f.write_str(
                "an operation that allocates cannot declare that it never reaches a safepoint",
            ),
            Self::HostCallDisposition => {
                f.write_str("a host call must declare the host-call disposition")
            }
            Self::IntrinsicDisposition => {
                f.write_str("an intrinsic must declare the engine-primitive disposition")
            }
            Self::MissingReference => f.write_str("an intrinsic must supply reference semantics"),
            Self::OperandOutOfRange { index, arity } => {
                write!(
                    f,
                    "operand {index} is out of range for a routine with {arity} parameters"
                )
            }
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationError {
    UnknownItem {
        path: Box<str>,
    },
    KindMismatch {
        path: Box<str>,
        found: ItemKind,
        wanted: &'static str,
    },
    NotBindable {
        path: Box<str>,
    },
    DigestMismatch {
        path: Box<str>,
        registered: Digest,
        sdk: Digest,
    },
    Duplicate {
        path: Box<str>,
    },
    Unbound {
        path: Box<str>,
    },
    Contract {
        path: Box<str>,
        violation: ContractViolation,
    },
    MissingDeclaration {
        path: Box<str>,
    },
    AmbiguousDeclaration {
        path: Box<str>,
    },
}
impl fmt::Display for RegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownItem { path } => write!(f, "the SDK has no item `{path}`"),
            Self::KindMismatch {
                path,
                found,
                wanted,
            } => {
                write!(
                    f,
                    "`{path}` is a {found:?}, but this registration binds a {wanted}"
                )
            }
            Self::NotBindable { path } => {
                write!(
                    f,
                    "`{path}` has an implementation in the SDK and cannot be bound"
                )
            }
            Self::DigestMismatch {
                path,
                registered,
                sdk,
            } => write!(
                f,
                "`{path}` was implemented against {registered}, but the SDK declares {sdk}"
            ),
            Self::Duplicate { path } => write!(f, "`{path}` is bound more than once"),
            Self::Unbound { path } => write!(f, "`{path}` has no implementation"),
            Self::Contract { path, violation } => write!(f, "contract of `{path}`: {violation}"),
            Self::MissingDeclaration { path } => write!(f, "`{path}` has no resolved declaration"),
            Self::AmbiguousDeclaration { path } => {
                write!(
                    f,
                    "`{path}` does not identify a single resolved declaration"
                )
            }
        }
    }
}
impl std::error::Error for RegistrationError {}
/// Implementations collected for one runtime. Nothing is checked until
/// [`install`](Self::install).
#[derive(Debug, Clone, Default)]
pub struct Registry {
    entries: Vec<(Box<str>, Binding)>,
}
impl Registry {
    pub fn new() -> Self {
        Self::default()
    }
    fn add(mut self, path: &str, digest: Digest, implementation: Implementation) -> Self {
        self.entries.push((
            path.into(),
            Binding {
                digest,
                implementation,
            },
        ));
        self
    }
    /// Bind a routine to an opaque host call.
    pub fn host_call(self, path: &str, digest: Digest, contract: BindingContract) -> Self {
        self.add(path, digest, Implementation::HostCall(contract))
    }
    /// Bind a routine to an engine intrinsic.
    pub fn intrinsic(
        self,
        path: &str,
        digest: Digest,
        contract: BindingContract,
        reference: Option<Reference>,
        lowerings: Vec<Engine>,
    ) -> Self {
        self.add(
            path,
            digest,
            Implementation::Intrinsic {
                contract,
                reference,
                lowerings,
            },
        )
    }
    /// Bind the representation of an opaque record.
    pub fn opaque_type(self, path: &str, digest: Digest, contract: OpaqueContract) -> Self {
        self.add(path, digest, Implementation::Opaque(contract))
    }
    /// Validate every registration against the SDK and require every bindable item
    /// to be bound. All problems are reported, in registration then SDK order.
    pub fn install(self, sdk: &InterfaceDigests) -> Result<Installed, Vec<RegistrationError>> {
        let mut errors = vec![];
        let mut bindings: BTreeMap<Box<str>, Binding> = BTreeMap::new();
        for (path, binding) in self.entries {
            let error = |e: RegistrationError| e;
            let Some(item) = sdk.item(&path) else {
                errors.push(error(RegistrationError::UnknownItem { path }));
                continue;
            };
            if !item.requires_binding {
                errors.push(RegistrationError::NotBindable { path });
                continue;
            }
            if let Some(wanted) = kind_mismatch(item, &binding.implementation) {
                errors.push(RegistrationError::KindMismatch {
                    path,
                    found: item.kind,
                    wanted,
                });
                continue;
            }
            if binding.digest != item.digest {
                errors.push(RegistrationError::DigestMismatch {
                    path,
                    registered: binding.digest,
                    sdk: item.digest,
                });
                continue;
            }
            if let Some(violation) = violation(item, &binding.implementation) {
                errors.push(RegistrationError::Contract { path, violation });
                continue;
            }
            if bindings.contains_key(&path) {
                errors.push(RegistrationError::Duplicate { path });
                continue;
            }
            bindings.insert(path, binding);
        }
        for item in sdk.items.iter().filter(|i| i.requires_binding) {
            if !bindings.contains_key(&item.path)
                && !errors.iter().any(|e| bound_path(e) == Some(&item.path))
            {
                errors.push(RegistrationError::Unbound {
                    path: item.path.clone(),
                });
            }
        }
        if errors.is_empty() {
            Ok(Installed { bindings })
        } else {
            Err(errors)
        }
    }

    /// Validate and link provider contracts for this resolved program. From this
    /// point on, consumers use declaration IDs, including for imported aliases.
    pub fn install_for_program(
        self,
        program: &ResolvedProgram,
    ) -> Result<ResolvedBindings, Vec<RegistrationError>> {
        let installed = self.install(&program.interface)?;
        let mut declarations = HashMap::new();
        for (path, id) in &program.interface_declarations {
            declarations
                .entry(path.as_ref())
                .and_modify(|entry| *entry = None)
                .or_insert(Some(*id));
        }

        let mut bindings = HashMap::new();
        let mut errors = vec![];
        for (path, binding) in installed.bindings {
            match declarations.get(path.as_ref()) {
                Some(Some(id)) if !bindings.contains_key(id) => {
                    bindings.insert(*id, binding);
                }
                Some(_) => errors.push(RegistrationError::AmbiguousDeclaration { path }),
                None => errors.push(RegistrationError::MissingDeclaration { path }),
            }
        }
        if errors.is_empty() {
            Ok(ResolvedBindings { bindings })
        } else {
            Err(errors)
        }
    }
}

/// Immutable provider contracts keyed by declarations in the resolved program
/// they were installed against. Numeric IDs are scoped to that program.
#[derive(Debug, Clone)]
pub struct ResolvedBindings {
    bindings: HashMap<DeclarationId, Binding>,
}

impl ResolvedBindings {
    pub fn get(&self, declaration: DeclarationId) -> Option<&Binding> {
        self.bindings.get(&declaration)
    }
}
/// The path an error is about, if it was raised while binding that path.
fn bound_path(error: &RegistrationError) -> Option<&str> {
    match error {
        RegistrationError::KindMismatch { path, .. }
        | RegistrationError::DigestMismatch { path, .. }
        | RegistrationError::Duplicate { path }
        | RegistrationError::Contract { path, .. } => Some(path),
        _ => None,
    }
}
fn kind_mismatch(item: &InterfaceItem, implementation: &Implementation) -> Option<&'static str> {
    let routine = matches!(item.kind, ItemKind::Function | ItemKind::Method);
    match implementation {
        Implementation::HostCall(_) | Implementation::Intrinsic { .. } if !routine => {
            Some("function or method")
        }
        Implementation::Opaque(_) if item.kind != ItemKind::OpaqueRecord => Some("opaque record"),
        _ => None,
    }
}
fn violation(item: &InterfaceItem, implementation: &Implementation) -> Option<ContractViolation> {
    let (contract, reference) = match implementation {
        Implementation::HostCall(c) => {
            if c.disposition != Disposition::HostCall {
                return Some(ContractViolation::HostCallDisposition);
            }
            (c, true)
        }
        Implementation::Intrinsic {
            contract,
            reference,
            ..
        } => {
            if contract.disposition != Disposition::EnginePrimitive {
                return Some(ContractViolation::IntrinsicDisposition);
            }
            (contract, reference.is_some())
        }
        Implementation::Opaque(_) => return None,
    };
    if !reference {
        return Some(ContractViolation::MissingReference);
    }
    let e = contract.effects;
    if e.may_park && contract.safepoint != Safepoint::MayReenter {
        return Some(ContractViolation::ParkingNeedsReentry);
    }
    if e.calls_guest && contract.safepoint != Safepoint::MayReenter {
        return Some(ContractViolation::GuestCallNeedsReentry);
    }
    if e.allocates && contract.safepoint == Safepoint::Never {
        return Some(ContractViolation::AllocationNeedsSafepoint);
    }
    if let Some(arity) = item.arity {
        let mut positions = contract.operands.exceptions.iter().map(|(i, _)| *i);
        if let Some(index) = positions.find(|i| *i >= arity) {
            return Some(ContractViolation::OperandOutOfRange { index, arity });
        }
    }
    None
}
/// Validated bindings. Immutable; query contracts by SDK item path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    bindings: BTreeMap<Box<str>, Binding>,
}
impl Installed {
    pub fn get(&self, path: &str) -> Option<&Binding> {
        self.bindings.get(path)
    }
    /// The contract of a bound routine, whether a host call or an intrinsic.
    pub fn contract(&self, path: &str) -> Option<&BindingContract> {
        match &self.get(path)?.implementation {
            Implementation::HostCall(c) | Implementation::Intrinsic { contract: c, .. } => Some(c),
            Implementation::Opaque(_) => None,
        }
    }
    pub fn opaque(&self, path: &str) -> Option<&OpaqueContract> {
        match &self.get(path)?.implementation {
            Implementation::Opaque(c) => Some(c),
            _ => None,
        }
    }
    pub fn len(&self) -> usize {
        self.bindings.len()
    }
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Binding)> {
        self.bindings
            .iter()
            .map(|(path, binding)| (path.as_ref(), binding))
    }
}
