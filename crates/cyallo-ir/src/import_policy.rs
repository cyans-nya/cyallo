//! Host-configured import authority resolved against actual module declarations.
//!
//! Paths are configuration lookup keys, never trust credentials. After resolve,
//! all permission checks use compilation-owned declaration IDs and parent edges.

use crate::declarations::SourceTrust;
use cyallo_ast::{DeclarationId, Diagnostic, Span};
use std::{
    collections::{HashMap, HashSet},
    fmt,
};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ModuleAccess {
    #[default]
    All,
    Only(Vec<Box<str>>),
    Except(Vec<Box<str>>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeMatch {
    Exact,
    Subtree,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportScope {
    pub referrer: Box<str>,
    pub matching: ScopeMatch,
    pub access: ModuleAccess,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportPolicy {
    pub ceiling: ModuleAccess,
    pub scopes: Vec<ImportScope>,
}

/// The loader/linker supplies this graph. Setting trust is a host decision and
/// must never be derived from a caller-controlled filename or path prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleDescriptor {
    pub id: DeclarationId,
    pub path: Box<str>,
    pub parent: Option<DeclarationId>,
    pub trust: SourceTrust,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleIdentity {
    pub id: DeclarationId,
    pub path: Box<str>,
}
impl From<&ModuleDescriptor> for ModuleIdentity {
    fn from(module: &ModuleDescriptor) -> Self {
        Self {
            id: module.id,
            path: module.path.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyLocation {
    Ceiling,
    ScopeReferrer { ordinal: usize },
    ScopeAccess { ordinal: usize },
}
impl fmt::Display for PolicyLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ceiling => f.write_str("global import ceiling"),
            Self::ScopeReferrer { ordinal } => {
                write!(f, "referrer of import scope {}", ordinal + 1)
            }
            Self::ScopeAccess { ordinal } => {
                write!(f, "access list of import scope {}", ordinal + 1)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleRole {
    Referrer,
    Target,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportPolicyError {
    UnknownConfiguredModule {
        path: Box<str>,
        location: PolicyLocation,
    },
    DuplicateModuleId {
        id: DeclarationId,
        first: Box<str>,
        second: Box<str>,
    },
    DuplicateModulePath {
        path: Box<str>,
        first: DeclarationId,
        second: DeclarationId,
    },
    UnknownParent {
        module: ModuleIdentity,
        parent: DeclarationId,
    },
    CyclicParents {
        modules: Vec<ModuleIdentity>,
    },
    UnknownModule {
        id: DeclarationId,
        role: ModuleRole,
    },
    Denied {
        referrer: ModuleIdentity,
        target: ModuleIdentity,
    },
}
impl fmt::Display for ImportPolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownConfiguredModule { path, location } => {
                write!(f, "{location} names undeclared module `{path}`")
            }
            Self::DuplicateModuleId { id, first, second } => write!(
                f,
                "module declaration ID {} belongs to both `{first}` and `{second}`",
                id.0
            ),
            Self::DuplicateModulePath {
                path,
                first,
                second,
            } => write!(
                f,
                "module path `{path}` belongs to declaration IDs {} and {}",
                first.0, second.0
            ),
            Self::UnknownParent { module, parent } => write!(
                f,
                "module `{}` has unknown parent declaration {}",
                module.path, parent.0
            ),
            Self::CyclicParents { modules } => {
                f.write_str("cyclic module parent graph: ")?;
                for (index, module) in modules.iter().enumerate() {
                    if index != 0 {
                        f.write_str(" → ")?;
                    }
                    write!(f, "`{}`", module.path)?;
                }
                Ok(())
            }
            Self::UnknownModule { id, role } => write!(
                f,
                "unknown {role:?} module declaration {} in this import policy",
                id.0
            ),
            Self::Denied { referrer, target } => write!(
                f,
                "module `{}` may not import native module `{}`",
                referrer.path, target.path
            ),
        }
    }
}
impl std::error::Error for ImportPolicyError {}

/// Presents typed import configuration/permission failures at the diagnostic edge.
pub fn import_policy_diagnostic(error: ImportPolicyError, span: Span) -> Diagnostic {
    Diagnostic::new("E174", error.to_string(), span)
}

#[derive(Debug, Clone)]
pub struct ResolvedPolicy {
    modules: HashMap<DeclarationId, ModuleDescriptor>,
    allowed: HashMap<DeclarationId, HashSet<DeclarationId>>,
}

struct ResolvedScope {
    referrer: DeclarationId,
    matching: ScopeMatch,
    allowed: HashSet<DeclarationId>,
}

impl ImportPolicy {
    /// Resolves every host-configured path once and returns an independent graph
    /// snapshot. Borrows inputs through return; retains only owned data and IDs.
    pub fn resolve(&self, graph: &[ModuleDescriptor]) -> Result<ResolvedPolicy, ImportPolicyError> {
        let mut modules: HashMap<DeclarationId, ModuleDescriptor> = HashMap::new();
        let mut paths: HashMap<&str, DeclarationId> = HashMap::new();
        for module in graph {
            if let Some(previous) = modules.insert(module.id, module.clone()) {
                return Err(ImportPolicyError::DuplicateModuleId {
                    id: module.id,
                    first: previous.path,
                    second: module.path.clone(),
                });
            }
            if let Some(previous) = paths.insert(&module.path, module.id) {
                return Err(ImportPolicyError::DuplicateModulePath {
                    path: module.path.clone(),
                    first: previous,
                    second: module.id,
                });
            }
        }
        validate_parents(graph, &modules)?;
        let native: HashSet<_> = graph
            .iter()
            .filter(|module| module.trust == SourceTrust::Core)
            .map(|module| module.id)
            .collect();
        let ceiling = resolve_access(&self.ceiling, &paths, &native, PolicyLocation::Ceiling)?;
        let scopes = self
            .scopes
            .iter()
            .enumerate()
            .map(|(ordinal, scope)| {
                let referrer = configured_id(
                    &paths,
                    &scope.referrer,
                    PolicyLocation::ScopeReferrer { ordinal },
                )?;
                let allowed = resolve_access(
                    &scope.access,
                    &paths,
                    &native,
                    PolicyLocation::ScopeAccess { ordinal },
                )?;
                Ok(ResolvedScope {
                    referrer,
                    matching: scope.matching,
                    allowed,
                })
            })
            .collect::<Result<Vec<_>, ImportPolicyError>>()?;
        let mut allowed = HashMap::new();
        for module in graph
            .iter()
            .filter(|module| module.trust == SourceTrust::User)
        {
            let ancestors = ancestors(module.id, &modules);
            let mut access = ceiling.clone();
            for scope in &scopes {
                let matches = match scope.matching {
                    ScopeMatch::Exact => scope.referrer == module.id,
                    ScopeMatch::Subtree => ancestors.contains(&scope.referrer),
                };
                if matches {
                    access.retain(|target| scope.allowed.contains(target));
                }
            }
            allowed.insert(module.id, access);
        }
        Ok(ResolvedPolicy { modules, allowed })
    }
}

impl ResolvedPolicy {
    /// Checks an import of an actual defining module. Aliases and reexports must
    /// pass that same target ID. Visibility and file-read grants are separate.
    pub fn check(
        &self,
        from: DeclarationId,
        target: DeclarationId,
    ) -> Result<(), ImportPolicyError> {
        let referrer = self
            .modules
            .get(&from)
            .ok_or(ImportPolicyError::UnknownModule {
                id: from,
                role: ModuleRole::Referrer,
            })?;
        let target = self
            .modules
            .get(&target)
            .ok_or(ImportPolicyError::UnknownModule {
                id: target,
                role: ModuleRole::Target,
            })?;
        // Core implementation dependencies do not inherit guest restrictions.
        // User labels that merely resemble a native path do not bypass checks.
        if referrer.trust != SourceTrust::User || target.trust != SourceTrust::Core {
            return Ok(());
        }
        if self
            .allowed
            .get(&from)
            .is_some_and(|allowed| allowed.contains(&target.id))
        {
            return Ok(());
        }
        Err(ImportPolicyError::Denied {
            referrer: referrer.into(),
            target: target.into(),
        })
    }
}

fn configured_id(
    paths: &HashMap<&str, DeclarationId>,
    path: &str,
    location: PolicyLocation,
) -> Result<DeclarationId, ImportPolicyError> {
    paths
        .get(path)
        .copied()
        .ok_or_else(|| ImportPolicyError::UnknownConfiguredModule {
            path: path.into(),
            location,
        })
}

fn resolve_access(
    access: &ModuleAccess,
    paths: &HashMap<&str, DeclarationId>,
    native: &HashSet<DeclarationId>,
    location: PolicyLocation,
) -> Result<HashSet<DeclarationId>, ImportPolicyError> {
    match access {
        ModuleAccess::All => Ok(native.clone()),
        ModuleAccess::Only(names) => {
            let selected = names
                .iter()
                .map(|name| configured_id(paths, name, location))
                .collect::<Result<HashSet<_>, _>>()?;
            Ok(selected.intersection(native).copied().collect())
        }
        ModuleAccess::Except(names) => {
            let excluded = names
                .iter()
                .map(|name| configured_id(paths, name, location))
                .collect::<Result<HashSet<_>, _>>()?;
            Ok(native.difference(&excluded).copied().collect())
        }
    }
}

fn ancestors(
    module: DeclarationId,
    graph: &HashMap<DeclarationId, ModuleDescriptor>,
) -> HashSet<DeclarationId> {
    let mut result = HashSet::new();
    let mut current = Some(module);
    while let Some(id) = current {
        // Called only after parent existence and cycles have been validated.
        result.insert(id);
        current = graph[&id].parent;
    }
    result
}

fn validate_parents(
    graph: &[ModuleDescriptor],
    indexed: &HashMap<DeclarationId, ModuleDescriptor>,
) -> Result<(), ImportPolicyError> {
    for module in graph {
        if let Some(parent) = module.parent
            && !indexed.contains_key(&parent)
        {
            return Err(ImportPolicyError::UnknownParent {
                module: module.into(),
                parent,
            });
        }
    }
    let mut complete = HashSet::new();
    for module in graph {
        let mut positions = HashMap::new();
        let mut chain = vec![];
        let mut current = Some(module.id);
        while let Some(id) = current {
            if complete.contains(&id) {
                break;
            }
            if let Some(start) = positions.insert(id, chain.len()) {
                let mut cycle: Vec<ModuleIdentity> = chain[start..]
                    .iter()
                    .map(|id| (&indexed[id]).into())
                    .collect();
                cycle.push((&indexed[&id]).into());
                return Err(ImportPolicyError::CyclicParents { modules: cycle });
            }
            chain.push(id);
            current = indexed[&id].parent;
        }
        complete.extend(chain);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: u32) -> DeclarationId {
        DeclarationId(value)
    }
    fn module(value: u32, path: &str, parent: Option<u32>, trust: SourceTrust) -> ModuleDescriptor {
        ModuleDescriptor {
            id: id(value),
            path: path.into(),
            parent: parent.map(id),
            trust,
        }
    }
    fn only(names: &[&str]) -> ModuleAccess {
        ModuleAccess::Only(names.iter().map(|name| (*name).into()).collect())
    }
    fn except(names: &[&str]) -> ModuleAccess {
        ModuleAccess::Except(names.iter().map(|name| (*name).into()).collect())
    }
    fn scope(referrer: &str, matching: ScopeMatch, access: ModuleAccess) -> ImportScope {
        ImportScope {
            referrer: referrer.into(),
            matching,
            access,
        }
    }
    fn graph() -> Vec<ModuleDescriptor> {
        vec![
            module(0, "app", None, SourceTrust::User),
            module(1, "app::worker", Some(0), SourceTrust::User),
            module(2, "app::worker::child", Some(1), SourceTrust::User),
            module(3, "app::sibling", Some(0), SourceTrust::User),
            module(10, "native", None, SourceTrust::Core),
            module(11, "native::task", Some(10), SourceTrust::Core),
            module(12, "native::clock", Some(10), SourceTrust::Core),
            module(13, "native::task::detail", Some(11), SourceTrust::Core),
        ]
    }

    #[test]
    fn whitelist_targets_are_exact_declarations_and_aliases_keep_the_same_identity() {
        let resolved = ImportPolicy {
            ceiling: only(&["native::task"]),
            scopes: vec![],
        }
        .resolve(&graph())
        .unwrap();
        assert!(resolved.check(id(0), id(11)).is_ok());
        let imported_alias = id(11);
        assert!(resolved.check(id(0), imported_alias).is_ok());
        assert!(matches!(
            resolved.check(id(0), id(10)),
            Err(ImportPolicyError::Denied { .. })
        ));
        assert!(matches!(
            resolved.check(id(0), id(12)),
            Err(ImportPolicyError::Denied { .. })
        ));
        assert!(matches!(
            resolved.check(id(0), id(13)),
            Err(ImportPolicyError::Denied { .. })
        ));
    }

    #[test]
    fn exclusion_is_exact_and_ordinary_user_imports_are_unaffected() {
        let resolved = ImportPolicy {
            ceiling: except(&["native::task"]),
            scopes: vec![],
        }
        .resolve(&graph())
        .unwrap();
        assert!(resolved.check(id(0), id(11)).is_err());
        assert!(resolved.check(id(0), id(13)).is_ok());
        assert!(resolved.check(id(0), id(12)).is_ok());
        assert!(resolved.check(id(0), id(1)).is_ok());
    }

    #[test]
    fn matching_ancestor_rules_intersect_and_child_scopes_cannot_widen_them() {
        let policy = ImportPolicy {
            ceiling: only(&["native::task", "native::clock"]),
            scopes: vec![
                scope("app", ScopeMatch::Subtree, only(&["native::task"])),
                scope("app::worker", ScopeMatch::Subtree, ModuleAccess::All),
                scope(
                    "app::worker::child",
                    ScopeMatch::Exact,
                    only(&["native::clock"]),
                ),
            ],
        };
        let resolved = policy.resolve(&graph()).unwrap();
        assert!(resolved.check(id(1), id(11)).is_ok());
        assert!(resolved.check(id(1), id(12)).is_err());
        assert!(resolved.check(id(2), id(11)).is_err());
        assert!(resolved.check(id(2), id(12)).is_err());
        assert!(resolved.check(id(3), id(11)).is_ok());
        assert!(resolved.check(id(0), id(13)).is_err());
    }

    #[test]
    fn exact_referrer_scope_does_not_restrict_unmatched_descendants() {
        let resolved = ImportPolicy {
            ceiling: ModuleAccess::All,
            scopes: vec![scope("app::worker", ScopeMatch::Exact, only(&[]))],
        }
        .resolve(&graph())
        .unwrap();
        assert!(resolved.check(id(1), id(11)).is_err());
        assert!(resolved.check(id(2), id(11)).is_ok());
        assert!(resolved.check(id(3), id(11)).is_ok());
    }

    #[test]
    fn subtree_matching_uses_graph_ancestry_without_path_prefix_authority() {
        let mut modules = graph();
        modules.push(module(
            4,
            "app::worker::looks_nested",
            Some(0),
            SourceTrust::User,
        ));
        modules.push(module(
            5,
            "different_display_name",
            Some(1),
            SourceTrust::User,
        ));
        let resolved = ImportPolicy {
            ceiling: ModuleAccess::All,
            scopes: vec![scope("app::worker", ScopeMatch::Subtree, only(&[]))],
        }
        .resolve(&modules)
        .unwrap();
        assert!(resolved.check(id(4), id(11)).is_ok());
        assert!(resolved.check(id(5), id(11)).is_err());
    }

    #[test]
    fn native_internal_imports_do_not_inherit_guest_restrictions() {
        let resolved = ImportPolicy {
            ceiling: only(&[]),
            scopes: vec![scope("native", ScopeMatch::Subtree, only(&[]))],
        }
        .resolve(&graph())
        .unwrap();
        assert!(resolved.check(id(0), id(11)).is_err());
        assert!(resolved.check(id(11), id(12)).is_ok());
        assert!(resolved.check(id(11), id(0)).is_ok());
    }

    #[test]
    fn native_looking_user_labels_never_acquire_core_trust() {
        let mut modules = graph();
        modules.push(module(4, "std::task", Some(0), SourceTrust::User));
        let resolved = ImportPolicy {
            ceiling: only(&[]),
            scopes: vec![],
        }
        .resolve(&modules)
        .unwrap();
        assert!(
            matches!(resolved.check(id(4),id(11)),Err(ImportPolicyError::Denied{referrer,..}) if referrer.path.as_ref()=="std::task")
        );
        // User source remains user source; policy does not make it a native module.
        assert!(resolved.check(id(0), id(4)).is_ok());
    }

    #[test]
    fn independent_graph_instances_resolve_equal_numeric_ids_independently() {
        let first = graph();
        let mut second = graph();
        second
            .iter_mut()
            .find(|module| module.id == id(11))
            .unwrap()
            .path = "native::clock".into();
        second
            .iter_mut()
            .find(|module| module.id == id(12))
            .unwrap()
            .path = "native::task".into();
        let policy = ImportPolicy {
            ceiling: only(&["native::task"]),
            scopes: vec![],
        };
        let a = policy.resolve(&first).unwrap();
        let b = policy.resolve(&second).unwrap();
        assert!(a.check(id(0), id(11)).is_ok());
        assert!(a.check(id(0), id(12)).is_err());
        assert!(b.check(id(0), id(11)).is_err());
        assert!(b.check(id(0), id(12)).is_ok());
        assert!(a.check(id(0), id(11)).is_ok());
    }

    #[test]
    fn unknown_host_configuration_is_rejected_before_any_import_check() {
        let error = ImportPolicy {
            ceiling: only(&["absent"]),
            scopes: vec![],
        }
        .resolve(&graph())
        .unwrap_err();
        assert!(matches!(
            error,
            ImportPolicyError::UnknownConfiguredModule {
                location: PolicyLocation::Ceiling,
                ..
            }
        ));
        let error = ImportPolicy {
            ceiling: ModuleAccess::All,
            scopes: vec![scope("not_a_module", ScopeMatch::Exact, ModuleAccess::All)],
        }
        .resolve(&graph())
        .unwrap_err();
        assert!(matches!(
            error,
            ImportPolicyError::UnknownConfiguredModule {
                location: PolicyLocation::ScopeReferrer { ordinal: 0 },
                ..
            }
        ));
        let error = ImportPolicy {
            ceiling: ModuleAccess::All,
            scopes: vec![scope("app", ScopeMatch::Exact, except(&["not_a_module"]))],
        }
        .resolve(&graph())
        .unwrap_err();
        assert!(matches!(
            error,
            ImportPolicyError::UnknownConfiguredModule {
                location: PolicyLocation::ScopeAccess { ordinal: 0 },
                ..
            }
        ));
    }

    #[test]
    fn malformed_graphs_and_unknown_check_ids_keep_structured_errors() {
        let mut modules = graph();
        modules.push(module(0, "duplicate", None, SourceTrust::User));
        assert!(matches!(
            ImportPolicy::default().resolve(&modules),
            Err(ImportPolicyError::DuplicateModuleId { .. })
        ));
        let mut modules = graph();
        modules.push(module(99, "app", None, SourceTrust::User));
        assert!(matches!(
            ImportPolicy::default().resolve(&modules),
            Err(ImportPolicyError::DuplicateModulePath { .. })
        ));
        let mut modules = graph();
        modules[0].parent = Some(id(99));
        assert!(matches!(
            ImportPolicy::default().resolve(&modules),
            Err(ImportPolicyError::UnknownParent {
                parent: DeclarationId(99),
                ..
            })
        ));
        let mut modules = graph();
        modules[0].parent = Some(id(2));
        assert!(
            matches!(ImportPolicy::default().resolve(&modules),Err(ImportPolicyError::CyclicParents{modules}) if modules.len()==4)
        );
        let resolved = ImportPolicy::default().resolve(&graph()).unwrap();
        assert!(matches!(
            resolved.check(id(99), id(11)),
            Err(ImportPolicyError::UnknownModule {
                role: ModuleRole::Referrer,
                ..
            })
        ));
        assert!(matches!(
            resolved.check(id(0), id(99)),
            Err(ImportPolicyError::UnknownModule {
                role: ModuleRole::Target,
                ..
            })
        ));
    }

    #[test]
    fn denied_diagnostics_retain_canonical_module_identity() {
        let resolved = ImportPolicy {
            ceiling: only(&[]),
            scopes: vec![],
        }
        .resolve(&graph())
        .unwrap();
        let error = resolved.check(id(1), id(11)).unwrap_err();
        assert!(
            matches!(&error,ImportPolicyError::Denied {referrer,target} if referrer.id==id(1) && target.id==id(11) && referrer.path.as_ref()=="app::worker" && target.path.as_ref()=="native::task")
        );
        assert_eq!(
            error.to_string(),
            "module `app::worker` may not import native module `native::task`"
        );
        let diagnostic = import_policy_diagnostic(error, Span::new(4, 8));
        assert_eq!(diagnostic.code, "E174");
        assert_eq!(diagnostic.span, Span::new(4, 8));
    }
}
