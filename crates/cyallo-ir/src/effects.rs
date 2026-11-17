//! Conservative effects: observable purity is separate from traps/resources/termination.
use crate::{
    Diagnostic, Span, Type,
    ir::{Function, Op, RoutineKind, Slot},
};
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EffectSummary {
    /// False when cold callees retain declaration contracts and conservative
    /// resource effects rather than a fully analyzed transitive summary.
    pub transitive_complete: bool,
    pub reads_immutable: bool,
    pub reads_mutable_globals: bool,
    /// Reads through mutable managed identities, rather than owned value copies.
    pub reads_managed: bool,
    pub writes_globals: bool,
    pub writes_references: bool,
    pub io: bool,
    pub nondeterministic: bool,
    pub may_allocate: bool,
    pub may_fail: bool,
    pub may_diverge: bool,
}
impl EffectSummary {
    fn union(&mut self, other: Self) {
        self.reads_immutable |= other.reads_immutable;
        self.reads_mutable_globals |= other.reads_mutable_globals;
        self.reads_managed |= other.reads_managed;
        self.writes_globals |= other.writes_globals;
        self.writes_references |= other.writes_references;
        self.io |= other.io;
        self.nondeterministic |= other.nondeterministic;
        self.may_allocate |= other.may_allocate;
        self.may_fail |= other.may_fail;
        self.may_diverge |= other.may_diverge;
    }
    pub fn is_observably_pure(self) -> bool {
        self.is_readonly() && !self.reads_managed
    }
    pub fn is_readonly(self) -> bool {
        !self.reads_mutable_globals
            && !self.writes_globals
            && !self.writes_references
            && !self.io
            && !self.nondeterministic
    }
}
fn cycle(root: usize, current: usize, calls: &[Vec<(usize, Span)>], seen: &mut [bool]) -> bool {
    if seen[current] {
        return false;
    }
    seen[current] = true;
    calls[current]
        .iter()
        .any(|(to, _)| *to == root || cycle(root, *to, calls, seen))
}
pub fn analyze(
    functions: &mut [Function],
    services: &crate::services::Metadata,
) -> Result<(), Diagnostic> {
    analyze_selected(functions, None, services)
}
pub fn analyze_one(
    functions: &mut [Function],
    function: usize,
    services: &crate::services::Metadata,
) -> Result<(), Diagnostic> {
    analyze_selected(functions, Some(function), services)
}
fn analyze_selected(
    functions: &mut [Function],
    selected: Option<usize>,
    services: &crate::services::Metadata,
) -> Result<(), Diagnostic> {
    let mut effects = vec![EffectSummary::default(); functions.len()];
    let mut violations = vec![None; functions.len()];
    let mut calls = vec![vec![]; functions.len()];
    for (fi, f) in functions.iter().enumerate() {
        if selected.is_some_and(|wanted| wanted != fi) {
            continue;
        }
        let effect = &mut effects[fi];
        effect.may_allocate = !f.locals.is_empty();
        effect.may_fail = effect.may_allocate;
        let mut addresses: Vec<Option<Slot>> = vec![None; f.registers.len()];
        let mut reference_targets = std::collections::HashMap::new();
        for (pc, inst) in f.code.iter().enumerate() {
            let mut violation = None;
            match &inst.op {
                Op::Dynamic(_, op, _) => {
                    effect.may_allocate = true;
                    effect.may_fail = true;
                    if !matches!(
                        op,
                        crate::dynamic::Operation::Erase | crate::dynamic::Operation::Object(..)
                    ) {
                        effect.writes_references = true;
                        violation =
                            Some("pure fn cannot perform unchecked dynamic operations".into());
                    }
                }
                Op::Lookup(destination, op, arguments) => {
                    // The operation's contract; operations without a single origin
                    // carry the conservative contract (allocates and fails).
                    let contract = op.contract().effects;
                    effect.may_allocate |= contract.allocates;
                    effect.may_fail |= contract.may_fail || contract.allocates;
                    if matches!(
                        op,
                        crate::lookup::Operation::Concurrent(_)
                            | crate::lookup::Operation::ModuleLoad
                    ) {
                        effect.nondeterministic = true;
                        effect.writes_references = true;
                        violation = Some("pure fn cannot perform concurrent operations".into());
                    }
                    if matches!(op, crate::lookup::Operation::CloneValue)
                        && crate::ownership::effectful_lifecycle(&f.registers[*destination])
                    {
                        effect.writes_references = true;
                        violation =
                            Some("pure fn cannot clone a value with effectful lifecycle".into());
                    }
                    if matches!(
                        op,
                        crate::lookup::Operation::Allocate
                            | crate::lookup::Operation::Deallocate
                            | crate::lookup::Operation::RcNewIn { .. }
                            | crate::lookup::Operation::ManagedNew(cyallo_ast::MemoryKind::Rc, _)
                            | crate::lookup::Operation::RcClone
                            | crate::lookup::Operation::RcDowngrade
                            | crate::lookup::Operation::RcUpgrade
                            | crate::lookup::Operation::RcStrongCount
                            | crate::lookup::Operation::RcWeakCount
                    ) {
                        effect.writes_references = true;
                        violation = Some("pure fn cannot construct, clone or observe mutable reference-counted ownership".into());
                    }
                    if matches!(op, crate::lookup::Operation::DropValue)
                        && arguments.first().is_some_and(|argument| {
                            crate::ownership::effectful_lifecycle(&f.registers[*argument])
                        })
                    {
                        effect.writes_references = true;
                        violation =
                            Some("pure fn cannot destroy a value with effectful lifecycle".into());
                    }
                    effect.reads_managed |= matches!(
                        op,
                        crate::lookup::Operation::CellRead
                            | crate::lookup::Operation::LocationLoad
                            | crate::lookup::Operation::Mapped
                    );
                    effect.may_diverge |= matches!(op, crate::lookup::Operation::Mapped);
                    effect.nondeterministic |= matches!(op, crate::lookup::Operation::Symbol);
                    if effect.reads_managed {
                        violation = Some("pure fn cannot read mutable managed storage; use a proc or pass an immutable value snapshot".into());
                    }
                    if matches!(
                        op,
                        crate::lookup::Operation::CollectionPush(_)
                            | crate::lookup::Operation::CollectionPop(_)
                    ) {
                        let receiver = arguments[0];
                        record_store(fi, addresses[receiver], f, effect);
                        if effect.writes_references || effect.writes_globals {
                            violation = Some(
                                "pure fn cannot mutate a borrowed collection or global".into(),
                            );
                        }
                    } else if op.writes() {
                        effect.writes_references = true;
                        violation =
                            Some("pure fn cannot mutate managed storage or create symbols".into());
                    }
                }
                Op::ServiceKey(..) => {}
                Op::TraitBox(..) => {
                    effect.may_allocate = true;
                    effect.may_fail = true;
                }
                Op::TraitCall(_, receiver, method, _) if matches!(&f.registers[*receiver],Type::TraitObject(name) if services.trait_id(name).is_some_and(|id|services.traits[id].methods[*method].kind==RoutineKind::Function)) =>
                {
                    effect.reads_managed = true;
                    effect.may_allocate = true;
                    effect.may_fail = true;
                    effect.may_diverge = true;
                    violation =
                        Some("pure fn cannot read through a mutable erased receiver".into());
                }
                Op::Service(..) | Op::TraitCall(..) => {
                    effect.may_allocate = true;
                    effect.may_fail = true;
                    effect.may_diverge = true;
                    effect.reads_mutable_globals = true;
                    effect.writes_references = true;
                    violation = Some(
                        "pure fn cannot resolve services or invoke an effectful erased method"
                            .into(),
                    );
                }
                Op::Address(r, slot) => {
                    addresses[*r] = Some(*slot);
                    if slot.global || fi == 0 {
                        let global = &functions[0].locals[slot.index];
                        if global.constant {
                            effect.reads_immutable = true;
                        } else {
                            effect.reads_mutable_globals = true;
                            violation = Some(format!(
                                "pure fn cannot access mutable global {}",
                                global.name
                            ));
                        }
                    }
                }
                Op::Index(r, a, _, write) => {
                    addresses[*r] = addresses[*a];
                    effect.may_fail = true;
                    effect.may_allocate |= *write;
                    if *write {
                        record_store(fi, addresses[*a], f, effect);
                    }
                }
                Op::EnumPayload(r, a, _, crate::enums::Projection::Address, _)
                | Op::Field(r, a, _) => {
                    addresses[*r] = addresses[*a];
                    effect.may_fail = true;
                }
                Op::Extract(..) => effect.may_fail = true,
                Op::Invoke(_, callee, _) => {
                    effect.transitive_complete = false;
                    effect.may_allocate = true;
                    effect.may_fail = true;
                    effect.may_diverge = true;
                    if cyallo_ast::callable(&f.registers[*callee])
                        .is_some_and(|(k, _, _)| k == RoutineKind::Procedure)
                    {
                        effect.io = true;
                        effect.nondeterministic = true;
                        effect.reads_mutable_globals = true;
                        effect.writes_globals = true;
                        effect.writes_references = true;
                        violation = Some("pure fn cannot call an effectful procedure value".into());
                    }
                }
                Op::Store(a, value) => {
                    if matches!(f.registers[*value], Type::Reference { .. })
                        && let Some(slot) = addresses[*a]
                    {
                        reference_targets.insert((slot.global, slot.index), addresses[*value]);
                    }
                    record_store(fi, addresses[*a], f, effect);
                    if effect.writes_references {
                        violation = Some("pure fn cannot mutate a ref capture".into());
                    }
                }
                Op::Load(result, a) | Op::Take(result, a) | Op::Length(result, a) => {
                    if matches!(f.registers[*result], Type::Reference { .. }) {
                        addresses[*result] = addresses[*a].and_then(|slot| {
                            reference_targets
                                .get(&(slot.global, slot.index))
                                .copied()
                                .flatten()
                        });
                    }
                    effect.may_fail = true;
                    effect.reads_managed |= addresses[*a]
                        .is_some_and(|s| !s.global && f.params.get(s.index) == Some(&true));
                    if effect.reads_managed {
                        violation = Some("pure fn cannot read a mutable ref capture".into());
                    }
                }
                Op::CheckStep(_) => effect.may_fail = true,
                Op::ManagedAddress(_, _) => effect.may_fail = true,
                Op::Drop(a) => {
                    if crate::ownership::effectful_lifecycle(&f.registers[*a]) {
                        effect.writes_references = true;
                        violation =
                            Some("pure fn cannot destroy a value with effectful lifecycle".into());
                    }
                }
                Op::Input(a, _) => {
                    effect.io = true;
                    effect.may_fail = true;
                    record_store(fi, addresses[*a], f, effect);
                    violation = Some("pure fn cannot read input".into());
                }
                Op::FrameInput(..) => {
                    effect.io = true;
                    effect.may_fail = true;
                    violation = Some("pure fn cannot read input".into());
                }
                Op::Output(..) => {
                    effect.io = true;
                    violation = Some("pure fn cannot produce output".into());
                }
                Op::ArrayLiteral(..)
                | Op::Frozen(..)
                | Op::EnumConstruct(..)
                | Op::Aggregate(..)
                | Op::FunctionValue(..)
                | Op::LiteralString(..) => effect.may_allocate = true,
                Op::Unary(..) | Op::Binary(..) => effect.may_fail = true,
                Op::Builtin(r, name, _) => {
                    // The intrinsic's contract states what it can do. Allocation can
                    // exhaust resources, so it still counts as a possible failure.
                    let contract = name.contract().effects;
                    effect.may_fail |= contract.may_fail || contract.allocates;
                    effect.may_allocate |= contract.allocates
                        || matches!(
                            f.registers[*r],
                            Type::String | Type::Array(..) | Type::SparseArray(_)
                        );
                    effect.io |= contract.io;
                    if !contract.is_pure() {
                        effect.nondeterministic = true;
                        violation = Some(format!("pure fn cannot call effectful builtin `{name}`"));
                    }
                }
                Op::Call(_, to, _) => {
                    calls[fi].push((*to, inst.span));
                    effect.may_fail = true;
                    if functions[*to].kind == RoutineKind::Procedure {
                        violation = Some(format!(
                            "pure fn cannot call procedure {}",
                            functions[*to].name
                        ));
                    }
                }
                Op::Jump(to) if *to <= pc => effect.may_diverge = true,
                Op::Branch(_, yes, no) if *yes <= pc || *no <= pc => effect.may_diverge = true,
                Op::Move(r, a) => addresses[*r] = addresses[*a],
                _ => {}
            }
            if let Some(message) = violation {
                violations[fi].get_or_insert((inst.span, message));
            }
        }
    }
    for (fi, effect) in effects.iter_mut().enumerate() {
        if selected.is_some_and(|wanted| wanted != fi) {
            continue;
        }
        effect.may_diverge |= cycle(fi, fi, &calls, &mut vec![false; functions.len()]);
    }
    loop {
        let previous = effects.clone();
        for (fi, edges) in calls.iter().enumerate() {
            for (to, _) in edges {
                effects[fi].union(previous[*to]);
            }
        }
        if previous == effects {
            break;
        }
    }
    for (fi, f) in functions.iter().enumerate().filter(|(_, f)| f.pure) {
        if selected.is_some_and(|wanted| wanted != fi) {
            continue;
        }
        if let Some((span, message)) = &violations[fi] {
            return Err(Diagnostic::new("E120", message.clone(), *span)
                .with_related(f.span, "declared pure function"));
        }
        if !effects[fi].is_observably_pure() {
            let (to, span) = calls[fi]
                .iter()
                .find(|(to, _)| !effects[*to].is_observably_pure())
                .expect("effect originates in call");
            let mut diagnostic = Diagnostic::new(
                "E120",
                format!(
                    "pure fn {} calls effectful function {}",
                    f.name, functions[*to].name
                ),
                *span,
            )
            .with_related(
                functions[*to].span,
                "effects originate through this routine",
            );
            if let Some((origin, message)) = &violations[*to] {
                diagnostic = diagnostic.with_related(*origin, message.clone());
            }
            return Err(diagnostic);
        }
    }
    for (f, effect) in functions.iter_mut().zip(effects) {
        f.effects = effect;
        f.effects.transitive_complete = !f.code.iter().any(|i| {
            matches!(
                i.op,
                Op::Invoke(..)
                    | Op::TraitCall(..)
                    | Op::Service(..)
                    | Op::Lookup(_, crate::lookup::Operation::Mapped, _)
            )
        });
    }
    Ok(())
}
fn record_store(fi: usize, slot: Option<Slot>, f: &Function, effect: &mut EffectSummary) {
    if let Some(slot) = slot {
        if slot.global || fi == 0 {
            effect.writes_globals = true;
        } else if f.params.get(slot.index) == Some(&true) {
            effect.writes_references = true;
        }
    } else {
        effect.writes_references = true;
    }
}

/// Check effects intrinsic to this body. Static callees are visited separately;
/// erased targets are checked after resolution, before executing their body.
pub fn readonly_body(
    function: &Function,
    main: &Function,
    services: &crate::services::Metadata,
) -> bool {
    let mut body = function.clone();
    body.pure = false;
    for instruction in &mut body.code {
        if matches!(instruction.op, Op::Call(..) | Op::TraitCall(..)) {
            instruction.op = Op::Nop;
        }
    }
    let mut bodies = vec![main.clone(), body];
    analyze_one(&mut bodies, 1, services).is_ok() && bodies[1].effects.is_readonly()
}
