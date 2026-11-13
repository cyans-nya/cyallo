//! Explicit-root moving cell collector. Native registers and machine stacks are
//! never scanned. The current binding is opt-in while byte accounting matures.
use super::error::{GcError, RuntimeError};
use super::{Cell, Session, Value};
use smallvec::SmallVec;
use std::cell::UnsafeCell;
use std::{
    cell::RefCell,
    ops::{Index, IndexMut},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

/// Registered VM slots have fixed backing storage and explicit interior
/// mutability. Arc prevents owner moves from retagging an underlying Box as
/// unique and invalidating registered pointers. Root pointers are derived from
/// UnsafeCell, not from short-lived exclusive element borrows. Clone still
/// creates distinct slot storage, rather than sharing mutable VM locations.
#[derive(Debug)]
pub(super) struct Slots<T>(Arc<[UnsafeCell<T>]>);
// SAFETY: backing storage is private and Clone copies into distinct storage.
// Only the owning Session edits it; collector aliases require that same exclusive
// Session. Moving ownership preserves addresses and never permits shared edits.
unsafe impl<T: Send> Send for Slots<T> {}
impl<T> From<Vec<T>> for Slots<T> {
    fn from(values: Vec<T>) -> Self {
        Self(values.into_iter().map(UnsafeCell::new).collect())
    }
}
impl<T: Clone> Clone for Slots<T> {
    fn clone(&self) -> Self {
        self.iter().cloned().collect::<Vec<_>>().into()
    }
}
impl<T> Slots<T> {
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.0.iter().map(|v| unsafe { &*v.get() })
    }
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.0.iter().map(|v| unsafe { &mut *v.get() })
    }
    pub fn as_ptr(&self) -> *const T {
        self.0.as_ptr().cast::<T>()
    }
}
impl<T> Index<usize> for Slots<T> {
    type Output = T;
    fn index(&self, i: usize) -> &T {
        unsafe { &*self.0[i].get() }
    }
}
impl<T> IndexMut<usize> for Slots<T> {
    fn index_mut(&mut self, i: usize) -> &mut T {
        unsafe { &mut *self.0[i].get() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GcConfig {
    pub enabled: bool,
    /// Collect before every managed cell allocation, including inside helpers.
    pub stress: bool,
    pub allocation_interval: usize,
}
impl Default for GcConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            stress: false,
            allocation_interval: 4096,
        }
    }
}
impl GcConfig {
    pub fn stress() -> Self {
        Self {
            enabled: true,
            stress: true,
            allocation_interval: 1,
        }
    }
}
#[derive(Debug, Clone, Default)]
pub struct GcStats {
    pub collections: u64,
    pub allocated_cells: u64,
    pub reclaimed_cells: u64,
    pub moved_cells: u64,
    pub occupied_cells: usize,
    pub peak_cells: usize,
    pub arena_capacity: usize,
    pub arena_bytes: usize,
    /// Registry storage at the most recent collection. These byte counters
    /// exclude external Value buffers and are not a total managed-memory cap.
    pub root_bytes: usize,
    pub scratch_bytes: usize,
    pub pause_time: Duration,
}

/// Fixed-size chunks; Option::take moves owners without duplicating/drop hazards.
#[derive(Debug)]
pub(super) struct Arena<T> {
    chunks: Vec<Box<[Option<T>]>>,
    len: usize,
    chunk: usize,
    shift: u32,
}
impl<T> Default for Arena<T> {
    fn default() -> Self {
        let slots = (65536 / size_of::<Option<T>>().max(1)).max(1);
        let chunk = 1usize << slots.ilog2();
        Self {
            chunks: vec![],
            len: 0,
            chunk,
            shift: chunk.ilog2(),
        }
    }
}
impl<T> Arena<T> {
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn capacity(&self) -> usize {
        self.chunks.len() * self.chunk
    }
    pub fn bytes(&self) -> usize {
        self.capacity() * size_of::<Option<T>>()
            + self.chunks.capacity() * size_of::<Box<[Option<T>]>>()
    }
    pub fn push(&mut self, value: T) {
        if self.len == self.capacity() {
            self.chunks
                .push(std::iter::repeat_with(|| None).take(self.chunk).collect());
        }
        self.chunks[self.len >> self.shift][self.len & (self.chunk - 1)] = Some(value);
        self.len += 1;
    }
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.chunks
            .iter()
            .flat_map(|c| c.iter())
            .take(self.len)
            .map(|v| v.as_ref().unwrap())
    }
    pub fn truncate(&mut self, len: usize) {
        while self.len > len {
            self.len -= 1;
            self.chunks[self.len >> self.shift][self.len & (self.chunk - 1)] = None;
        }
        self.chunks.truncate(self.len.div_ceil(self.chunk));
    }
    fn compact(&mut self, forward: &[u32]) {
        let mut count = 0;
        for (old, &new) in forward.iter().enumerate().take(self.len) {
            let value = self.chunks[old >> self.shift][old & (self.chunk - 1)].take();
            if new != u32::MAX {
                debug_assert_eq!(new as usize, count);
                self.chunks[count >> self.shift][count & (self.chunk - 1)] = value;
                count += 1;
            }
        }
        self.len = count;
        self.chunks.truncate(count.div_ceil(self.chunk));
    }
}
impl<T> Index<usize> for Arena<T> {
    type Output = T;
    fn index(&self, i: usize) -> &T {
        assert!(i < self.len);
        self.chunks[i >> self.shift][i & (self.chunk - 1)]
            .as_ref()
            .unwrap()
    }
}
impl<T> IndexMut<usize> for Arena<T> {
    fn index_mut(&mut self, i: usize) -> &mut T {
        assert!(i < self.len);
        self.chunks[i >> self.shift][i & (self.chunk - 1)]
            .as_mut()
            .unwrap()
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct RootId {
    index: u32,
    generation: u32,
}
#[derive(Debug)]
enum RootValue {
    Cell(usize),
    Value(Value),
    CellSlot(*mut usize),
    ValueSlot(*mut Value),
}
// SAFETY: slot pointers target stable Session-owned Slots and are dereferenced
// only through an exclusive Session/NoGcScope. Host RootedCell drops only retire
// registry entries under its mutex; they never dereference these pointers.
unsafe impl Send for RootValue {}
#[derive(Debug)]
struct Slot {
    generation: u32,
    active: bool,
    value: Option<RootValue>,
}
#[derive(Debug, Default)]
struct Roots {
    slots: Vec<Slot>,
    free: Vec<u32>,
}
impl Roots {
    fn get(&self, id: RootId) -> &Slot {
        let slot = &self.slots[id.index as usize];
        assert_eq!(slot.generation, id.generation, "stale root handle");
        assert!(slot.value.is_some(), "released root handle");
        slot
    }
    fn get_mut(&mut self, id: RootId) -> &mut Slot {
        self.get(id);
        &mut self.slots[id.index as usize]
    }
}
/// Locks a root registry, recovering it if an earlier holder panicked.
///
/// Handle validation (`Roots::get`) and root-kind checks panic before the
/// registry is mutated, so a poisoned registry is still structurally valid.
/// Recovery is required for teardown: `RootScope::drop` runs while unwinding
/// from such a panic, and a second panic there would abort the process instead
/// of letting `catch_unwind` at the native step boundary fail the Session.
fn lock(registry: &Mutex<Roots>) -> MutexGuard<'_, Roots> {
    registry.lock().unwrap_or_else(PoisonError::into_inner)
}
#[derive(Debug)]
pub(super) struct RootScope {
    registry: Arc<Mutex<Roots>>,
    ids: RefCell<SmallVec<[RootId; 8]>>,
}
impl RootScope {
    fn insert(&self, value: RootValue) -> RootId {
        let mut roots = lock(&self.registry);
        let id = if let Some(index) = roots.free.pop() {
            let slot = &mut roots.slots[index as usize];
            slot.value = Some(value);
            slot.active = true;
            RootId {
                index,
                generation: slot.generation,
            }
        } else {
            let index = u32::try_from(roots.slots.len()).expect("root capacity");
            // Dropping a root scope never grows the free list.
            let capacity = roots.slots.len() + 1;
            let spare = capacity.saturating_sub(roots.free.len());
            roots.free.reserve(spare);
            roots.slots.push(Slot {
                generation: 0,
                active: true,
                value: Some(value),
            });
            RootId {
                index,
                generation: 0,
            }
        };
        self.ids.borrow_mut().push(id);
        id
    }
    /// SAFETY: the pointer remains live and exclusive to GC edits until this scope drops.
    pub unsafe fn external_cell(&self, pointer: *mut usize) -> RootId {
        self.insert(RootValue::CellSlot(pointer))
    }
    pub fn cell(&self, id: usize) -> RootId {
        self.insert(RootValue::Cell(id))
    }
    pub fn value(&self, value: Value) -> RootId {
        self.insert(RootValue::Value(value))
    }
    pub fn cell_id(&self, id: RootId) -> usize {
        let roots = lock(&self.registry);
        let slot = roots.get(id);
        match slot.value.as_ref().unwrap() {
            RootValue::Cell(id) => *id,
            RootValue::CellSlot(pointer) => unsafe { **pointer },
            _ => panic!("cell root required"),
        }
    }
    pub fn value_copy(&self, id: RootId) -> Value {
        let roots = lock(&self.registry);
        let slot = roots.get(id);
        match slot.value.as_ref().unwrap() {
            RootValue::Value(value) => value.clone(),
            _ => panic!("value root required"),
        }
    }
    pub(super) fn edit_value(&self, id: RootId, edit: impl FnOnce(&mut Value)) {
        let mut roots = lock(&self.registry);
        let Some(RootValue::Value(value)) = roots.get_mut(id).value.as_mut() else {
            panic!("value root required")
        };
        edit(value);
    }
    pub fn array_len(&self, id: RootId) -> Option<usize> {
        let roots = lock(&self.registry);
        match roots.get(id).value.as_ref().unwrap() {
            RootValue::Value(Value::ArraySnapshot(v)) => Some(v.len()),
            _ => None,
        }
    }
    pub fn array_at(&self, id: RootId, index: usize) -> Value {
        let roots = lock(&self.registry);
        match roots.get(id).value.as_ref().unwrap() {
            RootValue::Value(Value::ArraySnapshot(v)) => v[index].clone(),
            _ => unreachable!(),
        }
    }
    pub fn sparse_keys(&self, id: RootId) -> Option<(Vec<u32>, u64)> {
        let roots = lock(&self.registry);
        match roots.get(id).value.as_ref().unwrap() {
            RootValue::Value(Value::SparseSnapshot(v, len)) => {
                Some((v.keys().copied().collect(), *len))
            }
            _ => None,
        }
    }
    pub fn sparse_at(&self, id: RootId, index: u32) -> Value {
        let roots = lock(&self.registry);
        match roots.get(id).value.as_ref().unwrap() {
            RootValue::Value(Value::SparseSnapshot(v, _)) => v[&index].clone(),
            _ => unreachable!(),
        }
    }
    pub fn insert_sparse(&self, id: RootId, index: u32, cell: usize) {
        let mut roots = lock(&self.registry);
        match roots.get_mut(id).value.as_mut().unwrap() {
            RootValue::Value(Value::SparseArray(v, _)) => {
                v.insert(index, cell);
            }
            _ => unreachable!(),
        }
    }
    pub fn take_value(&self, id: RootId) -> Value {
        let mut roots = lock(&self.registry);
        match roots.get_mut(id).value.as_mut().unwrap() {
            RootValue::Value(v) => std::mem::replace(v, Value::Uninitialized),
            _ => unreachable!(),
        }
    }
    /// SAFETY: caller keeps this heap-owned slice's backing storage fixed until
    /// the scope is dropped, and collection has exclusive access to its values.
    unsafe fn cells(&self, values: &Slots<usize>) {
        for value in values.0.iter() {
            self.insert(RootValue::CellSlot(value.get()));
        }
    }
    fn active(&self, id: RootId, active: bool) {
        let mut roots = lock(&self.registry);
        let slot = roots.get_mut(id);
        slot.active = active;
    }
}
impl Drop for RootScope {
    fn drop(&mut self) {
        let mut roots = lock(&self.registry);
        for id in self.ids.get_mut().drain(..) {
            let slot = roots.get_mut(id);
            slot.value = None;
            slot.active = false;
            if let Some(generation) = slot.generation.checked_add(1) {
                slot.generation = generation;
                roots.free.push(id.index);
            }
        }
    }
}
#[derive(Debug)]
pub(super) struct FrameRoots {
    scope: RootScope,
    registers: Vec<(usize, RootId)>,
}
impl FrameRoots {
    pub fn publish(&self, live: &[u64]) {
        for &(r, root) in &self.registers {
            self.scope.active(
                root,
                live.get(r / 64)
                    .is_some_and(|word| word & (1u64 << (r % 64)) != 0),
            );
        }
    }
}
#[derive(Debug, Default)]
pub(super) struct Collector {
    pending_owners: Vec<Value>,
    roots: Arc<Mutex<Roots>>,
    marked: Vec<u64>,
    forward: Vec<u32>,
    work: Vec<usize>,
    next: usize,
    pub stats: GcStats,
}
impl Collector {
    pub fn scope(&self) -> RootScope {
        RootScope {
            registry: self.roots.clone(),
            ids: RefCell::new(SmallVec::new()),
        }
    }
    pub fn reserve(&mut self, len: usize) {
        self.marked.resize(len.div_ceil(64), 0);
        self.forward.resize(len, u32::MAX);
        self.work.reserve(len.saturating_sub(self.work.len()));
    }
    pub fn due(&self, len: usize, config: GcConfig) -> bool {
        config.enabled && (config.stress || len >= self.next.max(config.allocation_interval.max(1)))
    }
    pub unsafe fn frame(
        &self,
        locals: &Slots<usize>,
        registers: &Slots<Value>,
        kinds: &[crate::gc_metadata::ReferenceKind],
    ) -> Option<FrameRoots> {
        if locals.0.is_empty()
            && kinds
                .iter()
                .all(|kind| *kind == crate::gc_metadata::ReferenceKind::Scalar)
        {
            return None;
        }
        let scope = self.scope();
        unsafe { scope.cells(locals) };
        let registers = registers
            .0
            .iter()
            .enumerate()
            .filter(|(r, _)| kinds[*r] != crate::gc_metadata::ReferenceKind::Scalar)
            .map(|(r, v)| (r, scope.insert(RootValue::ValueSlot(v.get()))))
            .collect();
        Some(FrameRoots { scope, registers })
    }
    fn collect(&mut self, heap: &mut Arena<Cell>, interval: usize) -> Result<(), RuntimeError> {
        let before = heap.len();
        debug_assert!(self.forward.len() >= before && self.work.capacity() >= before);
        self.marked.fill(0);
        self.work.clear();
        let roots = lock(&self.roots);
        let marked = &mut self.marked;
        let work = &mut self.work;
        let mut visit = |id: usize| -> Result<(), RuntimeError> {
            if id >= before {
                return Err(GcError::DanglingReference { edge: false }.into());
            }
            let mask = 1u64 << (id % 64);
            if marked[id / 64] & mask == 0 {
                marked[id / 64] |= mask;
                work.push(id);
            }
            Ok(())
        };
        for slot in roots.slots.iter().filter(|s| s.active) {
            if let Some(value) = &slot.value {
                match value {
                    RootValue::Cell(id) => visit(*id)?,
                    RootValue::Value(v) => trace(v, &mut visit)?,
                    // SAFETY: only explicitly registered fixed, owned slots are read.
                    RootValue::CellSlot(ptr) => visit(unsafe { **ptr })?,
                    RootValue::ValueSlot(ptr) => trace(unsafe { &**ptr }, &mut visit)?,
                }
            }
        }
        while let Some(id) = self.work.pop() {
            let marked = &mut self.marked;
            let work = &mut self.work;
            trace(&heap[id].value, &mut |id| {
                if id >= before {
                    return Err(GcError::DanglingReference { edge: true }.into());
                }
                let mask = 1u64 << (id % 64);
                if marked[id / 64] & mask == 0 {
                    marked[id / 64] |= mask;
                    work.push(id);
                }
                Ok(())
            })?;
        }
        drop(roots);
        let mut count = 0;
        let mut moved = 0;
        for old in 0..before {
            if self.marked[old / 64] & (1u64 << (old % 64)) != 0 {
                self.forward[old] = count;
                if old != count as usize {
                    moved += 1;
                }
                count += 1;
            } else {
                self.forward[old] = u32::MAX;
            }
        }
        let rewrite = |id: &mut usize| {
            let new = self.forward[*id];
            assert_ne!(new, u32::MAX, "live reference must have been marked");
            *id = new as usize;
        };
        for slot in lock(&self.roots).slots.iter_mut().filter(|s| s.active) {
            if let Some(value) = &mut slot.value {
                match value {
                    RootValue::Cell(id) => rewrite(id),
                    RootValue::Value(v) => relocate(v, &rewrite),
                    // SAFETY: registered owners are stopped and uniquely borrowed.
                    RootValue::CellSlot(ptr) => rewrite(unsafe { &mut **ptr }),
                    RootValue::ValueSlot(ptr) => relocate(unsafe { &mut **ptr }, &rewrite),
                }
            }
        }
        // Dead register slots must not keep old indices that can later be reused.
        for slot in lock(&self.roots).slots.iter_mut().filter(|s| !s.active) {
            if let Some(RootValue::ValueSlot(ptr)) = &mut slot.value {
                unsafe {
                    **ptr = Value::Uninitialized;
                }
            }
        }
        for old in 0..before {
            if self.forward[old] != u32::MAX {
                relocate(&mut heap[old].value, &rewrite);
            } else {
                fn owners(value: &Value, pending: &mut Vec<Value>) {
                    match value {
                        Value::Concurrent(_)
                        | Value::Rc(_)
                        | Value::Weak(_)
                        | Value::Allocation(_)
                        | Value::Generator(_) => pending.push(value.clone()),
                        Value::TaskOutcome(cyallo_concurrent_values::TaskOutcome::Completed(
                            value,
                        )) => owners(value, pending),
                        Value::SendError(error) => owners(&error.value, pending),
                        Value::ArraySnapshot(values) | Value::Function(_, values) => {
                            for value in values {
                                owners(value, pending);
                            }
                        }
                        Value::SparseSnapshot(values, _) => {
                            for value in values.values() {
                                owners(value, pending);
                            }
                        }
                        Value::GeneratorStep(
                            cyallo_values::GeneratorStep::Yielded(value)
                            | cyallo_values::GeneratorStep::Complete(value),
                        )
                        | Value::Cell(value)
                        | Value::Optional(Some(value))
                        | Value::Result(Ok(value) | Err(value))
                        | Value::Erased(_, value)
                        | Value::StoragePath(value, _) => owners(value, pending),
                        _ => {}
                    }
                }
                owners(&heap[old].value, &mut self.pending_owners);
            }
        }
        heap.compact(&self.forward[..before]);
        self.stats.collections += 1;
        self.stats.reclaimed_cells += (before - heap.len()) as u64;
        self.stats.moved_cells += moved;
        self.stats.occupied_cells = heap.len();
        self.stats.arena_capacity = heap.capacity();
        self.stats.arena_bytes = heap.bytes();
        let roots = lock(&self.roots);
        self.stats.root_bytes =
            roots.slots.capacity() * size_of::<Slot>() + roots.free.capacity() * size_of::<u32>();
        self.stats.scratch_bytes = self.marked.capacity() * 8
            + self.forward.capacity() * 4
            + self.work.capacity() * size_of::<usize>();
        self.next = heap.len().saturating_add(interval.max(1));
        Ok(())
    }
}
fn trace(
    value: &Value,
    visit: &mut impl FnMut(usize) -> Result<(), RuntimeError>,
) -> Result<(), RuntimeError> {
    match value {
        // Task results and failed sends retain values that may hold heap cells.
        Value::TaskOutcome(cyallo_concurrent_values::TaskOutcome::Completed(v)) => trace(v, visit)?,
        Value::SendError(error) => trace(&error.value, visit)?,
        Value::Managed(id) | Value::Address(id) | Value::TraitObject(id, _) => visit(*id)?,
        Value::Slice { owner, .. } => visit(*owner)?,
        Value::GeneratorStep(
            cyallo_values::GeneratorStep::Yielded(v) | cyallo_values::GeneratorStep::Complete(v),
        )
        | Value::Cell(v)
        | Value::Optional(Some(v))
        | Value::Result(Ok(v) | Err(v))
        | Value::Erased(_, v)
        | Value::StoragePath(v, _) => trace(v, visit)?,
        Value::Array(ids) => {
            for &id in ids {
                visit(id)?
            }
        }
        Value::Deque(ids) => {
            for &id in ids {
                visit(id)?;
            }
        }
        Value::SparseArray(ids, _) => {
            for &id in ids.values() {
                visit(id)?
            }
        }
        Value::ArraySnapshot(values) | Value::Function(_, values) => {
            for value in values {
                trace(value, visit)?
            }
        }
        Value::SparseSnapshot(values, _) => {
            for value in values.values() {
                trace(value, visit)?
            }
        }
        _ => {}
    }
    Ok(())
}
fn relocate(value: &mut Value, rewrite: &impl Fn(&mut usize)) {
    match value {
        Value::TaskOutcome(cyallo_concurrent_values::TaskOutcome::Completed(v)) => {
            relocate(v, rewrite)
        }
        Value::SendError(error) => relocate(&mut error.value, rewrite),
        Value::Managed(id) | Value::Address(id) | Value::TraitObject(id, _) => rewrite(id),
        Value::Slice { owner, .. } => rewrite(owner),
        Value::GeneratorStep(
            cyallo_values::GeneratorStep::Yielded(v) | cyallo_values::GeneratorStep::Complete(v),
        )
        | Value::Cell(v)
        | Value::Optional(Some(v))
        | Value::Result(Ok(v) | Err(v))
        | Value::Erased(_, v)
        | Value::StoragePath(v, _) => relocate(v, rewrite),
        Value::Array(ids) => {
            for id in ids {
                rewrite(id)
            }
        }
        Value::Deque(ids) => {
            for id in ids {
                rewrite(id);
            }
        }
        Value::SparseArray(ids, _) => {
            for id in ids.values_mut() {
                rewrite(id)
            }
        }
        Value::ArraySnapshot(values) | Value::Function(_, values) => {
            for value in values {
                relocate(value, rewrite)
            }
        }
        Value::SparseSnapshot(values, _) => {
            for value in values.values_mut() {
                relocate(value, rewrite)
            }
        }
        _ => {}
    }
}

impl Session {
    pub fn gc_config(&self) -> GcConfig {
        self.gc_config
    }
    pub fn gc_stats(&self) -> &GcStats {
        &self.collector.stats
    }
    pub fn heap_cell_count(&self) -> usize {
        self.heap.len()
    }
    pub fn gc_scope(&mut self) -> GcScope<'_> {
        GcScope { session: self }
    }
    pub(super) fn root_scope(&self) -> RootScope {
        self.collector.scope()
    }
    pub(super) fn register_globals(&mut self) {
        self.global_roots = None;
        if self.gc_config.enabled {
            let scope = self.root_scope();
            unsafe { scope.cells(&self.globals) };
            self.global_roots = Some(scope);
        }
    }
    /// Collect at an explicitly stopped VM boundary, without executing bytecode.
    pub fn collect_garbage(&mut self) -> Result<(), RuntimeError> {
        self.publish_generator_roots();
        if !self.gc_config.enabled {
            return Err(GcError::Disabled.into());
        }
        debug_assert_eq!(self.native_view.pending, 0);
        let started = Instant::now();
        let mut live = Vec::new();
        for (index, frame) in self.frames.iter().enumerate() {
            let pc = self
                .gc_instruction
                .filter(|(active, _)| *active == index)
                .map_or(frame.pc, |(_, pc)| pc);
            if let Some(roots) = &frame.roots {
                let function = &self.program.functions[frame.function];
                function.resume.live_at(function, pc, &mut live);
                roots.publish(&live);
            }
        }
        let result = self
            .collector
            .collect(&mut self.heap, self.gc_config.allocation_interval);
        if result.is_ok() {
            self.refresh_lookups_after_gc();
            let pending = std::mem::take(&mut self.collector.pending_owners);
            for owner in pending {
                self.drop_value(owner)?;
            }
        }
        if result.is_ok() && self.native_enabled {
            self.rebuild_native_after_gc();
        }
        self.collector.stats.pause_time += started.elapsed();
        result
    }
    pub(super) fn maybe_collect(&mut self) -> Result<(), RuntimeError> {
        if self.gc_config.enabled
            && (self.heap.len() >= self.context.max_cells
                || self.collector.due(self.heap.len(), self.gc_config))
        {
            self.collect_garbage()?;
        }
        Ok(())
    }
}

/// An owned host root in a single Session. The slot is released on drop.
/// Object indices are intentionally private; use a non-collecting borrow.
pub struct RootedCell {
    scope: RootScope,
    id: RootId,
}
/// Exclusive capability for a Session's potentially collecting heap operations.
pub struct GcScope<'session> {
    session: &'session mut Session,
}
/// A borrowed view that excludes collection while managed borrows remain usable.
///
/// ```compile_fail
/// use cyallo::{compile,Session};
/// use std::sync::Arc;
/// let mut session=Session::new(Arc::new(compile("").unwrap())).unwrap();
/// let mut gc=session.gc_scope();
/// let root=gc.allocate_integer(7).unwrap();
/// let borrowed=gc.no_gc();
/// let value=borrowed.get(&root).unwrap();
/// gc.collect().unwrap(); // cannot collect while the borrowed value is live
/// println!("{value}");
/// ```
pub struct NoGcScope<'scope> {
    session: &'scope mut Session,
}
impl GcScope<'_> {
    pub fn no_gc(&mut self) -> NoGcScope<'_> {
        NoGcScope {
            session: self.session,
        }
    }
    pub fn collect(&mut self) -> Result<(), RuntimeError> {
        self.session.collect_garbage()
    }
    /// Initial host allocation API; complex object constructors remain internal
    /// until their typed public contracts are implemented.
    pub fn allocate_integer(&mut self, value: i64) -> Result<RootedCell, RuntimeError> {
        let scope = self.session.root_scope();
        let root = scope.value(Value::Uninitialized);
        let id = self.session.alloc(&crate::Type::Integer)?;
        lock(&scope.registry).get_mut(root).value = Some(RootValue::Cell(id));
        self.session.store(id, Value::Integer(value))?;
        Ok(RootedCell { scope, id: root })
    }
}
impl NoGcScope<'_> {
    pub fn get(&self, root: &RootedCell) -> Result<&Value, RuntimeError> {
        if !Arc::ptr_eq(&root.scope.registry, &self.session.collector.roots) {
            return Err(GcError::ForeignRoot.into());
        }
        let id = root.scope.cell_id(root.id);
        Ok(&self.session.heap[id].value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Type;
    use std::rc::Rc;

    fn cell(value: Value) -> Cell {
        Cell {
            identity: 0,
            version: 0,
            parent: None,
            ty: Type::Integer,
            value,
        }
    }
    #[test]
    fn moves_cycles_and_reclaims_unreachable_cycles() {
        let mut heap = Arena::default();
        for value in [
            Value::Address(1),
            Value::Address(0),
            Value::Integer(9),
            Value::Address(4),
            Value::Address(3),
        ] {
            heap.push(cell(value));
        }
        let mut gc = Collector::default();
        gc.reserve(heap.len());
        let roots = gc.scope();
        let first = roots.cell(3);
        let alias = roots.cell(3);
        gc.collect(&mut heap, 1).unwrap();
        assert_eq!(heap.len(), 2);
        assert_eq!(roots.cell_id(first), 0);
        assert_eq!(roots.cell_id(alias), 0);
        assert_eq!(heap[0].value, Value::Address(1));
        assert_eq!(heap[1].value, Value::Address(0));
        assert_eq!(gc.stats.reclaimed_cells, 3);
        assert_eq!(gc.stats.moved_cells, 2);
        drop(roots);
        gc.collect(&mut heap, 1).unwrap();
        assert_eq!(heap.len(), 0);
        assert_eq!(heap.capacity(), 0);
    }
    #[test]
    fn task_results_and_failed_sends_keep_their_cells_alive() {
        let mut heap = Arena::default();
        heap.push(cell(Value::Integer(0)));
        heap.push(cell(Value::Integer(7)));
        heap.push(cell(Value::Integer(8)));
        let mut gc = Collector::default();
        gc.reserve(heap.len());
        let roots = gc.scope();
        let completed = roots.value(Value::TaskOutcome(
            cyallo_concurrent_values::TaskOutcome::Completed(Box::new(Value::Managed(1))),
        ));
        let unsent = roots.value(Value::SendError(cyallo_concurrent_values::SendError {
            kind: cyallo_concurrent_values::SendFailure::Closed,
            value: Box::new(Value::Managed(2)),
        }));
        gc.collect(&mut heap, 1).unwrap();
        // Cell 0 is unreachable; the two referenced cells survive and are renumbered.
        assert_eq!(heap.len(), 2);
        assert_eq!(heap[0].value, Value::Integer(7));
        assert_eq!(
            roots.value_copy(completed),
            Value::TaskOutcome(cyallo_concurrent_values::TaskOutcome::Completed(Box::new(
                Value::Managed(0)
            )))
        );
        let Value::SendError(error) = roots.value_copy(unsent) else {
            panic!("expected a send error")
        };
        assert_eq!(*error.value, Value::Managed(1));
    }
    #[test]
    fn invalid_edge_fails_before_rewriting_roots_or_heap() {
        let mut heap = Arena::default();
        heap.push(cell(Value::Integer(0)));
        heap.push(cell(Value::Address(99)));
        let mut gc = Collector::default();
        gc.reserve(heap.len());
        let roots = gc.scope();
        let root = roots.cell(1);
        assert!(gc.collect(&mut heap, 1).is_err());
        assert_eq!(roots.cell_id(root), 1);
        assert_eq!(heap.len(), 2);
        assert_eq!(heap[1].value, Value::Address(99));
        assert_eq!(gc.stats.collections, 0);
    }
    #[test]
    #[should_panic(expected = "stale root handle")]
    fn stale_root_generation_is_rejected_after_slot_reuse() {
        let gc = Collector::default();
        let old = gc.scope();
        let stale = old.cell(0);
        drop(old);
        let next = gc.scope();
        next.cell(0);
        next.cell_id(stale);
    }
    #[test]
    fn registry_survives_a_panic_while_locked() {
        let mut heap = Arena::default();
        heap.push(cell(Value::Integer(1)));
        heap.push(cell(Value::Integer(2)));
        let mut gc = Collector::default();
        gc.reserve(heap.len());
        let old = gc.scope();
        let stale = old.cell(0);
        drop(old);
        let live = gc.scope();
        let root = live.cell(1);
        // A rejected handle panics while holding the registry lock, poisoning it.
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            live.cell_id(stale);
        }));
        assert!(caught.is_err());
        assert!(gc.roots.is_poisoned());
        // Later roots, collection and teardown still work instead of aborting.
        let extra = live.cell(1);
        gc.collect(&mut heap, 1).unwrap();
        assert_eq!(heap.len(), 1);
        assert_eq!(live.cell_id(root), 0);
        assert_eq!(live.cell_id(extra), 0);
        drop(live);
        gc.collect(&mut heap, 1).unwrap();
        assert_eq!(heap.len(), 0);
    }
    #[test]
    fn compaction_drops_each_owner_once_and_releases_chunks() {
        struct Owner(Rc<std::cell::Cell<usize>>);
        impl Drop for Owner {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }
        let drops = Rc::new(std::cell::Cell::new(0));
        let mut arena = Arena::default();
        let len = arena.chunk * 3 + 1;
        for _ in 0..len {
            arena.push(Owner(drops.clone()));
        }
        let mut forward = vec![u32::MAX; len];
        forward[len - 1] = 0;
        arena.compact(&forward);
        assert_eq!(drops.get(), len - 1);
        assert_eq!(arena.chunks.len(), 1);
        arena.truncate(0);
        assert_eq!(drops.get(), len);
        assert_eq!(arena.capacity(), 0);
        arena.push(Owner(drops.clone()));
        assert_eq!(arena.len(), 1);
        drop(arena);
        assert_eq!(drops.get(), len + 1);
    }

    #[test]
    fn registered_frame_slots_and_native_mirrors_survive_repeated_movement() {
        // No JIT machine code is executed here, so Miri can check the raw root
        // pointers, their fixed UnsafeCell backing, and native mirror ownership.
        let program = std::sync::Arc::new(
            crate::compile(
                r#"
let mut saved = std::collections::Vec::from(0..0)
proc work(n: &mut i64) {
    let incremented = *n + 1
    let a = [*n, incremented]
    saved.push(a[0])
    *n = incremented
}
let mut n = 0
for i in 0..=4 { work(&mut n) }
std::runtime::caie::input(&mut n)
"#,
            )
            .unwrap(),
        );
        let mut session = Session::with_context(
            program,
            crate::Context {
                gc: GcConfig::stress(),
                ..Default::default()
            },
        )
        .unwrap();
        session.prepare_native();
        for _ in 0..1000 {
            session.step();
            session.collect_garbage().unwrap();
            session.prepare_native();
            if session.status() == &crate::Status::WaitingForInput {
                break;
            }
        }
        assert_eq!(session.status(), &crate::Status::WaitingForInput);
        assert!(
            session
                .globals()
                .iter()
                .any(|(name, value)| name == "n" && value == "5")
        );
        assert!(session.gc_stats().moved_cells > 0);
    }
}
