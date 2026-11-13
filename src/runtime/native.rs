//! Private native ABI. Only this module and generated code access these layouts.
//!
//! Scalar slots are shared by native tiers. Owned Values are materialized at every
//! safepoint, before entering Rust or yielding to another engine. Cell indices
//! relocate during GC; mirrors are rebuilt and views reloaded at that boundary.
use super::{Session, Status, Value};
use crate::metrics::ExecutionEngine;
use std::cell::UnsafeCell;
use std::sync::atomic::AtomicU64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub(crate) enum ExitReason {
    Return = 0,
    Budget = 1,
    CodeReady = 2,
    Osr = 3,
    Hot = 4,
    Error = 5,
    Continuation = 6,
}

pub(crate) const DIRTY: u64 = 256;
pub(crate) const INTEGER: u64 = 1;
pub(crate) const REAL: u64 = 2;
pub(crate) const BOOLEAN: u64 = 3;
pub(crate) const CHAR: u64 = 4;
pub(crate) const ADDRESS: u64 = 5;
pub(crate) const CONTEXT: u64 = 7;
pub(crate) const SERVICE_KEY: u64 = 8;
pub(crate) const SERVICE_REF: u64 = 9;
pub(crate) const REGISTRATION: u64 = 10;
pub(crate) const TRAIT_OBJECT: u64 = 11;
const HANDLE: u64 = 6;

#[derive(Debug, Default, Clone, Copy)]
#[repr(C)]
pub(crate) struct NativeSlot {
    pub bits: u64,
    pub kind: u64,
}
impl NativeSlot {
    pub(super) fn from_value(value: &Value) -> Self {
        let (kind, bits) = match value {
            Value::Integer(n) => (INTEGER, *n as u64),
            Value::Real(n) => (REAL, n.to_bits()),
            Value::Boolean(b) => (BOOLEAN, u64::from(*b)),
            Value::Char(c) => (CHAR, *c as u64),
            Value::Address(id) => (ADDRESS, *id as u64),
            Value::Context(id) => (CONTEXT, *id as u64),
            Value::ServiceKey(id) => (SERVICE_KEY, *id as u64),
            Value::Registration(id) => (REGISTRATION, *id as u64),
            Value::ServiceRef(view, key) => (SERVICE_REF, ((*view as u64) << 32) | *key as u64),
            Value::TraitObject(receiver, implementation) => (
                TRAIT_OBJECT,
                ((*implementation as u64) << 32) | *receiver as u64,
            ),
            Value::Uninitialized => (0, 0),
            _ => (HANDLE, 0),
        };
        Self { bits, kind }
    }
    fn materialize(self) -> Value {
        match self.kind & 255 {
            INTEGER => Value::Integer(self.bits as i64),
            REAL => Value::Real(f64::from_bits(self.bits)),
            BOOLEAN => Value::Boolean(self.bits != 0),
            CHAR => Value::Char(char::from_u32(self.bits as u32).expect("validated CHAR")),
            ADDRESS => Value::Address(self.bits as usize),
            CONTEXT => Value::Context(self.bits as usize),
            SERVICE_KEY => Value::ServiceKey(self.bits as usize),
            REGISTRATION => Value::Registration(self.bits as usize),
            SERVICE_REF => {
                Value::ServiceRef((self.bits >> 32) as usize, (self.bits as u32) as usize)
            }
            TRAIT_OBJECT => {
                Value::TraitObject((self.bits as u32) as usize, (self.bits >> 32) as usize)
            }
            _ => unreachable!("only native scalar writes may mark a slot dirty"),
        }
    }
}

const CHUNK: usize = 256;
#[derive(Default)]
pub(super) struct NativeStorage {
    chunks: Vec<Box<UnsafeCell<[NativeSlot; CHUNK]>>>,
    pub pointers: Vec<*mut NativeSlot>,
}
// SAFETY: chunks own all pointees. Access is private and requires the exclusive
// Session borrow; generated code runs synchronously under that same borrow.
unsafe impl Send for NativeStorage {}
impl NativeStorage {
    pub fn push(&mut self) {
        let id = self.pointers.len();
        if id / CHUNK == self.chunks.len() {
            self.chunks
                .push(Box::new(UnsafeCell::new([NativeSlot::default(); CHUNK])));
        }
        // SAFETY: the chunk owns CHUNK elements and id % CHUNK is in bounds.
        // UnsafeCell permits mutation without reborrowing the entire chunk and
        // invalidating raw pointers to its other, already published cells.
        let slot = unsafe {
            self.chunks[id / CHUNK]
                .get()
                .cast::<NativeSlot>()
                .add(id % CHUNK)
        };
        unsafe {
            *slot = NativeSlot::default();
        }
        self.pointers.push(slot);
    }
    pub fn set(&mut self, id: usize, slot: NativeSlot) {
        // SAFETY: only allocated IDs are used; &mut self excludes native access.
        unsafe {
            *self.pointers[id] = slot;
        }
    }
    fn get_mut(&mut self, id: usize) -> &mut NativeSlot {
        // SAFETY: same ownership/bounds invariant as set. The result cannot
        // outlive this exclusive storage borrow.
        unsafe { &mut *self.pointers[id] }
    }
    pub fn truncate(&mut self, len: usize) {
        self.pointers.truncate(len);
        self.chunks.truncate(self.pointers.len().div_ceil(CHUNK));
    }
}

#[derive(Default)]
#[repr(C)]
pub(crate) struct NativeView {
    pub service_tables: *const *const super::services::NativeBinding,
    pub call_caches: *mut super::services::CallCache,
    pub trait_hint_target: u64,
    pub registers: *mut NativeSlot,
    pub locals: *const usize,
    pub globals: *const usize,
    pub cells: *const *mut NativeSlot,
    pub pc: *mut usize,
    pub function: u64,
    pub depth: u64,
    pub halted: u64,
    pub pending: u64,
    pub error: u64,
    pub dirty_ids: *mut usize,
    pub dirty_count: u64,
    pub entries: *const usize,
    pub native_depth: u64,
    pub engine: u64,
    pub reporting: u64,
    pub control_epoch: *const AtomicU64,
    pub observed_epoch: u64,
    pub transition_limits: *const u64,
    pub transition_remaining: u64,
    pub osr_targets: *const u8,
    pub osr_requested: u64,
    pub poll_interval: u64,
    pub next_poll: u64,
    pub exit_reason: u64,
    pub code_version: u64,
    // Keep the baseline fields within Winch's compact addressing range. These
    // trailing extensions are accessed only by Cranelift's general loads.
    pub inlined_pending: u64,
    pub inlined_calls: u64,
    pub function_work: *mut u64,
    pub function_calls: *mut u64,
    pub max_depth: u64,
    pub body_inline_calls: u64,
    pub inline_exit_reason: u64,
    pub inline_exit_function: u64,
    pub inline_exit_pc: u64,
    pub inline_exit_target: u64,
}
// SAFETY: these are borrowed pointers refreshed before entry, never dereferenced
// by Rust outside the exclusive Session invocation. The boxed view has one owner.
unsafe impl Send for NativeView {}

impl Session {
    pub(crate) fn enter_native_reporting(&mut self) {
        self.execution_engine = ExecutionEngine::from_native(self.native_view.engine);
        self.enter_reporting();
    }
    pub(crate) fn prepare_native(&mut self) -> *mut NativeView {
        self.prepare_service_native();
        let start = self.report.as_ref().map(|_| std::time::Instant::now());
        if !self.native_enabled {
            // Interpreter and Winch executions pay no scalar mirror allocation.
            // Include suspended callers at first OSR entry, since native callees
            // can return into any of them.
            for cell in self.heap.iter() {
                let id = self.native_cells.pointers.len();
                self.native_cells.push();
                self.native_cells
                    .set(id, NativeSlot::from_value(&cell.value));
            }
            for frame in &mut self.frames {
                frame.native_registers =
                    frame.registers.iter().map(NativeSlot::from_value).collect();
            }
            self.native_enabled = true;
        }
        debug_assert_eq!(self.native_view.pending, 0);
        debug_assert_eq!(self.native_view.inlined_pending, 0);
        debug_assert_eq!(self.native_view.dirty_count, 0);
        // Each cell is enqueued at most once between safepoints. Reserve actual
        // initialized elements so generated writes are within the Vec's length.
        self.native_dirty.resize(self.heap.len(), 0);
        let view = &mut self.native_view;
        view.service_tables = self.services.pointers.as_ptr();
        view.call_caches = self
            .frames
            .last()
            .and_then(|f| self.services.caches.get_mut(f.function))
            .map_or(std::ptr::null_mut(), |c| c.as_mut_ptr());
        view.halted = u64::from(
            matches!(self.status, Status::Finished | Status::Failed(_))
                || (self.status == Status::WaitingForInput && self.context.input.is_empty()),
        );
        if view.halted == 0 {
            self.status = Status::Paused;
        }
        view.depth = self.frames.len() as u64;
        view.max_depth = self.context.max_frames as u64;
        view.function_work = self.function_work.as_mut_ptr();
        view.function_calls = self.function_calls.as_mut_ptr();
        // Eager artifacts have no compiler mailbox. Disable readiness polling
        // rather than repeatedly checking a null epoch (the unbound interval
        // may be zero). Transition/OSR checks remain independent of this clock.
        view.next_poll = if view.control_epoch.is_null() {
            u64::MAX
        } else {
            0
        };
        if let Some(frame) = self.frames.last_mut() {
            view.registers = frame.native_registers.as_mut_ptr();
            view.locals = frame.locals.as_ptr();
            view.pc = &mut frame.pc;
            view.function = frame.function as u64;
            // SAFETY: bind_native_control pins program-sized tables for the
            // exclusive run. Generated frame IDs belong to that program.
            view.transition_remaining = if view.transition_limits.is_null() {
                u64::MAX
            } else {
                unsafe { *view.transition_limits.add(frame.function) }
                    .saturating_sub(self.function_work[frame.function])
            };
            view.osr_requested = if view.osr_targets.is_null() {
                0
            } else {
                u64::from(unsafe { *view.osr_targets.add(frame.function) } != 0)
            };
        } else {
            view.halted = 1;
            view.pc = std::ptr::null_mut();
        }
        view.globals = self.globals.as_ptr();
        view.cells = self.native_cells.pointers.as_ptr();
        view.dirty_ids = self.native_dirty.as_mut_ptr();
        if let (Some(start), Some(report)) = (start, &mut self.report) {
            report.native_prepare_time += start.elapsed();
        }
        &mut **view
    }

    pub(crate) fn finish_native(&mut self) {
        let reason = std::mem::take(&mut self.native_view.inline_exit_reason);
        if reason != 0 {
            let caller = self.native_view.inline_exit_function as usize;
            let target = self.native_view.inline_exit_target as usize;
            let pc = self.native_view.inline_exit_pc;
            let cause = match reason {
                1 => "budget",
                2 => "full tracing",
                3 => "logical depth",
                4 => "argument tag",
                5 => "callee promotion",
                _ => "checked arithmetic",
            };
            self.program.notice(
                "fallback",
                caller,
                format!(
                    "pc {pc}: {} inline guard failed ({cause}); ordinary call, arguments preserved",
                    self.program.functions[target].name
                ),
            );
        }
        let start = self.report.as_ref().map(|_| std::time::Instant::now());
        let count = std::mem::take(&mut self.native_view.pending);
        // Inline bodies update their own routine counters directly. Their work
        // contributes to global fuel, without being charged to the real caller.
        let inlined = std::mem::take(&mut self.native_view.inlined_pending);
        self.instructions = self.instructions.saturating_add(inlined);
        self.execution_engine = ExecutionEngine::from_native(self.native_view.engine);
        if count != 0 {
            self.record_work(count, self.execution_engine);
        }
        if let Some(frame) = self.frames.last_mut() {
            for (value, slot) in frame.registers.iter_mut().zip(&mut frame.native_registers) {
                if slot.kind & DIRTY != 0 {
                    *value = slot.materialize();
                    slot.kind &= !DIRTY;
                }
            }
        }
        for dirty in 0..self.native_view.dirty_count as usize {
            let id = self.native_dirty[dirty];
            let slot = self.native_cells.get_mut(id);
            self.heap[id].value = slot.materialize();
            slot.kind &= !DIRTY;
            if self.bump_cell_version(id).is_err() {
                self.native_view.error = 6;
            }
        }
        let view = &mut self.native_view;
        view.dirty_count = 0;
        let error = std::mem::take(&mut view.error);
        if error != 0 {
            let message = match error {
                1 => "INTEGER overflow",
                2 => "read of uninitialized storage",
                3 => "FOR STEP must not be zero",
                4 => "division by zero",
                6 => "storage version exhausted",
                _ => "internal native execution failure",
            };
            self.fail(message, self.current_span().unwrap_or_default());
        }
        if let (Some(start), Some(report)) = (start, &mut self.report) {
            report.native_materialize_time += start.elapsed();
        }
    }

    /// Generated callees publish pending work before returning. A normal VM
    /// return goes through native_step, which already materializes the callee,
    /// pops its frame, and prepares the caller's view. With no intervening native
    /// instructions, repeating that synchronization is redundant. Dirty budget,
    /// promotion and error exits still require it, including in deeper frames.
    pub(crate) fn synchronize_native_return(&mut self) {
        let clean = self.native_view.pending == 0
            && self.native_view.inlined_pending == 0
            && self.native_view.dirty_count == 0
            && self.native_view.error == 0;
        if let Some(report) = &mut self.report {
            report.boundaries.flush += 1;
            report.boundaries.flush_skipped += u64::from(clean);
        }
        if clean {
            debug_assert!(self.frames.last().is_none_or(|frame| {
                frame
                    .native_registers
                    .iter()
                    .all(|slot| slot.kind & DIRTY == 0)
            }));
            return;
        }
        self.finish_native();
        self.prepare_native();
    }
    pub(super) fn rebuild_native_after_gc(&mut self) {
        self.services.refresh_native();
        debug_assert_eq!(self.native_view.pending, 0);
        debug_assert_eq!(self.native_view.inlined_pending, 0);
        debug_assert_eq!(self.native_view.dirty_count, 0);
        for (id, cell) in self.heap.iter().enumerate() {
            self.native_cells
                .set(id, NativeSlot::from_value(&cell.value));
        }
        self.native_cells.truncate(self.heap.len());
        self.native_dirty.truncate(self.heap.len());
        for frame in &mut self.frames {
            for (slot, value) in frame
                .native_registers
                .iter_mut()
                .zip(frame.registers.iter())
            {
                *slot = NativeSlot::from_value(value);
            }
        }
    }

    /// The caller pins both this table and its executable owners for the run.
    pub(crate) fn bind_native_entries(&mut self, entries: *const usize) {
        self.native_view.entries = entries;
    }
    pub(crate) fn bind_native_control(
        &mut self,
        epoch: *const AtomicU64,
        seen: u64,
        limits: *const u64,
        targets: *const u8,
        interval: u64,
    ) {
        let view = &mut self.native_view;
        view.control_epoch = epoch;
        view.observed_epoch = seen;
        view.transition_limits = limits;
        view.osr_targets = targets;
        view.poll_interval = interval.max(1);
    }
    pub(crate) fn native_exit_reason(&self) -> u64 {
        self.native_view.exit_reason
    }
    pub(crate) fn observe_native_exit(&mut self) {
        if let Some(report) = &mut self.report {
            report.native_exits[self.native_view.exit_reason as usize] += 1;
        }
    }
    pub(crate) fn reset_native_exit(&mut self) {
        self.native_view.exit_reason = ExitReason::Return as u64;
    }
}
