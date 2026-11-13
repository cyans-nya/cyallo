//! Safe storage, checked operations, and resumable interpreter state.
use cyallo_ast::IdentifierEq as _;
use cyallo_ast::Operator;
mod error;
pub mod gc;
mod generator;
mod lookup;
mod memory;
pub(crate) mod native;
pub(crate) mod services;
mod utilities;
use crate::ir::{LocalStorage, Op, Program, Reg};
use crate::metrics::{ExecutionEngine, ExecutionReport, FrameMetrics};
use cyallo_ast::{Diagnostic, Span, Type};
pub use cyallo_values::Value;
use error::expected;
pub use error::{
    AllocatorError, ArithmeticError, BoundsError, EngineError, GcError, GeneratorError,
    IdentitySpace, InputError, Invariant, LookupError, OwnershipError, ResourceLimit, RuntimeError,
    RuntimeFailure, ServiceError, TextError, UninitializedRead, UnwrapError, ValueKind,
};
pub use lookup::LookupStats;
use native::{NativeSlot, NativeStorage, NativeView};
pub use services::ServiceStats;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};
#[derive(Debug, Clone)]
pub struct Context {
    pub input: VecDeque<String>,
    pub output: String,
    pub random_seed: u64,
    pub max_cells: usize,
    pub max_frames: usize,
    /// Captured when the Session is constructed; later edits do not reconfigure GC.
    pub gc: gc::GcConfig,
}
impl Default for Context {
    fn default() -> Self {
        Self {
            input: VecDeque::new(),
            output: String::new(),
            random_seed: 0x9e3779b97f4a7c15,
            max_cells: 1_000_000,
            max_frames: 1024,
            gc: gc::GcConfig::default(),
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Paused,
    WaitingForInput,
    Finished,
    Failed(RuntimeFailure),
}
#[derive(Debug)]
struct Cell {
    identity: u64,
    version: u64,
    // Weak owning-container link: child writes invalidate aggregate snapshots.
    parent: Option<(u64, usize)>,
    ty: Type,
    value: Value,
}
#[derive(Debug)]
pub(crate) struct Frame {
    generator: Option<usize>,
    memory_finalizer: Option<usize>,
    service_scope: Option<usize>,
    service_lease: Option<usize>,
    // Drop registered pointers before their backing locals/registers.
    roots: Option<gc::FrameRoots>,
    pub speed_version: u64,
    pub function: usize,
    pub pc: usize,
    locals: gc::Slots<usize>,
    registers: gc::Slots<Value>,
    native_registers: Box<[NativeSlot]>,
    // Ownership follows the cell, so BYREF writes keep the caller's lifetime.
    owner_id: Option<u64>,
    owned_cells: Vec<usize>,
    metrics: Option<FrameMetrics>,
    return_to: Option<Reg>,
}
pub struct Session {
    generators: generator::Generators,
    memory: memory::Memory,
    lookups: lookup::Lookups,
    next_cell_identity: u64,
    next_symbol: u64,
    services: services::Services,
    pub(crate) program: Arc<Program>,
    pub context: Context,
    pub(crate) frames: Vec<Frame>,
    global_roots: Option<gc::RootScope>,
    heap: gc::Arena<Cell>,
    globals: gc::Slots<usize>,
    status: Status,
    initial_context: Context,
    instructions: u64,
    constant_memory: Option<usize>,
    function_work: Vec<u64>,
    function_calls: Vec<u64>,
    native_cells: NativeStorage,
    native_view: Box<NativeView>,
    native_dirty: Vec<usize>,
    // No-GC calls recycle frame-owned cells without moving global cell IDs.
    free_cells: Vec<usize>,
    cell_owners: Vec<Option<u64>>,
    transaction_reused_cells: Vec<usize>,
    track_reused_cells: bool,
    pending_frame_cells: Vec<usize>,
    pending_frame_owner: Option<u64>,
    allocation_owner: Option<u64>,
    next_frame_owner: u64,
    native_enabled: bool,
    execution_engine: ExecutionEngine,
    report: Option<ExecutionReport>,
    collector: gc::Collector,
    gc_config: gc::GcConfig,
    gc_instruction: Option<(usize, usize)>,
}
type RunResult<T> = Result<T, RuntimeError>;
impl Session {
    pub fn new(program: Arc<Program>) -> Result<Self, Diagnostic> {
        Self::with_context(program, Context::default())
    }
    pub fn with_context(program: Arc<Program>, mut context: Context) -> Result<Self, Diagnostic> {
        context.gc.enabled |= program.requires_tracing;
        let function_count = program.functions.len();
        let mut s = Self {
            generators: Default::default(),
            memory: memory::Memory::default(),
            lookups: lookup::Lookups::default(),
            next_cell_identity: 1,
            next_symbol: 1,
            services: services::Services::default(),
            program,
            initial_context: context.clone(),
            gc_config: context.gc,
            context,
            frames: vec![],
            heap: gc::Arena::default(),
            global_roots: None,
            globals: vec![].into(),
            status: Status::Paused,
            instructions: 0,
            constant_memory: None,
            function_work: vec![0; function_count],
            function_calls: vec![0; function_count],
            native_cells: NativeStorage::default(),
            native_view: Box::default(),
            native_dirty: vec![],
            free_cells: vec![],
            cell_owners: vec![],
            transaction_reused_cells: vec![],
            track_reused_cells: false,
            pending_frame_cells: vec![],
            pending_frame_owner: None,
            allocation_owner: None,
            next_frame_owner: 1,
            native_enabled: false,
            execution_engine: ExecutionEngine::Interpreter,
            report: None,
            collector: gc::Collector::default(),
            gc_instruction: None,
        };
        s.push_frame(0, vec![], None)
            .map_err(|e| RuntimeFailure::new(e, Span::default()).diagnostic())?;
        s.globals = s.frames[0].locals.clone();
        s.register_globals();
        Ok(s)
    }
    pub fn function_instruction_count(&self, function: usize) -> u64 {
        self.function_work.get(function).copied().unwrap_or(0)
    }
    /// Number of entries into each routine. Cheap to maintain: a single
    /// saturating increment when a frame is pushed for a non-entry routine.
    pub fn function_call_count(&self, function: usize) -> u64 {
        self.function_calls.get(function).copied().unwrap_or(0)
    }
    pub fn instruction_count(&self) -> u64 {
        self.instructions
    }
    /// Logical calls completed through the frameless native inline path.
    pub fn inlined_call_count(&self) -> u64 {
        self.native_view.inlined_calls
    }
    /// Calls entering embedded procedure code while retaining a real VM frame.
    pub fn body_inlined_call_count(&self) -> u64 {
        self.native_view.body_inline_calls
    }
    /// Start a fresh, opt-in report. Existing live frames retain their state.
    pub fn enable_reporting(&mut self, origin: std::time::Instant) {
        let mut report = ExecutionReport::new(
            origin,
            self.program.functions.len(),
            self.frames.len(),
            self.heap.len(),
        );
        report.gc_pause_at_start = self.collector.stats.pause_time;
        for frame in &mut self.frames {
            frame.metrics = Some(report.frame(frame.function));
        }
        self.report = Some(report);
        self.native_view.reporting = 1;
    }
    pub fn execution_report(&self) -> Option<&ExecutionReport> {
        self.report.as_ref()
    }
    pub(crate) fn execution_report_mut(&mut self) -> Option<&mut ExecutionReport> {
        self.report.as_mut()
    }
    pub(crate) fn select_execution_engine(&mut self, engine: ExecutionEngine) -> ExecutionEngine {
        std::mem::replace(&mut self.execution_engine, engine)
    }
    pub(crate) fn enter_reporting(&mut self) {
        let depth = self.frames.len();
        if let (Some(report), Some(frame)) = (&mut self.report, self.frames.last_mut())
            && let Some(metrics) = &mut frame.metrics
        {
            report.enter(
                metrics,
                frame.function,
                frame.pc,
                depth,
                self.execution_engine,
                if self.execution_engine == ExecutionEngine::Interpreter {
                    0
                } else {
                    self.native_view.code_version
                },
            );
        }
    }
    pub(crate) fn pause_reporting(&mut self) {
        if let Some(report) = &mut self.report {
            report.pause();
        }
    }
    #[inline]
    fn record_work(&mut self, count: u64, engine: ExecutionEngine) {
        self.instructions = self.instructions.saturating_add(count);
        if let Some(frame) = self.frames.last_mut() {
            if engine == ExecutionEngine::CraneliftSpeed {
                frame.speed_version = self.native_view.code_version;
            }
            let work = &mut self.function_work[frame.function];
            *work = work.saturating_add(count);
        }
        if self.report.is_some() {
            self.record_observed_work(count, engine);
        }
    }
    #[cold]
    fn record_observed_work(&mut self, count: u64, engine: ExecutionEngine) {
        self.execution_engine = engine;
        self.enter_reporting();
        if let (Some(report), Some(frame)) = (&mut self.report, self.frames.last_mut()) {
            report.functions[frame.function].instructions[engine as usize] += count;
            if let Some(metrics) = &mut frame.metrics {
                metrics.instructions += count;
            }
        }
    }
    pub fn status(&self) -> &Status {
        &self.status
    }
    pub fn depth(&self) -> usize {
        self.frames.len()
    }
    pub fn program(&self) -> &Arc<Program> {
        &self.program
    }
    pub fn current_span(&self) -> Option<Span> {
        if let Status::Failed(diagnostic) = &self.status {
            return Some(diagnostic.span);
        }
        let f = self.frames.last()?;
        self.program.functions[f.function]
            .code
            .get(f.pc)
            .map(|i| i.span)
    }
    pub fn at_statement(&self) -> bool {
        self.frames.last().is_some_and(|f| {
            matches!(
                self.program.functions[f.function].code[f.pc].op,
                Op::Statement
            )
        })
    }
    pub fn stack(&self) -> Vec<String> {
        self.frames
            .iter()
            .map(|f| self.program.functions[f.function].name.to_string())
            .collect()
    }
    pub fn variables(&self) -> Vec<(String, String)> {
        let Some(f) = self.frames.last() else {
            return vec![];
        };
        let mut vars: Vec<_> = self.program.functions[f.function]
            .locals
            .iter()
            .zip(self.program.functions[f.function].local_storage.iter())
            .filter_map(|(local, storage)| {
                let name = local.display_name.as_ref()?;
                let value = match *storage {
                    LocalStorage::Cell(index) => self.inspect(f.locals[index]),
                    LocalStorage::Register(register) => f.registers[register].to_string(),
                };
                Some((name.to_string(), value))
            })
            .collect();
        if f.function != 0 {
            vars.extend(
                self.program.functions[0]
                    .locals
                    .iter()
                    .zip(self.globals.iter())
                    .filter_map(|(local, id)| {
                        local
                            .display_name
                            .as_ref()
                            .map(|name| (format!("global::{name}"), self.inspect(*id)))
                    }),
            );
        }
        vars
    }
    fn inspect(&self, id: usize) -> String {
        match &self.heap[id].value {
            Value::Array(ids) => format!(
                "[{}]",
                ids.iter()
                    .take(32)
                    .map(|id| self.inspect(*id))
                    .chain((ids.len() > 32).then_some("…".into()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Value::SparseArray(entries, length) => format!(
                "length={length} {{{}}}",
                entries
                    .iter()
                    .take(32)
                    .map(|(key, id)| format!("{key}: {}", self.inspect(*id)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            v => v.to_string(),
        }
    }
    /// Replace code between REPL entries, preserving storage by name rather than slot number.
    pub(crate) fn continue_with(
        &mut self,
        program: Arc<Program>,
        offset: u32,
    ) -> Result<(), Diagnostic> {
        if program.requires_tracing && !self.gc_config.enabled {
            self.gc_config.enabled = true;
            self.context.gc.enabled = true;
            self.collector.reserve(self.heap.len());
            self.free_cells.clear();
            self.cell_owners.clear();
            self.register_globals();
            for frame in &mut self.frames {
                let function = &self.program.functions[frame.function];
                // SAFETY: stopped frames own fixed slot backing, as in push_frame.
                frame.roots = unsafe {
                    self.collector
                        .frame(&frame.locals, &frame.registers, &function.gc.registers)
                };
            }
        }
        let staged = self.root_scope();
        let mut pending = Vec::with_capacity(program.functions[0].locals.len());
        let heap_len = self.heap.len();
        self.transaction_reused_cells.clear();
        self.track_reused_cells = true;
        for local in &program.functions[0].locals {
            if let Some((index, old)) = self.program.functions[0]
                .locals
                .iter()
                .enumerate()
                .find(|(_, old)| old.name.eq_identifier(&local.name))
            {
                if old.ty != local.ty || old.constant != local.constant {
                    let old_span = old.span;
                    let reused = std::mem::take(&mut self.transaction_reused_cells);
                    self.track_reused_cells = false;
                    if !self.gc_config.enabled {
                        self.heap.truncate(heap_len);
                        self.native_cells.truncate(heap_len);
                        self.cell_owners.truncate(heap_len);
                    }
                    self.release_cells(reused);
                    return Err(Diagnostic::new(
                        "E201",
                        format!(
                            "cannot change established type or constness of {}",
                            local.name
                        ),
                        local.span,
                    )
                    .with_related(old_span, "established here"));
                }
                pending.push(staged.cell(self.globals[index]));
            } else {
                match self.alloc(&local.ty) {
                    Ok(id) => pending.push(staged.cell(id)),
                    Err(message) => {
                        let reused = std::mem::take(&mut self.transaction_reused_cells);
                        self.track_reused_cells = false;
                        if !self.gc_config.enabled {
                            self.heap.truncate(heap_len);
                            self.native_cells.truncate(heap_len);
                            self.cell_owners.truncate(heap_len);
                        }
                        self.release_cells(reused);
                        return Err(RuntimeFailure::new(message, local.span).diagnostic());
                    }
                }
            }
        }
        self.track_reused_cells = false;
        self.transaction_reused_cells.clear();
        let locals: Vec<_> = pending.iter().map(|root| staged.cell_id(*root)).collect();
        let main = &program.functions[0];
        let pc = main
            .code
            .iter()
            .position(|i| matches!(i.op, Op::Statement) && i.span.start >= offset)
            .unwrap_or(main.code.len() - 1);
        let registers = vec![Value::Uninitialized; main.registers.len()];
        self.frames = vec![Frame {
            generator: None,
            memory_finalizer: None,
            service_scope: None,
            service_lease: None,
            roots: None,
            speed_version: 0,
            function: 0,
            pc,
            locals: locals.clone().into(),
            native_registers: if self.native_enabled {
                vec![NativeSlot::default(); registers.len()].into_boxed_slice()
            } else {
                Box::default()
            },
            owner_id: None,
            owned_cells: vec![],
            registers: registers.into(),
            return_to: None,
            metrics: None,
        }];
        self.global_roots = None;
        self.globals = locals.into();
        self.register_globals();
        if self.gc_config.enabled {
            let frame = &mut self.frames[0];
            frame.roots = unsafe {
                self.collector.frame(
                    &frame.locals,
                    &frame.registers,
                    &program.functions[0].gc.registers,
                )
            };
        }
        self.function_work.resize(program.functions.len(), 0);
        self.function_calls.resize(program.functions.len(), 0);
        // Routine IDs can move when a REPL entry adds named routines before
        // lifted closures. Remap stored functions and nested environments by
        // their source-stable declaration names before replacing the program.
        let remap: Vec<_> = self
            .program
            .functions
            .iter()
            .map(|old| {
                program
                    .functions
                    .iter()
                    .position(|new| new.symbol == old.symbol)
                    .expect("accepted declarations persist")
            })
            .collect();
        let key_remap: Vec<_> = self
            .program
            .services
            .keys
            .iter()
            .map(|old| {
                program
                    .services
                    .keys
                    .iter()
                    .position(|new| new.span == old.span)
                    .expect("accepted service declarations persist")
            })
            .collect();
        for id in 0..self.heap.len() {
            remap_functions(&mut self.heap[id].value, &remap);
            remap_service_keys(&mut self.heap[id].value, &key_remap);
            if self.native_enabled {
                self.native_cells
                    .set(id, NativeSlot::from_value(&self.heap[id].value));
            }
        }
        self.relink_memory(&remap);
        self.services
            .relink(&remap, &key_remap, program.services.keys.len());
        self.reset_lookup_caches();
        program.notice(
            "invalidation",
            0,
            "Program snapshot replaced; previous artifacts must be rebuilt",
        );
        self.program = program;
        self.status = Status::Paused;
        self.report = None;
        self.native_view.reporting = 0;
        Ok(())
    }
    /// Inspect persistent globals even after the top-level entry has returned.
    pub fn globals(&self) -> Vec<(String, String)> {
        self.program.functions[0]
            .locals
            .iter()
            .zip(self.globals.iter())
            .filter_map(|(local, id)| {
                local
                    .display_name
                    .as_ref()
                    .map(|name| (name.to_string(), self.inspect(*id)))
            })
            .collect()
    }
    pub(crate) fn constant_value(&self, index: usize) -> RunResult<Value> {
        self.read(self.globals[index])
    }
    pub(crate) fn limit_constant_memory(&mut self, bytes: usize) {
        self.constant_memory = Some(bytes);
    }
    fn check_constant_allocation(&mut self, op: &Op) -> RunResult<()> {
        if self.constant_memory.is_none() {
            return Ok(());
        }
        let regs = &self.frames.last().unwrap().registers;
        let bytes = match op {
            Op::Frozen(_, value) => value.allocation_size(),
            Op::LiteralString(_, text) => text.len(),
            Op::Binary(_, Operator::Concat, a, b) => {
                let size = |r: usize| match &regs[r] {
                    Value::String(s) => s.len(),
                    _ => 32,
                };
                size(*a).checked_add(size(*b))
            }
            .ok_or(ResourceLimit::ConstantAllocation)?,
            Op::Builtin(_, name, args)
                if matches!(
                    *name,
                    cyallo_ir::intrinsic::Intrinsic::Replace
                        | cyallo_ir::intrinsic::Intrinsic::Upper
                        | cyallo_ir::intrinsic::Intrinsic::Lower
                        | cyallo_ir::intrinsic::Intrinsic::Trim
                        | cyallo_ir::intrinsic::Intrinsic::Left
                        | cyallo_ir::intrinsic::Intrinsic::Right
                        | cyallo_ir::intrinsic::Intrinsic::Mid
                ) =>
            {
                let Value::String(text) = &regs[args[0]] else {
                    return Ok(());
                };
                if *name == cyallo_ir::intrinsic::Intrinsic::Replace {
                    let (Value::String(from), Value::String(to)) = (&regs[args[1]], &regs[args[2]])
                    else {
                        return Ok(());
                    };
                    text.matches(from.as_ref())
                        .count()
                        .checked_mul(to.len())
                        .and_then(|n| n.checked_add(text.len()))
                } else {
                    text.len().checked_mul(3)
                }
                .ok_or(ResourceLimit::ConstantAllocation)?
            }
            Op::ArrayLiteral(_, args) | Op::Aggregate(_, args) | Op::FunctionValue(_, _, args) => {
                args.len().saturating_mul(size_of::<Value>())
            }
            _ => 0,
        };
        let remaining = self.constant_memory.as_mut().unwrap();
        *remaining = remaining
            .checked_sub(bytes)
            .ok_or(ResourceLimit::ConstantAllocation)?;
        Ok(())
    }
    pub fn restart(&mut self) -> Result<(), Diagnostic> {
        let mut context = self.initial_context.clone();
        context.output.clear();
        *self = Self::with_context(self.program.clone(), context)?;
        Ok(())
    }
    pub fn run(&mut self, budget: usize) -> &Status {
        let previous = self.execution_engine;
        self.execution_engine = ExecutionEngine::Interpreter;
        for _ in 0..budget {
            if !self.step() {
                break;
            }
        }
        self.pause_reporting();
        self.execution_engine = previous;
        &self.status
    }
    /// Execute one IR instruction. Waiting input does not advance the program counter.
    pub fn step(&mut self) -> bool {
        if matches!(self.status, Status::Finished | Status::Failed(_)) {
            return false;
        }
        if self.status == Status::WaitingForInput && self.context.input.is_empty() {
            return false;
        }
        self.status = Status::Paused;
        let span = self.current_span().unwrap_or_default();
        self.record_work(1, self.execution_engine);
        if self.gc_config.enabled {
            self.gc_instruction = self.frames.last().map(|f| (self.frames.len() - 1, f.pc));
        }
        let result = self.execute();
        self.gc_instruction = None;
        // Cleanup enters after the instruction commits its result. Collection
        // must publish the successor's live values, including that new result.
        let result = result.and_then(|_| self.start_memory_finalizer());
        if let Err(e) = result {
            self.fail(e, span);
            return false;
        }
        !matches!(
            self.status,
            Status::Finished | Status::Failed(_) | Status::WaitingForInput
        )
    }
    pub(crate) fn fail(&mut self, error: impl Into<RuntimeError>, span: Span) {
        self.status = Status::Failed(RuntimeFailure::new(error.into(), span));
    }
    /// A deferred body or mapper failed its semantic checks on first use.
    pub(crate) fn fail_check(&mut self, diagnostic: Diagnostic) {
        let span = diagnostic.span;
        self.fail(RuntimeError::DeferredCheck(diagnostic), span);
    }
    fn alloc(&mut self, ty: &Type) -> RunResult<usize> {
        if let Type::RecordRef(identity) = ty {
            let record = self
                .program
                .records
                .get(identity.as_ref())
                .cloned()
                .ok_or(Invariant::UnknownRecordLayout)?;
            return self.alloc(&record);
        }
        self.maybe_collect()?;
        let identity = self.next_cell_identity;
        self.next_cell_identity = self
            .next_cell_identity
            .checked_add(1)
            .ok_or(IdentitySpace::StorageIdentity)?;
        let id = if !self.gc_config.enabled
            && let Some(id) = self.free_cells.pop()
        {
            if self.track_reused_cells {
                self.transaction_reused_cells.push(id);
            }
            self.heap[id] = Cell {
                identity,
                version: 0,
                parent: None,
                ty: ty.clone(),
                value: Value::Uninitialized,
            };
            if self.native_enabled {
                self.native_cells.set(id, NativeSlot::default());
            }
            id
        } else {
            if self.heap.len() >= self.context.max_cells || self.heap.len() >= u32::MAX as usize {
                return Err(ResourceLimit::Storage.into());
            }
            if self.gc_config.enabled {
                self.collector.reserve(self.heap.len() + 1);
            }
            let id = self.heap.len();
            self.heap.push(Cell {
                identity,
                version: 0,
                parent: None,
                ty: ty.clone(),
                value: Value::Uninitialized,
            });
            if self.native_enabled {
                self.native_cells.push();
            }
            if !self.gc_config.enabled {
                self.cell_owners.push(None);
            }
            id
        };
        self.record_cell_owner(id);
        self.collector.stats.allocated_cells += 1;
        let occupied = self.heap.len() - self.free_cells.len();
        self.collector.stats.occupied_cells = occupied;
        self.collector.stats.peak_cells = self.collector.stats.peak_cells.max(occupied);
        self.collector.stats.arena_capacity = self.heap.capacity();
        self.collector.stats.arena_bytes = self.heap.bytes();
        if let Some(report) = &mut self.report {
            report.peak_cells = report.peak_cells.max(occupied);
        }
        if let Type::Array(bounds, element) = ty {
            let roots = self.root_scope();
            let parent = roots.cell(id);
            let mut count = 1usize;
            for (low, high) in bounds {
                let width = high
                    .checked_sub(*low)
                    .and_then(|v| v.checked_add(1))
                    .and_then(|v| usize::try_from(v).ok())
                    .ok_or(ResourceLimit::Storage)?;
                count = count.checked_mul(width).ok_or(ResourceLimit::Storage)?;
            }
            if count >= self.context.max_cells {
                return Err(ResourceLimit::Storage.into());
            }
            self.heap[roots.cell_id(parent)].value = Value::Array(Vec::with_capacity(count));
            for _ in 0..count {
                let child = self.alloc(element)?;
                self.attach_child(child, roots.cell_id(parent));
                let Value::Array(ids) = &mut self.heap[roots.cell_id(parent)].value else {
                    unreachable!()
                };
                ids.push(child);
            }
            return Ok(roots.cell_id(parent));
        }
        let fields: Vec<_> = match ty {
            Type::Record(_, fs) => fs.iter().map(|(_, t)| t).collect(),
            Type::Tuple(ts) => ts.iter().collect(),
            _ => return Ok(id),
        };
        let roots = self.root_scope();
        let parent = roots.cell(id);
        self.heap[roots.cell_id(parent)].value = Value::Array(Vec::with_capacity(fields.len()));
        for ty in fields {
            let child = self.alloc(ty)?;
            self.attach_child(child, roots.cell_id(parent));
            let Value::Array(ids) = &mut self.heap[roots.cell_id(parent)].value else {
                unreachable!()
            };
            ids.push(child);
        }
        Ok(roots.cell_id(parent))
    }
    fn record_cell_owner(&mut self, id: usize) {
        if self.gc_config.enabled {
            return;
        }
        let owner = self.allocation_owner;
        self.cell_owners[id] = owner;
        let Some(owner) = owner else {
            return;
        };
        if self.pending_frame_owner == Some(owner) {
            self.pending_frame_cells.push(id);
        } else if let Some(frame) = self
            .frames
            .iter_mut()
            .rev()
            .find(|frame| frame.owner_id == Some(owner))
        {
            frame.owned_cells.push(id);
        }
    }
    fn alloc_for_cell(&mut self, ty: &Type, parent: usize) -> RunResult<usize> {
        let previous_owner = self.allocation_owner;
        self.allocation_owner = if self.gc_config.enabled {
            None
        } else {
            self.cell_owners[parent]
        };
        let result = self.alloc(ty);
        self.allocation_owner = previous_owner;
        result
    }
    fn release_cells(&mut self, cells: Vec<usize>) {
        if self.gc_config.enabled || cells.is_empty() {
            return;
        }
        let released = cells.len() as u64;
        self.free_cells.reserve(cells.len());
        for id in cells {
            self.heap[id].value = Value::Uninitialized;
            self.cell_owners[id] = None;
            if self.native_enabled {
                self.native_cells.set(id, NativeSlot::default());
            }
            self.free_cells.push(id);
        }
        self.collector.stats.reclaimed_cells += released;
        self.collector.stats.occupied_cells = self.heap.len() - self.free_cells.len();
    }
    fn push_frame(
        &mut self,
        function: usize,
        args: Vec<Value>,
        return_to: Option<Reg>,
    ) -> RunResult<()> {
        if self.frames.len() >= self.context.max_frames {
            return Err(ResourceLimit::CallDepth.into());
        }
        let program = Arc::clone(&self.program);
        let fun = &program.functions[function];
        if function != 0
            && let Some(calls) = self.function_calls.get_mut(function)
        {
            *calls = calls.saturating_add(1);
        }
        let mut registers = vec![Value::Uninitialized; fun.registers.len()];
        let cell_count = fun
            .local_storage
            .iter()
            .filter(|storage| matches!(storage, LocalStorage::Cell(_)))
            .count();
        let owner_id = if function != 0 && !self.gc_config.enabled {
            let owner = self.next_frame_owner;
            self.next_frame_owner = self.next_frame_owner.wrapping_add(1).max(1);
            Some(owner)
        } else {
            None
        };
        let previous_owner = self.allocation_owner;
        let previous_pending_owner = self.pending_frame_owner;
        self.pending_frame_cells.clear();
        self.pending_frame_owner = owner_id;
        self.allocation_owner = owner_id;
        let locals = (|| -> RunResult<Vec<usize>> {
            if self.gc_config.enabled && cell_count != 0 {
                let argc = args.len();
                let roots = self.root_scope();
                let arguments = roots.value(Value::ArraySnapshot(args));
                let mut local_roots = smallvec::SmallVec::<[gc::RootId; 8]>::new();
                for (i, (local, storage)) in fun.locals.iter().zip(&fun.local_storage).enumerate() {
                    if let LocalStorage::Register(register) = *storage {
                        if i < argc {
                            registers[register] =
                                coerce_assignment(&local.ty, roots.array_at(arguments, i));
                        }
                        continue;
                    }
                    let root = if fun.params.get(i) == Some(&true) {
                        let Value::Address(id) = roots.array_at(arguments, i) else {
                            return Err(expected(ValueKind::Address));
                        };
                        roots.cell(id)
                    } else {
                        let id = self.alloc(&local.ty)?;
                        let root = roots.cell(id);
                        if i < argc {
                            self.store(roots.cell_id(root), roots.array_at(arguments, i))?;
                        }
                        root
                    };
                    local_roots.push(root);
                }
                // No collecting operation occurs between resolving these indices
                // and registering the completed frame below.
                Ok(local_roots
                    .iter()
                    .map(|r| roots.cell_id(*r))
                    .collect::<Vec<_>>())
            } else {
                let mut locals = Vec::with_capacity(cell_count);
                let mut args = args.into_iter();
                for (i, (local, storage)) in fun.locals.iter().zip(&fun.local_storage).enumerate() {
                    let argument = args.next();
                    if let LocalStorage::Register(register) = *storage {
                        if let Some(value) = argument {
                            registers[register] = coerce_assignment(&local.ty, value);
                        }
                        continue;
                    }
                    let id = if fun.params.get(i) == Some(&true) {
                        let Some(Value::Address(id)) = argument else {
                            return Err(expected(ValueKind::Address));
                        };
                        id
                    } else {
                        let id = self.alloc(&local.ty)?;
                        if let Some(value) = argument {
                            self.store(id, value)?;
                        }
                        id
                    };
                    locals.push(id);
                }
                Ok(locals)
            }
        })();
        self.allocation_owner = previous_owner;
        self.pending_frame_owner = previous_pending_owner;
        let owned_cells = std::mem::take(&mut self.pending_frame_cells);
        let locals = match locals {
            Ok(locals) => locals,
            Err(error) => {
                self.release_cells(owned_cells);
                return Err(error);
            }
        };
        let mut frame = Frame {
            generator: None,
            memory_finalizer: None,
            service_scope: None,
            service_lease: None,
            roots: None,
            speed_version: 0,
            function,
            pc: 0,
            locals: locals.into(),
            native_registers: if self.native_enabled {
                registers.iter().map(NativeSlot::from_value).collect()
            } else {
                Box::default()
            },
            owner_id,
            owned_cells,
            registers: registers.into(),
            return_to,
            metrics: self.report.as_mut().map(|r| r.frame(function)),
        };
        if self.gc_config.enabled {
            // SAFETY: Slots owns fixed UnsafeCell backing; Frame drops roots first.
            frame.roots = unsafe {
                self.collector
                    .frame(&frame.locals, &frame.registers, &fun.gc.registers)
            };
        }
        self.frames.push(frame);
        if let Some(report) = &mut self.report {
            report.peak_frames = report.peak_frames.max(self.frames.len());
        }
        Ok(())
    }
    fn register(&self, r: Reg) -> Value {
        self.frames.last().unwrap().registers[r].clone()
    }
    fn set(&mut self, r: Reg, value: Value) {
        let frame = self.frames.last_mut().unwrap();
        if self.native_enabled {
            frame.native_registers[r] = NativeSlot::from_value(&value);
        }
        frame.registers[r] = value;
    }
    fn address(&self, r: Reg) -> RunResult<usize> {
        match self.register(r) {
            Value::Address(id) | Value::Managed(id) => Ok(id),
            value @ Value::Rc(_) => self.rc_address(&value),
            _ => Err(expected(ValueKind::Address)),
        }
    }
    fn read(&self, id: usize) -> RunResult<Value> {
        let v = &self.heap[id].value;
        if *v == Value::Uninitialized {
            Err(UninitializedRead::Storage.into())
        } else {
            Ok(self.snapshot(v))
        }
    }
    fn snapshot(&self, value: &Value) -> Value {
        match value {
            Value::Deque(ids) => Value::ArraySnapshot(
                ids.iter()
                    .map(|id| self.snapshot(&self.heap[*id].value))
                    .collect(),
            ),
            Value::Array(ids) => Value::ArraySnapshot(
                ids.iter()
                    .map(|id| self.snapshot(&self.heap[*id].value))
                    .collect(),
            ),
            Value::SparseArray(entries, length) => Value::SparseSnapshot(
                entries
                    .iter()
                    .map(|(key, id)| (*key, self.snapshot(&self.heap[*id].value)))
                    .collect(),
                *length,
            ),
            v => v.clone(),
        }
    }
    fn store(&mut self, id: usize, value: Value) -> RunResult<()> {
        let previous_owner = self.allocation_owner;
        self.allocation_owner = if self.gc_config.enabled {
            None
        } else {
            self.cell_owners[id]
        };
        let result = self.store_inner(id, value);
        self.allocation_owner = previous_owner;
        result
    }
    fn attach_child(&mut self, child: usize, parent: usize) {
        self.heap[child].parent = Some((self.heap[parent].identity, parent));
    }
    fn bump_cell_version(&mut self, mut id: usize) -> RunResult<()> {
        loop {
            self.heap[id].version = self.heap[id]
                .version
                .checked_add(1)
                .ok_or(IdentitySpace::StorageVersion)?;
            match self.heap[id].parent {
                Some((identity, parent))
                    if parent < self.heap.len() && self.heap[parent].identity == identity =>
                {
                    id = parent
                }
                _ => break,
            }
        }
        Ok(())
    }
    fn store_inner(&mut self, id: usize, value: Value) -> RunResult<()> {
        self.bump_cell_version(id)?;
        if let Type::Storage(kind, element) = self.heap[id].ty.clone()
            && matches!(
                kind,
                cyallo_ast::StorageKind::Vec | cyallo_ast::StorageKind::VecDeque
            )
        {
            let roots = self.root_scope();
            let target = roots.cell(id);
            let source = roots.value(value);
            if cyallo_ir::ownership::effectful_lifecycle(&self.heap[id].ty) {
                self.drop_cell(id)?;
            }
            let len = roots.array_len(source).ok_or(expected(ValueKind::Array))?;
            self.heap[roots.cell_id(target)].value = if kind == cyallo_ast::StorageKind::Vec {
                Value::Array(Vec::new())
            } else {
                Value::Deque(VecDeque::new())
            };
            for index in 0..len {
                let parent = roots.cell_id(target);
                let child = self.alloc_for_cell(&element, parent)?;
                self.attach_child(child, roots.cell_id(target));
                match &mut self.heap[roots.cell_id(target)].value {
                    Value::Array(ids) => ids.push(child),
                    Value::Deque(ids) => ids.push_back(child),
                    _ => unreachable!(),
                }
                self.store(child, roots.array_at(source, index))?;
            }
            if self.native_enabled {
                self.native_cells.set(
                    roots.cell_id(target),
                    NativeSlot::from_value(&self.heap[roots.cell_id(target)].value),
                );
            }
            return Ok(());
        }
        // Scalar/string writes cannot collect. Avoid root registration on the
        // common numeric path, including interpreter-only execution.
        if !matches!(
            self.heap[id].ty,
            Type::Array(..) | Type::SparseArray(..) | Type::Record(..) | Type::Tuple(..)
        ) {
            if cyallo_ir::ownership::effectful_lifecycle(&self.heap[id].ty) {
                self.drop_cell(id)?;
            }
            self.heap[id].value = coerce_assignment(&self.heap[id].ty, value);
            if self.native_enabled {
                self.native_cells
                    .set(id, NativeSlot::from_value(&self.heap[id].value));
            }
            return Ok(());
        }
        let roots = self.root_scope();
        let target = roots.cell(id);
        let source = roots.value(value);
        let ty = self.heap[roots.cell_id(target)].ty.clone();
        if let Some(len) = roots.array_len(source) {
            let Value::Array(dst) = &self.heap[roots.cell_id(target)].value else {
                return Err(Invariant::ArrayShapeMismatch.into());
            };
            if dst.len() != len {
                return Err(Invariant::ArrayShapeMismatch.into());
            }
            for i in 0..len {
                let Value::Array(dst) = &self.heap[roots.cell_id(target)].value else {
                    unreachable!()
                };
                let child = dst[i];
                self.store(child, roots.array_at(source, i))?;
            }
        } else if let (Type::SparseArray(element), Some((keys, length))) =
            (&ty, roots.sparse_keys(source))
        {
            let dest = roots.value(Value::SparseArray(BTreeMap::new(), length));
            for key in keys {
                let old = match &self.heap[roots.cell_id(target)].value {
                    Value::SparseArray(entries, _) => entries.get(&key).copied(),
                    _ => None,
                };
                let child = if let Some(id) = old {
                    id
                } else {
                    self.alloc(element)?
                };
                self.attach_child(child, roots.cell_id(target));
                roots.insert_sparse(dest, key, child);
                self.store(child, roots.sparse_at(source, key))?;
            }
            self.heap[roots.cell_id(target)].value = roots.take_value(dest);
        } else {
            let value = match (&ty, roots.value_copy(source)) {
                (Type::Real, Value::Integer(n)) => Value::Real(n as f64),
                (_, value) => value,
            };
            self.heap[roots.cell_id(target)].value = value;
        }
        let id = roots.cell_id(target);
        if self.native_enabled {
            self.native_cells
                .set(id, NativeSlot::from_value(&self.heap[id].value));
        }
        Ok(())
    }
    fn call(&mut self, function: usize, args: Vec<Value>, result: Option<usize>) -> RunResult<()> {
        let routine = &self.program.functions[function];
        let started =
            (self.report.is_some() && !routine.is_analyzed()).then(std::time::Instant::now);
        let checked = routine.checked().map(|_| ()).map_err(Clone::clone);
        if let (Some(start), Some(report)) = (started, &mut self.report) {
            report.deferred_bodies += 1;
            report.body_analysis_time += start.elapsed();
        }
        if let Err(error) = checked {
            self.fail_check(error);
            return Ok(());
        }
        if routine.generator.is_some() {
            self.construct_generator(function, args, result)
        } else {
            self.push_frame(function, args, result)
        }
    }
    fn execute(&mut self) -> RunResult<()> {
        let f = self.frames.last().unwrap();
        let function = f.function;
        let op = self.program.bytecode.functions[function].decode(f.pc);
        self.check_constant_allocation(&op)?;
        self.frames.last_mut().unwrap().pc += 1;
        match op {
            Op::Yield(input, value) => self.yield_generator(input, value)?,
            Op::Frozen(r, value) => self.set(r, Value::from(value.as_ref())),
            Op::Lookup(r, operation, args) => self.execute_lookup(r, operation, args)?,
            Op::ServiceKey(r, key) => self.set(r, Value::ServiceKey(key)),
            Op::Service(r, operation, args) => self.execute_service(r, operation, args)?,
            Op::TraitCall(result, target, method, args) => self.trait_call(
                result,
                target,
                method,
                args,
                self.frames.last().unwrap().pc - 1,
            )?,
            Op::TraitBox(r, value, implementation) => {
                let roots = self.root_scope();
                let source = roots.value(self.register(value));
                let ty = self.program.services.implementations[implementation]
                    .receiver
                    .clone();
                let previous = self.allocation_owner;
                self.allocation_owner = None;
                let allocated = self.alloc(&ty);
                self.allocation_owner = previous;
                let receiver = allocated?;
                let root = roots.cell(receiver);
                self.store(roots.cell_id(root), roots.value_copy(source))?;
                self.set(r, Value::TraitObject(roots.cell_id(root), implementation));
            }
            Op::Statement | Op::Nop => {}
            Op::Aggregate(r, regs) => {
                let values = regs.into_iter().map(|r| self.register(r)).collect();
                self.set(r, Value::ArraySnapshot(values));
            }
            Op::Field(r, base, index) => {
                let id = self.address(base)?;
                let Value::Array(ids) = &self.heap[id].value else {
                    return Err(Invariant::InvalidStorage.into());
                };
                let id = *ids.get(index).ok_or(BoundsError::Field(index))?;
                self.set(r, Value::Address(id));
            }
            Op::Extract(r, base, index) => {
                let Value::ArraySnapshot(values) = self.register(base) else {
                    return Err(expected(ValueKind::Composite));
                };
                let value = values
                    .get(index)
                    .cloned()
                    .ok_or(BoundsError::Field(index))?;
                if value == Value::Uninitialized {
                    return Err(UninitializedRead::Storage.into());
                }
                self.set(r, value);
            }
            Op::FunctionValue(r, id, regs) => {
                let captures = regs.into_iter().map(|r| self.register(r)).collect();
                self.set(r, Value::Function(id, captures));
            }
            Op::Invoke(r, callee, args) => {
                let Value::Function(function, mut captures) = self.register(callee) else {
                    return Err(expected(ValueKind::Callable));
                };
                if self.program.functions[function].generator.is_some() {
                    let roots = self.root_scope();
                    let count = captures.len();
                    let original = roots.value(Value::ArraySnapshot(captures));
                    let mut copies = Vec::with_capacity(count);
                    for index in 0..count {
                        let value = self.clone_value(roots.array_at(original, index))?;
                        copies.push(roots.value(value));
                    }
                    captures = copies.into_iter().map(|id| roots.value_copy(id)).collect();
                }
                captures.extend(args.into_iter().map(|r| self.register(r)));
                self.call(function, captures, r)?;
            }
            Op::ArrayLiteral(r, regs) => {
                if matches!(
                    self.program.functions[function].registers[r],
                    Type::Array(..)
                ) {
                    let values = regs.into_iter().map(|r| self.register(r)).collect();
                    self.set(r, Value::ArraySnapshot(values));
                    return Ok(());
                }
                let length = regs.len() as u64;
                let values = regs
                    .into_iter()
                    .enumerate()
                    .map(|(i, r)| (i as u32, self.register(r)))
                    .collect();
                self.set(r, Value::SparseSnapshot(values, length));
            }
            Op::Length(r, address) => {
                let id = self.address(address)?;
                let length = match &self.heap[id].value {
                    Value::String(text) => text.chars().count() as i64,
                    Value::SparseArray(_, length) => *length as i64,
                    Value::Array(ids) => ids.len() as i64,
                    Value::Deque(ids) => ids.len() as i64,
                    Value::Uninitialized => return Err(UninitializedRead::Storage.into()),
                    _ => return Err(expected(ValueKind::Array)),
                };
                self.set(r, Value::Integer(length));
            }
            Op::Literal(r, l) => self.set(r, l.into()),
            Op::LiteralString(r, text) => self.set(r, Value::String(text)),
            Op::Address(r, s) => {
                let id = if s.global {
                    self.globals[s.index]
                } else {
                    let index = self.program.functions[function].cell_slot(s.index);
                    self.frames.last().unwrap().locals[index]
                };
                self.set(r, Value::Address(id));
            }
            Op::Load(r, a) => {
                let v = self.read(self.address(a)?)?;
                self.set(r, v);
            }
            Op::Store(a, r) => {
                let id = self.address(a)?;
                let v = self.register(r);
                self.store(id, v)?;
            }
            Op::FrameLoad(r, slot) => {
                let value = self.register(slot);
                if value == Value::Uninitialized {
                    return Err(UninitializedRead::Storage.into());
                }
                self.set(r, value);
            }
            Op::FrameStore(slot, r) => {
                let value = coerce_assignment(
                    &self.program.functions[function].registers[slot],
                    self.register(r),
                );
                self.set(slot, value);
            }
            Op::Index(r, a, indices, write) => {
                if let Value::Slice {
                    owner,
                    offset,
                    length,
                } = self.register(a)
                {
                    let requested = integer(self.register(indices[0]))?;
                    let index = usize::try_from(requested)
                        .ok()
                        .filter(|index| *index < length)
                        .ok_or(BoundsError::Slice {
                            index: requested,
                            length,
                        })?;
                    let Value::Array(elements) = &self.heap[owner].value else {
                        return Err(Invariant::InvalidSliceStorage.into());
                    };
                    // The owner is re-checked: a slice must never index past live storage.
                    let element = offset
                        .checked_add(index)
                        .and_then(|position| elements.get(position))
                        .ok_or(BoundsError::Slice {
                            index: requested,
                            length,
                        })?;
                    self.set(r, Value::Address(*element));
                    return Ok(());
                }
                let id = self.address(a)?;
                if let Type::SparseArray(element) = self.heap[id].ty.clone() {
                    let requested = integer(self.register(indices[0]))?;
                    let index = u32::try_from(requested)
                        .ok()
                        .filter(|i| *i < u32::MAX)
                        .ok_or(BoundsError::SparseIndex(requested))?;
                    if self.heap[id].value == Value::Uninitialized && write {
                        self.heap[id].value = Value::SparseArray(BTreeMap::new(), 0);
                    }
                    let Value::SparseArray(entries, _) = &self.heap[id].value else {
                        return Err(UninitializedRead::Storage.into());
                    };
                    let cell = if let Some(cell) = entries.get(&index) {
                        *cell
                    } else if write {
                        let roots = self.root_scope();
                        let container = roots.cell(id);
                        let cell = self.alloc_for_cell(&element, id)?;
                        self.attach_child(cell, roots.cell_id(container));
                        let Value::SparseArray(entries, length) =
                            &mut self.heap[roots.cell_id(container)].value
                        else {
                            unreachable!()
                        };
                        entries.insert(index, cell);
                        *length = (*length).max(index as u64 + 1);
                        let parent = roots.cell_id(container);
                        self.bump_cell_version(parent)?;
                        cell
                    } else {
                        return Err(UninitializedRead::Hole(index).into());
                    };
                    self.set(r, Value::Address(cell));
                    return Ok(());
                }

                if matches!(
                    self.heap[id].ty,
                    Type::Storage(
                        cyallo_ast::StorageKind::Vec | cyallo_ast::StorageKind::VecDeque,
                        _
                    )
                ) {
                    let requested = integer(self.register(indices[0]))?;
                    let (length, child) = match &self.heap[id].value {
                        Value::Array(cells) => (
                            cells.len(),
                            usize::try_from(requested).ok().and_then(|i| cells.get(i)),
                        ),
                        Value::Deque(cells) => (
                            cells.len(),
                            usize::try_from(requested).ok().and_then(|i| cells.get(i)),
                        ),
                        _ => return Err(UninitializedRead::Collection.into()),
                    };
                    let child = *child.ok_or(BoundsError::Collection {
                        index: requested,
                        length,
                    })?;
                    self.set(r, Value::Address(child));
                    return Ok(());
                }
                let Type::Array(bounds, _) = &self.heap[id].ty else {
                    return Err(expected(ValueKind::Array));
                };
                let mut offset = 0usize;
                for ((l, h), r) in bounds.iter().zip(indices) {
                    let i = integer(self.register(r))?;
                    if i < *l || i > *h {
                        return Err(BoundsError::Array {
                            index: i,
                            lower: *l,
                            upper: *h,
                        }
                        .into());
                    }
                    offset = offset * (*h - *l + 1) as usize + (i - *l) as usize;
                }
                let Value::Array(ids) = &self.heap[id].value else {
                    return Err(UninitializedRead::Storage.into());
                };
                self.set(r, Value::Address(ids[offset]));
            }
            Op::Take(r, address) => {
                let value = self.take_cell(self.address(address)?)?;
                self.set(r, value);
            }
            Op::ManagedAddress(result, owner) => {
                let address = self.address(owner)?;
                self.set(result, Value::Address(address));
            }
            Op::Drop(address) => self.drop_cell(self.address(address)?)?,
            Op::Unary(r, op, a) => {
                let v = unary(op, self.register(a))?;
                self.set(r, v);
            }
            Op::Binary(r, op, a, b) => {
                let v = binary(op, self.register(a), self.register(b))?;
                self.set(r, v);
            }
            Op::Move(r, a) => {
                let v = self.register(a);
                self.set(r, v);
            }
            Op::Jump(pc) => self.frames.last_mut().unwrap().pc = pc,
            Op::Branch(r, a, b) => {
                let v = boolean(self.register(r))?;
                self.frames.last_mut().unwrap().pc = if v { a } else { b };
            }
            Op::Call(r, function, args) => {
                let args = args.into_iter().map(|r| self.register(r)).collect();
                self.call(function, args, r)?;
            }
            Op::Return(r) => {
                if self.frames.last().unwrap().generator.is_some() {
                    self.complete_generator(r)?;
                    return Ok(());
                }
                let lookup_return = self.lookup_return();
                let mut value = r.map(|r| self.register(r));
                if lookup_return.is_some() {
                    value =
                        Some(self.finish_lookup(
                            value.take().ok_or(LookupError::MapperReturnedNothing)?,
                        )?);
                }
                let returned = self.root_scope();
                let returned_value = value.take().map(|value| returned.value(value));
                let active = self.frames.last().unwrap();
                if active.function != 0 {
                    let function = &self.program.functions[active.function];
                    let cells: Vec<_> = function
                        .locals
                        .iter()
                        .zip(function.local_storage.iter())
                        .filter_map(|(local, storage)| {
                            if cyallo_ir::ownership::needs_drop(&local.ty) {
                                if let LocalStorage::Cell(index) = storage {
                                    Some(active.locals[*index])
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        })
                        .collect();
                    for cell in cells {
                        self.drop_cell(cell)?;
                    }
                }
                let mut frame = self.frames.pop().unwrap();
                let function = frame.function;
                let return_to = frame.return_to;
                let service_lease = frame.service_lease;
                let memory_finalizer = frame.memory_finalizer;
                let owned_cells = std::mem::take(&mut frame.owned_cells);
                let result = self.program.functions[function].result.clone();
                drop(frame);
                if self.frames.is_empty() {
                    self.status = Status::Finished;
                } else if let Some(r) = return_to {
                    let mut value = returned_value
                        .map(|root| returned.value_copy(root))
                        .ok_or(Invariant::MissingReturnValue)?;
                    if lookup_return.is_none()
                        && result == Type::Real
                        && let Value::Integer(n) = value
                    {
                        value = Value::Real(n as f64);
                    }
                    self.set(r, value);
                }
                self.release_cells(owned_cells);
                if let Some(binding) = service_lease {
                    self.release_service_lease(binding);
                }
                self.release_retired();
                if let Some(block) = memory_finalizer {
                    self.finish_memory_finalizer(block)?;
                }
            }
            Op::Builtin(r, n, args) => {
                let args = args.into_iter().map(|r| self.register(r)).collect();
                let value = builtin(
                    n,
                    args,
                    &mut self.context.random_seed,
                    self.context.max_cells,
                )?;
                let value = if let Some(kind) =
                    self.program.functions[function].registers[r].number_kind()
                    && let Some(number) = value.as_number()
                    && number.kind != kind
                {
                    Value::from_number(number.cast(kind)?)
                } else {
                    value
                };
                self.set(r, value);
            }
            Op::Output(rs) => {
                let text = rs
                    .into_iter()
                    .map(|r| self.register(r).to_string())
                    .collect::<String>();
                self.context.output.push_str(&text);
                self.context.output.push('\n');
            }
            Op::Input(r, ref ty) | Op::FrameInput(r, ref ty) => {
                if let Some(line) = self.context.input.pop_front() {
                    let value = parse_input(&line, ty)?;
                    if matches!(op, Op::FrameInput(..)) {
                        self.set(r, value);
                    } else {
                        self.store(self.address(r)?, value)?;
                    }
                } else {
                    self.frames.last_mut().unwrap().pc -= 1;
                    self.status = Status::WaitingForInput;
                }
            }
            Op::CheckStep(r) => {
                if integer(self.register(r))? == 0 {
                    return Err(ArithmeticError::ZeroStep.into());
                }
            }
        }
        Ok(())
    }
    pub(crate) fn active_function(&self) -> Option<usize> {
        self.frames.last().map(|f| f.function)
    }
    #[cfg(feature = "wasm-adapter")]
    pub(crate) fn state_key(&self) -> i64 {
        self.frames
            .last()
            .map_or(-1, |f| ((f.function as u64) << 32 | f.pc as u64) as i64)
    }
    /// Scalar accessors for native lowering. Called only for compiler-validated registers.
    #[cfg(feature = "wasm-adapter")]
    pub(crate) fn scalar_bits(&self, r: Reg) -> i64 {
        match self.register(r) {
            Value::Integer(n) => n,
            Value::Real(n) => n.to_bits() as i64,
            Value::Boolean(b) => i64::from(b),
            _ => 0,
        }
    }
    #[cfg(feature = "wasm-adapter")]
    pub(crate) fn native_result(&mut self, r: Reg, bits: i64, overflow: bool) {
        let span = self.current_span().unwrap_or_default();
        self.record_work(1, self.execution_engine);
        if overflow {
            self.fail(ArithmeticError::IntegerOverflow, span);
            return;
        }
        let frame = self.frames.last().unwrap();
        let ty = &self.program.functions[frame.function].registers[r];
        let v = match ty {
            Type::Integer => Value::Integer(bits),
            Type::Real => Value::Real(f64::from_bits(bits as u64)),
            Type::Boolean => Value::Boolean(bits != 0),
            _ => unreachable!(),
        };
        self.set(r, v);
        self.frames.last_mut().unwrap().pc += 1;
    }
}
fn remap_functions(value: &mut Value, remap: &[usize]) {
    match value {
        Value::GeneratorStep(
            cyallo_values::GeneratorStep::Yielded(v) | cyallo_values::GeneratorStep::Complete(v),
        )
        | Value::Optional(Some(v))
        | Value::Erased(_, v)
        | Value::StoragePath(v, _) => remap_functions(v, remap),
        Value::Function(id, captures) => {
            *id = remap[*id];
            for v in captures {
                remap_functions(v, remap);
            }
        }
        Value::ArraySnapshot(values) => {
            for v in values {
                remap_functions(v, remap);
            }
        }
        Value::SparseSnapshot(values, _) => {
            for v in values.values_mut() {
                remap_functions(v, remap);
            }
        }
        _ => {}
    }
}
fn remap_service_keys(value: &mut Value, remap: &[usize]) {
    match value {
        Value::GeneratorStep(
            cyallo_values::GeneratorStep::Yielded(v) | cyallo_values::GeneratorStep::Complete(v),
        )
        | Value::Optional(Some(v))
        | Value::Erased(_, v)
        | Value::StoragePath(v, _) => remap_service_keys(v, remap),
        Value::ServiceKey(key) | Value::ServiceRef(_, key) => *key = remap[*key],
        Value::Function(_, values) | Value::ArraySnapshot(values) => {
            for v in values {
                remap_service_keys(v, remap);
            }
        }
        Value::SparseSnapshot(values, _) => {
            for v in values.values_mut() {
                remap_service_keys(v, remap);
            }
        }
        _ => {}
    }
}
fn coerce_assignment(ty: &Type, value: Value) -> Value {
    match (ty, value) {
        (Type::Real, Value::Integer(n)) => Value::Real(n as f64),
        (_, value) => value,
    }
}

fn integer(v: Value) -> RunResult<i64> {
    let number = v
        .as_number()
        .filter(|n| !n.kind.float())
        .ok_or(expected(ValueKind::Integer))?;
    i64::try_from(number.int()).map_err(|_| cyallo_ast::NumberError::OutOfRange.into())
}
fn real(v: Value) -> RunResult<f64> {
    match v {
        Value::Integer(n) => Ok(n as f64),
        Value::Real(n) => Ok(n),
        Value::Number(n) => Ok(n.real()),
        _ => Err(expected(ValueKind::Number)),
    }
}
fn boolean(v: Value) -> RunResult<bool> {
    if let Value::Boolean(b) = v {
        Ok(b)
    } else {
        Err(expected(ValueKind::Boolean))
    }
}
fn unary(op: cyallo_ast::Operator, v: Value) -> RunResult<Value> {
    match (op, v) {
        (Operator::Subtract, Value::Number(n)) => n
            .negate()
            .map(Value::from_number)
            .map_err(RuntimeError::from),
        (Operator::Not, Value::Boolean(b)) => Ok(Value::Boolean(!b)),
        (Operator::Subtract, Value::Integer(n)) => n
            .checked_neg()
            .map(Value::Integer)
            .ok_or(ArithmeticError::IntegerOverflow.into()),
        (Operator::Subtract, Value::Real(n)) => Ok(Value::Real(-n)),
        (Operator::Add, v) => Ok(v),
        _ => Err(Invariant::UnsupportedOperation.into()),
    }
}
fn binary(op: cyallo_ast::Operator, a: Value, b: Value) -> RunResult<Value> {
    if matches!(a, Value::Number(_)) || matches!(b, Value::Number(_)) {
        let x = a.as_number().ok_or(expected(ValueKind::Number))?;
        let y = b.as_number().ok_or(expected(ValueKind::Number))?;
        return if matches!(
            op,
            Operator::Equal
                | Operator::NotEqual
                | Operator::Less
                | Operator::Greater
                | Operator::LessEqual
                | Operator::GreaterEqual
        ) {
            x.compare(y, op)
                .map(Value::Boolean)
                .map_err(RuntimeError::from)
        } else {
            x.binary(y, op)
                .map(Value::from_number)
                .map_err(RuntimeError::from)
        };
    }
    if let (Value::Integer(x), Value::Integer(y)) = (&a, &b) {
        let n = match op {
            Operator::Add => Some(x.checked_add(*y)),
            Operator::Subtract => Some(x.checked_sub(*y)),
            Operator::Multiply => Some(x.checked_mul(*y)),
            Operator::IntegerDivide | Operator::Divide => Some(x.checked_div(*y)),
            Operator::Remainder => Some(if *x == i64::MIN && *y == -1 {
                Some(0)
            } else {
                x.checked_rem(*y)
            }),
            _ => None,
        };
        if let Some(n) = n {
            return n.map(Value::Integer).ok_or_else(|| {
                if *y == 0
                    && matches!(
                        op,
                        Operator::IntegerDivide | Operator::Divide | Operator::Remainder
                    )
                {
                    ArithmeticError::DivisionByZero.into()
                } else {
                    ArithmeticError::IntegerOverflow.into()
                }
            });
        }
        let cmp = match op {
            Operator::Equal => Some(x == y),
            Operator::NotEqual => Some(x != y),
            Operator::Less => Some(x < y),
            Operator::Greater => Some(x > y),
            Operator::LessEqual => Some(x <= y),
            Operator::GreaterEqual => Some(x >= y),
            _ => None,
        };
        if let Some(b) = cmp {
            return Ok(Value::Boolean(b));
        }
    }
    if op == Operator::Concat {
        return Ok(Value::String(format!("{a}{b}").into()));
    }
    if matches!(a, Value::Integer(_) | Value::Real(_))
        && matches!(b, Value::Integer(_) | Value::Real(_))
    {
        let x = real(a)?;
        let y = real(b)?;
        return Ok(match op {
            Operator::Add => Value::Real(x + y),
            Operator::Subtract => Value::Real(x - y),
            Operator::Multiply => Value::Real(x * y),
            // IEEE: a zero divisor yields an infinity or NaN.
            Operator::Divide => Value::Real(x / y),
            Operator::Remainder => Value::Real(x % y),
            Operator::RealDivide => {
                if y == 0.0 {
                    return Err(ArithmeticError::DivisionByZero.into());
                }
                Value::Real(x / y)
            }
            Operator::Equal => Value::Boolean(x == y),
            Operator::NotEqual => Value::Boolean(x != y),
            Operator::Less => Value::Boolean(x < y),
            Operator::Greater => Value::Boolean(x > y),
            Operator::LessEqual => Value::Boolean(x <= y),
            Operator::GreaterEqual => Value::Boolean(x >= y),
            _ => return Err(Invariant::UnsupportedOperation.into()),
        });
    }
    let ordering = match (&a, &b) {
        (Value::String(x), Value::String(y)) => Some(x.cmp(y)),
        (Value::Char(x), Value::Char(y)) => Some(x.cmp(y)),
        _ => None,
    };
    Ok(Value::Boolean(match op {
        Operator::Equal => a == b,
        Operator::NotEqual => a != b,
        Operator::Less => ordering.is_some_and(|o| o.is_lt()),
        Operator::Greater => ordering.is_some_and(|o| o.is_gt()),
        Operator::LessEqual => ordering.is_some_and(|o| o.is_le()),
        Operator::GreaterEqual => ordering.is_some_and(|o| o.is_ge()),
        _ => return Err(Invariant::UnsupportedOperation.into()),
    }))
}
fn text(v: &Value) -> RunResult<&str> {
    if let Value::String(s) = v {
        Ok(s)
    } else {
        Err(expected(ValueKind::String))
    }
}
fn builtin(
    n: cyallo_ir::intrinsic::Intrinsic,
    args: Vec<Value>,
    seed: &mut u64,
    limit: usize,
) -> RunResult<Value> {
    if let Some(kind) = n.cast_kind() {
        return args[0]
            .as_number()
            .ok_or(expected(ValueKind::Number))?
            .cast(kind)
            .map(Value::from_number)
            .map_err(RuntimeError::from);
    }
    Ok(match n {
        cyallo_ir::intrinsic::Intrinsic::Length
        | cyallo_ir::intrinsic::Intrinsic::ArrayLength
        | cyallo_ir::intrinsic::Intrinsic::StringLength => Value::Integer(match &args[0] {
            Value::SparseSnapshot(_, length) => *length as i64,
            Value::ArraySnapshot(values) => values.len() as i64,
            Value::Slice { length, .. } => *length as i64,
            value => text(value)?.chars().count() as i64,
        }),
        cyallo_ir::intrinsic::Intrinsic::Left
        | cyallo_ir::intrinsic::Intrinsic::Right
        | cyallo_ir::intrinsic::Intrinsic::Mid => {
            let chars: Vec<_> = text(&args[0])?.chars().collect();
            let (start, len) = if n == cyallo_ir::intrinsic::Intrinsic::Mid {
                let start = integer(args[1].clone())?
                    .checked_sub(1)
                    .ok_or(TextError::IndexOutOfBounds)?;
                (
                    usize::try_from(start).map_err(|_| TextError::IndexOutOfBounds)?,
                    usize::try_from(integer(args[2].clone())?)
                        .map_err(|_| TextError::NegativeLength)?,
                )
            } else {
                let len = usize::try_from(integer(args[1].clone())?)
                    .map_err(|_| TextError::NegativeLength)?;
                let start = if n == cyallo_ir::intrinsic::Intrinsic::Right {
                    chars
                        .len()
                        .checked_sub(len)
                        .ok_or(TextError::RangeOutOfBounds)?
                } else {
                    0
                };
                (start, len)
            };
            let end = start.checked_add(len).ok_or(TextError::RangeOutOfBounds)?;
            Value::String(
                chars
                    .get(start..end)
                    .ok_or(TextError::RangeOutOfBounds)?
                    .iter()
                    .collect::<String>()
                    .into(),
            )
        }
        cyallo_ir::intrinsic::Intrinsic::LowerChar | cyallo_ir::intrinsic::Intrinsic::UpperChar => {
            let Value::Char(c) = args[0] else {
                return Err(expected(ValueKind::Char));
            };
            let s = if n == cyallo_ir::intrinsic::Intrinsic::LowerChar {
                c.to_lowercase().collect::<String>()
            } else {
                c.to_uppercase().collect::<String>()
            };
            let mut cs = s.chars();
            let c = cs.next().unwrap();
            if cs.next().is_some() {
                return Err(TextError::CaseConversionNotSingleChar.into());
            }
            Value::Char(c)
        }
        cyallo_ir::intrinsic::Intrinsic::CharCode => {
            let Value::Char(c) = args[0] else {
                return Err(expected(ValueKind::Char));
            };
            Value::Integer(c as i64)
        }
        cyallo_ir::intrinsic::Intrinsic::CharFromCode => Value::Char(
            u32::try_from(integer(args[0].clone())?)
                .ok()
                .and_then(char::from_u32)
                .ok_or(TextError::InvalidScalar)?,
        ),
        cyallo_ir::intrinsic::Intrinsic::Truncate => {
            if let Value::Integer(n) = args[0] {
                return Ok(Value::Integer(n));
            }
            let x = real(args[0].clone())?.trunc();
            if !x.is_finite() || x < i64::MIN as f64 || x >= -(i64::MIN as f64) {
                return Err(ArithmeticError::TruncationOutOfRange.into());
            }
            Value::Integer(x as i64)
        }
        cyallo_ir::intrinsic::Intrinsic::StringFrom => Value::String(args[0].to_string().into()),
        cyallo_ir::intrinsic::Intrinsic::ParseLegacy => Value::Real(
            text(&args[0])?
                .parse::<f64>()
                .ok()
                .filter(|x| x.is_finite())
                .ok_or(TextError::InvalidNumber)?,
        ),
        cyallo_ir::intrinsic::Intrinsic::IsNumeric => {
            Value::Boolean(text(&args[0])?.parse::<f64>().is_ok_and(|x| x.is_finite()))
        }
        cyallo_ir::intrinsic::Intrinsic::Random => {
            let x = real(args[0].clone())?;
            if !x.is_finite() || x <= 0.0 {
                return Err(ArithmeticError::RandomBound.into());
            }
            *seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let value = (*seed >> 11) as f64 / 9007199254740992.0 * x;
            Value::Real(if value < x {
                value
            } else {
                f64::from_bits(x.to_bits() - 1)
            })
        }
        _ => return utilities::call(n, args, limit),
    })
}
fn parse_input(s: &str, ty: &Type) -> RunResult<Value> {
    if let Type::Numeric(kind) = ty {
        return cyallo_ast::Number::parse(*kind, s.trim())
            .map(Value::from_number)
            .map_err(RuntimeError::from);
    }
    match ty {
        Type::String => Ok(Value::String(s.into())),
        Type::Char => {
            let mut cs = s.chars();
            let c = cs.next().ok_or(InputError::EmptyChar)?;
            if cs.next().is_some() {
                return Err(InputError::MultipleChars.into());
            }
            Ok(Value::Char(c))
        }
        Type::Integer => s
            .trim()
            .parse()
            .map(Value::Integer)
            .map_err(|_| InputError::Integer.into()),
        Type::Real => s
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .map(Value::Real)
            .ok_or(InputError::Real.into()),
        Type::Boolean => match s.trim().to_ascii_uppercase().as_str() {
            "TRUE" => Ok(Value::Boolean(true)),
            "FALSE" => Ok(Value::Boolean(false)),
            _ => Err(InputError::Boolean.into()),
        },
        _ => Err(InputError::UnsupportedType.into()),
    }
}
