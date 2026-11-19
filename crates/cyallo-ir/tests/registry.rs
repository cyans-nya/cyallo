use cyallo_ir::contract::*;
use cyallo_ir::namespace;
use cyallo_ir::registry::*;
use cyallo_ir::sdk::{Digest, InterfaceDigests};
const SDK: &str = r#"
pub interface mod tasks {
    pub opaque record Handle;
    impl Handle {
        pub fn finished(self: &Self) -> bool;
        pub proc cancel(self: &Self);
    }
    pub record Failure { message: string }
    pub proc spawn(work: i64) -> Handle;
    pub fn sum(a: i64, b: i64) -> i64;
    pub fn helper(n: i64) -> i64 { return n + 1 }
}
"#;
fn sdk(source: &str) -> InterfaceDigests {
    namespace::single(cyallo_parser::parse(source).unwrap())
        .unwrap()
        .interface
}
fn digest(d: &InterfaceDigests, path: &str) -> Digest {
    d.item(path)
        .unwrap_or_else(|| panic!("no item {path}"))
        .digest
}
fn host(effects: Effects, safepoint: Safepoint) -> BindingContract {
    BindingContract {
        disposition: Disposition::HostCall,
        effects,
        safepoint,
        operands: OperandModes {
            receiver: Operand::Read,
            rest: Operand::Read,
            exceptions: &[],
        },
    }
}
fn engine(effects: Effects, safepoint: Safepoint) -> BindingContract {
    BindingContract {
        disposition: Disposition::EnginePrimitive,
        ..host(effects, safepoint)
    }
}
const NONE: Effects = Effects::NONE;
fn opaque() -> OpaqueContract {
    OpaqueContract {
        copyable: false,
        transferable: false,
        drop_runs_guest: false,
        holds_managed_references: true,
    }
}
fn complete(d: &InterfaceDigests) -> Registry {
    Registry::new()
        .opaque_type("tasks::Handle", digest(d, "tasks::Handle"), opaque())
        .host_call(
            "tasks::spawn",
            digest(d, "tasks::spawn"),
            host(
                Effects {
                    may_fail: true,
                    allocates: true,
                    fresh_identity: true,
                    nondeterministic: true,
                    ..NONE
                },
                Safepoint::MayCollect,
            ),
        )
        .host_call(
            "tasks::Handle::finished",
            digest(d, "tasks::Handle::finished"),
            host(
                Effects {
                    reads_memory: true,
                    nondeterministic: true,
                    ..NONE
                },
                Safepoint::Never,
            ),
        )
        .host_call(
            "tasks::Handle::cancel",
            digest(d, "tasks::Handle::cancel"),
            host(
                Effects {
                    writes_memory: true,
                    ..NONE
                },
                Safepoint::Never,
            ),
        )
        .intrinsic(
            "tasks::sum",
            digest(d, "tasks::sum"),
            engine(
                Effects {
                    may_fail: true,
                    ..NONE
                },
                Safepoint::Never,
            ),
            Some(Reference::Source("sum_reference".into())),
            vec![Engine::Cranelift],
        )
}
#[test]
fn only_bodyless_items_and_opaque_records_require_binding() {
    let d = sdk(SDK);
    let required: Vec<_> = d
        .items
        .iter()
        .filter(|i| i.requires_binding)
        .map(|i| i.path.as_ref())
        .collect();
    assert_eq!(
        required,
        [
            "tasks::Handle",
            "tasks::Handle::cancel",
            "tasks::Handle::finished",
            "tasks::spawn",
            "tasks::sum"
        ]
    );
    assert_eq!(d.item("tasks::sum").unwrap().arity, Some(2));
    assert_eq!(d.item("tasks::Handle::finished").unwrap().arity, Some(1));
}
#[test]
fn a_complete_valid_registration_installs_and_exposes_contracts() {
    let d = sdk(SDK);
    let installed = complete(&d).install(&d).unwrap();
    assert_eq!(installed.len(), 5);
    assert_eq!(
        installed.contract("tasks::sum").unwrap().disposition,
        Disposition::EnginePrimitive
    );
    assert!(installed.contract("tasks::sum").unwrap().inlinable());
    assert!(!installed.contract("tasks::spawn").unwrap().inlinable());
    assert!(
        installed
            .opaque("tasks::Handle")
            .unwrap()
            .holds_managed_references
    );
    assert!(installed.contract("tasks::Handle").is_none());
    assert!(installed.get("tasks::helper").is_none());
}
#[test]
fn missing_unknown_unbindable_and_mismatched_registrations_are_reported_together() {
    let d = sdk(SDK);
    let errors = Registry::new()
        .host_call(
            "tasks::nothing",
            digest(&d, "tasks::sum"),
            host(NONE, Safepoint::Never),
        )
        .host_call(
            "tasks::helper",
            digest(&d, "tasks::helper"),
            host(NONE, Safepoint::Never),
        )
        .host_call(
            "tasks::Failure",
            digest(&d, "tasks::Failure"),
            host(NONE, Safepoint::Never),
        )
        .host_call(
            "tasks::Handle",
            digest(&d, "tasks::Handle"),
            host(NONE, Safepoint::Never),
        )
        .opaque_type("tasks::spawn", digest(&d, "tasks::spawn"), opaque())
        .install(&d)
        .unwrap_err();
    let has = |f: &dyn Fn(&RegistrationError) -> bool| errors.iter().any(f);
    assert!(has(
        &|e| matches!(e, RegistrationError::UnknownItem { path } if path.as_ref() == "tasks::nothing")
    ));
    assert!(has(
        &|e| matches!(e, RegistrationError::NotBindable { path } if path.as_ref() == "tasks::helper")
    ));
    assert!(has(
        &|e| matches!(e, RegistrationError::NotBindable { path } if path.as_ref() == "tasks::Failure")
    ));
    assert!(has(
        &|e| matches!(e, RegistrationError::KindMismatch { path, .. } if path.as_ref() == "tasks::Handle")
    ));
    assert!(has(
        &|e| matches!(e, RegistrationError::KindMismatch { path, .. } if path.as_ref() == "tasks::spawn")
    ));
    // Items whose registration failed are not also reported as unbound; the rest are.
    let unbound: Vec<_> = errors
        .iter()
        .filter_map(|e| match e {
            RegistrationError::Unbound { path } => Some(path.as_ref()),
            _ => None,
        })
        .collect();
    assert_eq!(
        unbound,
        [
            "tasks::Handle::cancel",
            "tasks::Handle::finished",
            "tasks::sum"
        ]
    );
}
#[test]
fn a_digest_from_an_older_sdk_is_rejected_precisely() {
    let old = sdk(SDK);
    let new = sdk(&SDK.replace(
        "pub fn sum(a: i64, b: i64) -> i64;",
        "pub fn sum(a: i64, b: i64, c: i64) -> i64;",
    ));
    let errors = complete(&old).install(&new).unwrap_err();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(
        errors[0],
        RegistrationError::DigestMismatch {
            path: "tasks::sum".into(),
            registered: digest(&old, "tasks::sum"),
            sdk: digest(&new, "tasks::sum"),
        }
    );
    assert!(
        errors[0]
            .to_string()
            .contains("implemented against sha256:")
    );
}
#[test]
fn duplicate_and_missing_bindings_are_reported() {
    let d = sdk(SDK);
    let duplicated = complete(&d)
        .host_call(
            "tasks::spawn",
            digest(&d, "tasks::spawn"),
            host(
                Effects {
                    allocates: true,
                    ..NONE
                },
                Safepoint::MayCollect,
            ),
        )
        .install(&d)
        .unwrap_err();
    assert_eq!(
        duplicated,
        [RegistrationError::Duplicate {
            path: "tasks::spawn".into()
        }]
    );
    let missing = Registry::new().install(&d).unwrap_err();
    assert_eq!(missing.len(), 5);
    assert!(
        missing
            .iter()
            .all(|e| matches!(e, RegistrationError::Unbound { .. }))
    );
}
#[test]
fn inconsistent_contracts_are_rejected() {
    let d = sdk(SDK);
    let rule = |contract: BindingContract| {
        let r = Registry::new().host_call("tasks::spawn", digest(&d, "tasks::spawn"), contract);
        match r
            .install(&d)
            .unwrap_err()
            .into_iter()
            .find_map(|e| match e {
                RegistrationError::Contract { violation, .. } => Some(violation),
                _ => None,
            }) {
            Some(v) => v,
            None => panic!("no contract violation reported"),
        }
    };
    assert_eq!(
        rule(host(
            Effects {
                may_park: true,
                ..NONE
            },
            Safepoint::MayCollect
        )),
        ContractViolation::ParkingNeedsReentry
    );
    assert_eq!(
        rule(host(
            Effects {
                calls_guest: true,
                ..NONE
            },
            Safepoint::Never
        )),
        ContractViolation::GuestCallNeedsReentry
    );
    assert_eq!(
        rule(host(
            Effects {
                allocates: true,
                ..NONE
            },
            Safepoint::Never
        )),
        ContractViolation::AllocationNeedsSafepoint
    );
    assert_eq!(
        rule(engine(NONE, Safepoint::Never)),
        ContractViolation::HostCallDisposition
    );
    assert_eq!(
        rule(BindingContract {
            disposition: Disposition::SourceCandidate,
            ..host(NONE, Safepoint::Never)
        }),
        ContractViolation::HostCallDisposition
    );
}
#[test]
fn intrinsics_need_the_engine_disposition_and_reference_semantics() {
    let d = sdk(SDK);
    let violation = |contract: BindingContract, reference: Option<Reference>| {
        Registry::new()
            .intrinsic(
                "tasks::sum",
                digest(&d, "tasks::sum"),
                contract,
                reference,
                vec![],
            )
            .install(&d)
            .unwrap_err()
            .into_iter()
            .find_map(|e| match e {
                RegistrationError::Contract { violation, .. } => Some(violation),
                _ => None,
            })
    };
    let reference = Some(Reference::Declarative("sum".into()));
    assert_eq!(
        violation(engine(NONE, Safepoint::Never), None),
        Some(ContractViolation::MissingReference)
    );
    assert_eq!(
        violation(host(NONE, Safepoint::Never), reference.clone()),
        Some(ContractViolation::IntrinsicDisposition)
    );
    assert_eq!(violation(engine(NONE, Safepoint::Never), reference), None);
}
#[test]
fn operand_exceptions_must_name_real_parameters() {
    let d = sdk(SDK);
    let contract = BindingContract {
        operands: OperandModes {
            receiver: Operand::Read,
            rest: Operand::Read,
            exceptions: &[(2, Operand::Transfer)],
        },
        ..host(NONE, Safepoint::Never)
    };
    // `sum` has two parameters: operand 2 does not exist.
    let errors = Registry::new()
        .host_call("tasks::sum", digest(&d, "tasks::sum"), contract)
        .install(&d)
        .unwrap_err();
    assert!(errors.iter().any(|e| matches!(
        e,
        RegistrationError::Contract {
            violation: ContractViolation::OperandOutOfRange { index: 2, arity: 2 },
            ..
        }
    )));
}

#[test]
fn a_zero_argument_routine_cannot_override_operand_zero() {
    let d = sdk("pub interface mod api { pub proc flush(); }");
    let contract = BindingContract {
        operands: OperandModes {
            receiver: Operand::Read,
            rest: Operand::Read,
            exceptions: &[(0, Operand::Transfer)],
        },
        ..host(NONE, Safepoint::Never)
    };
    let errors = Registry::new()
        .host_call("api::flush", digest(&d, "api::flush"), contract)
        .install(&d)
        .unwrap_err();
    assert_eq!(
        errors,
        [RegistrationError::Contract {
            path: "api::flush".into(),
            violation: ContractViolation::OperandOutOfRange { index: 0, arity: 0 },
        }]
    );
}

#[test]
fn linked_provider_contracts_follow_resolved_aliases_and_stay_independent() {
    let source = format!(
        "{SDK}\nmod consumer {{\n use crate::tasks::sum as add\n fn wrap() -> i64 {{ return add(1, 2) }}\n}}"
    );
    let program = namespace::single(cyallo_parser::parse(&source).unwrap()).unwrap();
    let declaration = |path: &str, name: &str| {
        program
            .modules
            .iter()
            .find(|module| module.path.as_ref() == path)
            .unwrap()
            .bindings
            .iter()
            .find(|binding| binding.name.as_ref() == name)
            .unwrap()
            .declaration
    };
    let sum = declaration("crate::tasks", "sum");
    assert_eq!(declaration("crate::consumer", "add"), sum);
    let strict = complete(&program.interface)
        .install_for_program(&program)
        .unwrap();
    let other_effects = Effects {
        may_fail: true,
        nondeterministic: true,
        ..NONE
    };
    let other = Registry::new()
        .opaque_type(
            "tasks::Handle",
            digest(&program.interface, "tasks::Handle"),
            opaque(),
        )
        .host_call(
            "tasks::spawn",
            digest(&program.interface, "tasks::spawn"),
            host(NONE, Safepoint::Never),
        )
        .host_call(
            "tasks::Handle::finished",
            digest(&program.interface, "tasks::Handle::finished"),
            host(NONE, Safepoint::Never),
        )
        .host_call(
            "tasks::Handle::cancel",
            digest(&program.interface, "tasks::Handle::cancel"),
            host(NONE, Safepoint::Never),
        )
        .host_call(
            "tasks::sum",
            digest(&program.interface, "tasks::sum"),
            host(other_effects, Safepoint::Never),
        )
        .install_for_program(&program)
        .unwrap();
    assert!(matches!(
        &strict.get(sum).unwrap().implementation,
        Implementation::Intrinsic { .. }
    ));
    assert!(
        matches!(&other.get(sum).unwrap().implementation, Implementation::HostCall(c) if c.effects == other_effects)
    );
    assert!(matches!(
        &strict
            .get(declaration("crate::tasks", "Handle"))
            .unwrap()
            .implementation,
        Implementation::Opaque(_)
    ));
    assert!(strict.get(declaration("crate::tasks", "helper")).is_none());
}

#[test]
fn registries_are_independent_values() {
    let d = sdk(SDK);
    let strict = complete(&d).install(&d).unwrap();
    // A second runtime binds `tasks::sum` as a host call instead.
    let other = Registry::new()
        .opaque_type(
            "tasks::Handle",
            digest(&d, "tasks::Handle"),
            OpaqueContract {
                transferable: true,
                ..opaque()
            },
        )
        .host_call(
            "tasks::spawn",
            digest(&d, "tasks::spawn"),
            host(
                Effects {
                    allocates: true,
                    ..NONE
                },
                Safepoint::MayCollect,
            ),
        )
        .host_call(
            "tasks::Handle::finished",
            digest(&d, "tasks::Handle::finished"),
            host(NONE, Safepoint::Never),
        )
        .host_call(
            "tasks::Handle::cancel",
            digest(&d, "tasks::Handle::cancel"),
            host(NONE, Safepoint::Never),
        )
        .host_call(
            "tasks::sum",
            digest(&d, "tasks::sum"),
            host(NONE, Safepoint::Never),
        )
        .install(&d)
        .unwrap();
    assert_eq!(
        strict.contract("tasks::sum").unwrap().disposition,
        Disposition::EnginePrimitive
    );
    assert_eq!(
        other.contract("tasks::sum").unwrap().disposition,
        Disposition::HostCall
    );
    assert!(!strict.opaque("tasks::Handle").unwrap().transferable);
    assert!(other.opaque("tasks::Handle").unwrap().transferable);
}
