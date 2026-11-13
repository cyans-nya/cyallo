//! Session-owned resolution slots. Provider churn changes data, never bytecode.
use super::error::{AllocatorError, Invariant, ResourceLimit, ServiceError, ValueKind, expected};
use super::{RunResult, Session, Value, gc};
use crate::{ir::Reg, services::Operation};
use std::collections::HashMap;
#[derive(Debug, Clone, Default)]
pub struct ServiceStats {
    pub resolutions: u64,
    pub slot_hits: u64,
    pub provisions: u64,
    pub withdrawals: u64,
    pub target_hits: u64,
    pub native_target_hits: u64,
    pub target_misses: u64,
    pub max_target_cases: u64,
    pub released: u64,
}
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NativeBinding {
    pub available: u64,
    pub implementation: u64,
    pub receiver: u64,
    pub binding: u64,
}
#[derive(Default)]
pub(super) struct NativePointers(Vec<*const NativeBinding>);
// SAFETY: each pointer refers to a Box<[NativeBinding]> owned by the same
// Session's View. Moving the Session preserves those allocations. Generated
// code borrows the tables only during an exclusive Session invocation.
unsafe impl Send for NativePointers {}
impl std::ops::Deref for NativePointers {
    type Target = Vec<*const NativeBinding>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for NativePointers {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TargetCase {
    pub implementation: u64,
    pub target: u64,
}
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CallCache {
    pub cases: [TargetCase; 4],
    pub count: u64,
}
struct Rooted {
    scope: gc::RootScope,
    id: gc::RootId,
}
impl Rooted {
    fn value(&self) -> Value {
        self.scope.value_copy(self.id)
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Active,
    Retiring,
    Inactive,
}
struct Binding {
    owner: usize,
    key: usize,
    realm: u64,
    state: State,
    value: Option<Rooted>,
    leases: usize,
}
struct View {
    parent: Option<usize>,
    realms: Vec<u64>,
    provided: Vec<Option<usize>>,
    routes: Vec<Option<usize>>,
    admitted: Vec<Option<usize>>,
    state: State,
    checked: bool,
    cleanups: Vec<Rooted>,
    native: Box<[NativeBinding]>,
}
#[derive(Default)]
pub(super) struct Services {
    active: HashMap<(usize, u64), usize>,
    views: Vec<View>,
    bindings: Vec<Binding>,
    pub caches: Vec<Box<[CallCache]>>,
    pub pointers: NativePointers,
    pub stats: ServiceStats,
    next_realm: u64,
    retirement_pending: bool,
}
impl Services {
    pub(super) fn relink(&mut self, functions: &[usize], keys: &[usize], key_count: usize) {
        fn remap<T: Clone>(old: &[T], keys: &[usize], count: usize, default: T) -> Vec<T> {
            let mut new = vec![default; count];
            for (i, value) in old.iter().enumerate() {
                new[keys[i]] = value.clone();
            }
            new
        }
        for binding in &mut self.bindings {
            binding.key = keys[binding.key];
            if let Some(root) = &binding.value {
                root.scope.edit_value(root.id, |v| {
                    super::remap_functions(v, functions);
                    super::remap_service_keys(v, keys);
                });
            }
        }
        for view in &mut self.views {
            for root in &view.cleanups {
                root.scope.edit_value(root.id, |v| {
                    super::remap_functions(v, functions);
                    super::remap_service_keys(v, keys);
                });
            }
            view.realms = remap(&view.realms, keys, key_count, 0);
            view.provided = remap(&view.provided, keys, key_count, None);
            view.admitted = remap(&view.admitted, keys, key_count, None);
            view.routes = vec![None; key_count];
            view.native = vec![NativeBinding::default(); key_count].into_boxed_slice();
        }
        self.active.clear();
        for (id, binding) in self.bindings.iter().enumerate() {
            if binding.state == State::Active {
                self.active.insert((binding.key, binding.realm), id);
            }
        }
        self.pointers.0 = self.views.iter().map(|v| v.native.as_ptr()).collect();
        self.caches.clear();
        self.refresh_all();
    }
    fn view(&mut self, parent: Option<usize>, keys: usize) -> usize {
        let id = self.views.len();
        let (realms, checked) = parent
            .map(|p| (self.views[p].realms.clone(), self.views[p].checked))
            .unwrap_or((vec![0; keys], false));
        self.views.push(View {
            parent,
            realms,
            provided: vec![None; keys],
            routes: vec![None; keys],
            admitted: parent
                .map(|p| self.views[p].admitted.clone())
                .unwrap_or_else(|| vec![None; keys]),
            state: State::Active,
            checked,
            cleanups: vec![],
            native: vec![NativeBinding::default(); keys].into_boxed_slice(),
        });
        self.pointers.push(self.views[id].native.as_ptr());
        self.refresh_all();
        id
    }
    fn route(&self, view: usize, key: usize) -> Option<usize> {
        self.active
            .get(&(key, self.views[view].realms[key]))
            .copied()
    }
    fn refresh_key(&mut self, key: usize) {
        for v in 0..self.views.len() {
            self.stats.resolutions += 1;
            self.views[v].routes[key] = self.route(v, key);
            self.update_native(v, key);
        }
    }
    fn refresh_all(&mut self) {
        if let Some(v) = self.views.first() {
            for key in 0..v.routes.len() {
                self.refresh_key(key);
            }
        }
    }
    fn selected(&self, view: usize, key: usize, cleanup: bool) -> RunResult<usize> {
        let v = self.views.get(view).ok_or(expected(ValueKind::Context))?;
        if v.state == State::Inactive || (v.state == State::Retiring && !cleanup) {
            return Err(ServiceError::InactiveContext.into());
        }
        let own = v.provided[key].filter(|b| {
            self.bindings[*b].state == State::Active
                || (cleanup && self.bindings[*b].state == State::Retiring)
        });
        let b = if v.checked {
            own.or(v.admitted[key])
        } else {
            own.or(v.routes[key])
        }
        .ok_or(ServiceError::NotInjected)?;
        let binding = &self.bindings[b];
        if binding.state == State::Inactive
            || (binding.state == State::Retiring
                && !(cleanup && (v.admitted[key] == Some(b) || v.provided[key] == Some(b))))
        {
            return Err(ServiceError::Unavailable.into());
        }
        Ok(b)
    }
    fn update_native(&mut self, view: usize, key: usize) {
        let selected = self.selected(view, key, false).ok();
        let value = selected.and_then(|b| self.bindings[b].value.as_ref().map(|v| (b, v.value())));
        self.views[view].native[key] = match value {
            Some((binding, Value::TraitObject(receiver, implementation))) => NativeBinding {
                available: 1,
                implementation: implementation as u64,
                receiver: receiver as u64,
                binding: binding as u64,
            },
            _ => NativeBinding::default(),
        };
    }
    pub(super) fn refresh_native(&mut self) {
        for view in 0..self.views.len() {
            for key in 0..self.views[view].routes.len() {
                self.update_native(view, key);
            }
        }
    }
}
impl Session {
    pub(super) fn prepare_service_native(&mut self) {
        while self.services.caches.len() < self.program.functions.len() {
            self.services.caches.push(Box::default());
        }
        if let Some(f) = self.frames.last()
            && self.services.caches[f.function].is_empty()
        {
            self.services.caches[f.function] =
                vec![CallCache::default(); self.program.functions[f.function].code.len()]
                    .into_boxed_slice();
        }
    }
    pub(super) fn release_service_lease(&mut self, binding: usize) {
        self.services.bindings[binding].leases -= 1;
    }
    pub fn service_stats(&self) -> &ServiceStats {
        &self.services.stats
    }
    fn service_context(&self, value: &Value) -> RunResult<usize> {
        if let Value::Context(v) = value {
            Ok(*v)
        } else {
            Err(expected(ValueKind::Context))
        }
    }
    fn service_key(&self, value: &Value) -> RunResult<usize> {
        if let Value::ServiceKey(k) = value
            && *k < self.program.services.keys.len()
        {
            return Ok(*k);
        }
        Err(expected(ValueKind::ServiceKey))
    }
    fn root_service_value(&self, value: Value) -> Rooted {
        let scope = self.root_scope();
        let id = scope.value(value);
        Rooted { scope, id }
    }
    fn cleanup_admitted(&self, view: usize) -> bool {
        self.frames.iter().any(|f| f.service_scope == Some(view))
    }
    pub(super) fn execute_service(
        &mut self,
        r: Reg,
        operation: Operation,
        regs: &[Reg],
    ) -> RunResult<()> {
        let values: Vec<_> = regs.iter().map(|&r| self.register(r)).collect();
        use Operation::*;
        if operation == Context {
            if self.services.views.len() >= self.context.max_cells {
                return Err(ResourceLimit::Context.into());
            }
            let v = self.services.view(None, self.program.services.keys.len());
            self.set(r, Value::Context(v));
            return Ok(());
        }
        if operation == Dispose {
            match values[0] {
                Value::Registration(b) => self.retire_binding(b)?,
                Value::Context(v) => self.retire_view(v)?,
                _ => return Err(expected(ValueKind::Registration)),
            };
            return Ok(());
        }
        let view = self.service_context(&values[0])?;
        if operation == Reference {
            let key = self.service_key(&values[1])?;
            self.set(r, Value::ServiceRef(view, key));
            return Ok(());
        }
        if operation == Available {
            let key = self.service_key(&values[1])?;
            let available = self
                .services
                .selected(view, key, self.cleanup_admitted(view))
                .is_ok();
            self.set(r, Value::Boolean(available));
            return Ok(());
        }
        if self.services.views[view].state != State::Active {
            return Err(ServiceError::InactiveContext.into());
        }
        match operation {
            Extend | Isolate => {
                if self.services.views.len() >= self.context.max_cells {
                    return Err(ResourceLimit::Context.into());
                }
                let v = self
                    .services
                    .view(Some(view), self.program.services.keys.len());
                if operation == Isolate {
                    let key = self.service_key(&values[1])?;
                    self.services.next_realm += 1;
                    self.services.views[v].realms[key] = self.services.next_realm;
                    self.services.views[v].admitted[key] = None;
                    self.services.refresh_key(key);
                }
                self.set(r, Value::Context(v));
            }
            Reference => {
                let key = self.service_key(&values[1])?;
                self.set(r, Value::ServiceRef(view, key));
            }
            Provide => {
                let key = self.service_key(&values[1])?;
                let realm = self.services.views[view].realms[key];
                if self.services.active.contains_key(&(key, realm)) {
                    return Err(ServiceError::ProviderConflict.into());
                }
                if self.services.bindings.len() >= self.context.max_cells {
                    return Err(ResourceLimit::Registration.into());
                }
                let value = self.root_service_value(values[2].clone());
                let binding = self.services.bindings.len();
                self.services.bindings.push(Binding {
                    owner: view,
                    key,
                    realm,
                    state: State::Active,
                    value: Some(value),
                    leases: 0,
                });
                self.services.views[view].provided[key] = Some(binding);
                self.services.active.insert((key, realm), binding);
                self.services.stats.provisions += 1;
                self.services.refresh_key(key);
                self.set(r, Value::Registration(binding));
            }
            Available => {
                let key = self.service_key(&values[1])?;
                let available = self
                    .services
                    .selected(view, key, self.cleanup_admitted(view))
                    .is_ok();
                self.set(r, Value::Boolean(available));
            }
            OnDispose => {
                let value = self.root_service_value(values[1].clone());
                self.services.views[view].cleanups.push(value);
            }
            Inject => {
                let key = self.service_key(&values[1])?;
                let binding = self
                    .services
                    .selected(view, key, self.cleanup_admitted(view))?;
                let child = self
                    .services
                    .view(Some(view), self.program.services.keys.len());
                self.services.views[child].checked = true;
                self.services.views[child].admitted[key] = Some(binding);
                self.services.refresh_key(key);
                let Value::Function(function, mut args) = values[2].clone() else {
                    return Err(ServiceError::InjectRequiresCallback.into());
                };
                args.push(Value::Context(child));
                self.call(function, args, None)?;
                if let Some(f) = self.frames.last_mut() {
                    f.service_scope = Some(child);
                }
            }
            _ => unreachable!(),
        }
        Ok(())
    }
    fn collect_binding_cleanup(&mut self, binding: usize, plan: &mut Vec<(usize, Value)>) {
        if self.services.bindings[binding].state != State::Active {
            return;
        }
        self.services.retirement_pending = true;
        self.services.bindings[binding].state = State::Retiring;
        self.services.stats.withdrawals += 1;
        let key = self.services.bindings[binding].key;
        self.services
            .active
            .remove(&(key, self.services.bindings[binding].realm));
        self.services.refresh_key(key);
        let dependents: Vec<_> = self
            .services
            .views
            .iter()
            .enumerate()
            .filter(|(_, v)| v.admitted[key] == Some(binding) && v.state == State::Active)
            .map(|(id, _)| id)
            .collect();
        for view in dependents {
            self.collect_view_cleanup(view, plan);
        }
    }
    fn collect_view_cleanup(&mut self, view: usize, plan: &mut Vec<(usize, Value)>) {
        if self.services.views[view].state != State::Active {
            return;
        }
        self.services.retirement_pending = true;
        self.services.views[view].state = State::Retiring;
        let children: Vec<_> = self
            .services
            .views
            .iter()
            .enumerate()
            .filter(|(_, v)| v.parent == Some(view) && v.state == State::Active)
            .map(|(id, _)| id)
            .collect();
        for child in children {
            self.collect_view_cleanup(child, plan);
        }
        let provided: Vec<_> = self
            .services
            .bindings
            .iter()
            .enumerate()
            .filter(|(_, b)| b.owner == view && b.state == State::Active)
            .map(|(id, _)| id)
            .collect();
        for binding in provided {
            self.collect_binding_cleanup(binding, plan);
        }
        for callback in self.services.views[view].cleanups.iter().rev() {
            plan.push((view, callback.value()));
        }
    }
    fn run_cleanup_plan(&mut self, plan: Vec<(usize, Value)>) -> RunResult<()> {
        // Every cleanup remains rooted by its view until its frame completes.
        for (view, callback) in plan.into_iter().rev() {
            let Value::Function(function, args) = callback else {
                return Err(AllocatorError::InvalidCleanup.into());
            };
            self.call(function, args, None)?;
            self.frames.last_mut().unwrap().service_scope = Some(view);
        }
        self.services.refresh_native();
        self.release_retired();
        Ok(())
    }
    fn retire_binding(&mut self, binding: usize) -> RunResult<()> {
        let mut plan = vec![];
        self.collect_binding_cleanup(binding, &mut plan);
        self.run_cleanup_plan(plan)
    }
    fn retire_view(&mut self, view: usize) -> RunResult<()> {
        let mut plan = vec![];
        self.collect_view_cleanup(view, &mut plan);
        self.run_cleanup_plan(plan)
    }
    pub(super) fn release_retired(&mut self) {
        if !self.services.retirement_pending {
            return;
        }
        for v in 0..self.services.views.len() {
            if self.services.views[v].state == State::Retiring && !self.cleanup_admitted(v) {
                self.services.views[v].state = State::Inactive;
                self.services.views[v].cleanups.clear();
                self.services.views[v].admitted.fill(None);
            }
        }
        for b in 0..self.services.bindings.len() {
            if self.services.bindings[b].state == State::Retiring
                && self.services.bindings[b].leases == 0
                && !self.cleanup_admitted(self.services.bindings[b].owner)
                && !self
                    .services
                    .views
                    .iter()
                    .any(|v| v.state != State::Inactive && v.admitted.contains(&Some(b)))
            {
                self.services.bindings[b].value = None;
                self.services.bindings[b].state = State::Inactive;
                self.services.stats.released += 1;
            }
        }
        self.services.refresh_native();
        self.services.retirement_pending = self
            .services
            .views
            .iter()
            .any(|v| v.state == State::Retiring)
            || self
                .services
                .bindings
                .iter()
                .any(|b| b.state == State::Retiring);
    }
    pub(super) fn trait_call(
        &mut self,
        result: Option<Reg>,
        target: Reg,
        method: usize,
        regs: &[Reg],
        pc: usize,
    ) -> RunResult<()> {
        let hint = std::mem::take(&mut self.native_view.trait_hint_target);
        let value = self.register(target);
        let mut lease = None;
        let (receiver, implementation) = match value {
            Value::TraitObject(r, i) => (r, i),
            Value::ServiceRef(view, key) => {
                let b = self
                    .services
                    .selected(view, key, self.cleanup_admitted(view))?;
                let Value::TraitObject(r, i) = self.services.bindings[b]
                    .value
                    .as_ref()
                    .ok_or(ServiceError::Released)?
                    .value()
                else {
                    return Err(ServiceError::TraitRequired.into());
                };
                self.services.stats.slot_hits += 1;
                self.services.bindings[b].leases += 1;
                lease = Some(b);
                (r, i)
            }
            _ => return Err(expected(ValueKind::TraitObject)),
        };
        self.observe_lookup_read(receiver);
        let function_id = self.frames.last().unwrap().function;
        while self.services.caches.len() < self.program.functions.len() {
            self.services.caches.push(Box::default());
        }
        if self.services.caches[function_id].is_empty() {
            self.services.caches[function_id] =
                vec![CallCache::default(); self.program.functions[function_id].code.len()]
                    .into_boxed_slice();
        }
        let cache = &mut self.services.caches[function_id][pc];
        let target = if hint != 0 {
            self.services.stats.native_target_hits += 1;
            self.services.stats.target_hits += 1;
            (hint - 1) as usize
        } else if let Some(case) = cache.cases[..cache.count as usize]
            .iter()
            .find(|c| c.implementation == implementation as u64 + 1)
        {
            self.services.stats.target_hits += 1;
            case.target as usize
        } else {
            self.services.stats.target_misses += 1;
            let f = *self.program.services.implementations[implementation]
                .methods
                .get(method)
                .ok_or(Invariant::InvalidTraitMethod)?;
            if cache.count < 4 {
                cache.cases[cache.count as usize] = TargetCase {
                    implementation: implementation as u64 + 1,
                    target: f as u64,
                };
                cache.count += 1;
                self.services.stats.max_target_cases =
                    self.services.stats.max_target_cases.max(cache.count);
            }
            f
        };
        let metadata = &self.program.services.implementations[implementation];
        if self.has_pending_lookup()
            && let Err(failure) =
                self.check_lookup_mapper(target, &mut vec![false; self.program.functions.len()])
        {
            self.status = super::Status::Failed(failure);
            if let Some(binding) = lease {
                self.services.bindings[binding].leases -= 1;
                self.release_retired();
            }
            return Ok(());
        }
        let receiver_mode =
            self.program.services.traits[metadata.trait_id].methods[method].receiver;
        let mut args = Vec::with_capacity(regs.len() + 1);
        args.push(if receiver_mode != crate::services::Receiver::Value {
            Value::Address(receiver)
        } else {
            self.read(receiver)?
        });
        args.extend(regs.iter().map(|&r| self.register(r)));
        let depth = self.frames.len();
        let called = self.call(target, args, result);
        if called.is_ok() && self.frames.len() > depth {
            self.frames.last_mut().unwrap().service_lease = lease;
        } else if let Some(b) = lease {
            self.services.bindings[b].leases -= 1;
            self.release_retired();
        }
        called
    }
}
