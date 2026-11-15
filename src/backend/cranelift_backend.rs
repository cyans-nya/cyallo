use super::{
    CompileError, Executable, NativeBackend,
    abi::{native_enter, native_flush, native_property_write, native_step},
};
use crate::{
    Type,
    ir::{Op, Program},
    metrics::ExecutionEngine,
    runtime::{
        EngineError, Session,
        native::{self, ExitReason, NativeFault, NativeSlot, NativeView},
    },
};
use cranelift::{
    codegen::ir::{FuncRef, MemFlagsData, SigRef},
    frontend::Switch,
    prelude::*,
};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module, default_libcall_names};
use cyallo_ast::Literal;
use cyallo_ast::Operator;
use std::{collections::HashMap, mem::offset_of, sync::Arc};

/// Eager compilation with low-latency Cranelift settings.
pub struct CraneliftBackend;
/// Eager compilation with Cranelift's speed optimizations for every routine.
pub struct CraneliftSpeedBackend;
type Entry = unsafe extern "C" fn(*mut Session, *mut NativeView, u64) -> u64;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptimizationLevel {
    None,
    SpeedAndSize,
    Speed,
}
impl OptimizationLevel {
    fn flag(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::SpeedAndSize => "speed_and_size",
            Self::Speed => "speed",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompilationScope {
    Combined,
    PerRoutine,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeStrategy {
    AllPcs,
    LoopHeaders,
}
/// Code-generation options. Optimization is independent of dispatch scope and
/// resume entries, so configurations can be compared directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CraneliftConfig {
    pub optimization: OptimizationLevel,
    pub scope: CompilationScope,
    pub resume: ResumeStrategy,
    pub backtracking: bool,
    pub inlining: bool,
    /// Retain real frames while embedding effectful procedure bodies. Opt-in:
    /// measured call setup dominates, so code growth is not generally repaid.
    pub procedure_inlining: bool,
}
impl Default for CraneliftConfig {
    fn default() -> Self {
        Self::fast()
    }
}
impl CraneliftConfig {
    /// Eager fast settings: unoptimized codegen over linked all-PC routines.
    pub const fn fast() -> Self {
        Self {
            optimization: OptimizationLevel::None,
            scope: CompilationScope::PerRoutine,
            resume: ResumeStrategy::AllPcs,
            backtracking: true,
            inlining: true,
            procedure_inlining: false,
        }
    }
    /// Eager speed settings. Like fast, uses linked all-PC routines.
    pub const fn speed() -> Self {
        Self {
            optimization: OptimizationLevel::Speed,
            ..Self::fast()
        }
    }
    /// Per-routine settings used by independently replaceable JIT tiers.
    pub fn tiered(speed: bool) -> Self {
        Self {
            scope: CompilationScope::PerRoutine,
            resume: if speed {
                ResumeStrategy::LoopHeaders
            } else {
                ResumeStrategy::AllPcs
            },
            ..if speed { Self::speed() } else { Self::fast() }
        }
    }
    fn flags(self) -> [(&'static str, &'static str); 2] {
        [
            ("opt_level", self.optimization.flag()),
            (
                "regalloc_algorithm",
                if self.backtracking {
                    "backtracking"
                } else {
                    "single_pass"
                },
            ),
        ]
    }
}
pub(super) struct CraneliftUnit {
    program: Arc<Program>,
    module: Option<JITModule>,
    entry: Entry,
    engine: ExecutionEngine,
    function: Option<usize>,
    version: u64,
    continuation: Option<Box<CraneliftUnit>>,
    config: CraneliftConfig,
    code_bytes: usize,
}
impl CraneliftUnit {
    fn code_size(&self) -> usize {
        self.code_bytes
            + self
                .continuation
                .as_ref()
                .map_or(0, |unit| unit.code_size())
    }
    pub(super) fn entry_address(&self) -> usize {
        self.entry as usize
    }
    pub(super) fn owns_speed_frame(&self, session: &Session) -> bool {
        self.engine == ExecutionEngine::CraneliftSpeed
            && session
                .frames
                .last()
                .is_some_and(|f| f.speed_version == self.version)
    }
    fn invoke(&self, session: &mut Session, budget: usize) -> u64 {
        session.enter_execution();
        if !session.poll_concurrent() {
            session.leave_execution();
            return budget as u64;
        }
        let previous = session.select_execution_engine(self.engine);
        session.reset_native_exit();
        let view = session.prepare_native();
        // SAFETY: the exclusively borrowed session/view and code owner remain
        // pinned through this call, including its native continuation entries.
        let remaining = unsafe { (self.entry)(session, view, budget as u64) };
        session.finish_native();
        session.observe_native_exit();
        session.pause_reporting();
        session.select_execution_engine(previous);
        session.leave_execution();
        remaining
    }
}
impl Executable for CraneliftUnit {
    fn run(&self, session: &mut Session, budget: usize) -> Result<(), EngineError> {
        if !Arc::ptr_eq(&self.program, &session.program) {
            if session.program.extends(&self.program) {
                session.run(budget);
                return Ok(());
            }

            self.program.notice(
                "invalidation",
                0,
                "stale Program artifact rejected before execution; no frame reconstruction",
            );
            return Err(EngineError::ForeignProgram);
        }
        if budget == 0 {
            return Ok(());
        }
        let mut budget = budget;
        // Selective JIT units bridge arbitrary PCs through the lower tier or
        // their native continuation. Eager defaults accept every PC directly.
        if self.config.resume == ResumeStrategy::LoopHeaders {
            while budget > 0 {
                let Some(frame) = session.frames.last() else {
                    return Ok(());
                };
                if self.function.is_some_and(|fi| fi != frame.function) {
                    return Ok(());
                }
                if self.program.functions[frame.function]
                    .resume
                    .accepts(frame.pc)
                {
                    break;
                }
                if self.owns_speed_frame(session)
                    && let Some(continuation) = &self.continuation
                {
                    let remaining = continuation.invoke(session, budget);
                    if session.native_exit_reason() != Some(ExitReason::Continuation) {
                        return Ok(());
                    }
                    budget = remaining as usize;
                    continue;
                }
                if !session.step() {
                    session.pause_reporting();
                    return Ok(());
                }
                budget -= 1;
            }
            session.pause_reporting();
            if budget == 0 {
                return Ok(());
            }
        }
        self.invoke(session, budget);
        Ok(())
    }
}
impl Drop for CraneliftUnit {
    fn drop(&mut self) {
        if let Some(module) = self.module.take() {
            // SAFETY: private entries cannot outlive the artifact/table borrow.
            unsafe {
                module.free_memory();
            }
        }
    }
}
fn import(
    module: &mut JITModule,
    name: &str,
    ptr: cranelift::prelude::Type,
) -> Result<cranelift_module::FuncId, CompileError> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(ptr));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(CompileError::from)
}
impl NativeBackend for CraneliftBackend {
    type Artifact = CraneliftArtifact;
    fn compile(&self, program: Arc<Program>) -> Result<Self::Artifact, CompileError> {
        CraneliftConfig::fast().compile(program)
    }
}
impl NativeBackend for CraneliftSpeedBackend {
    type Artifact = EagerSpeedArtifact;
    fn compile(&self, program: Arc<Program>) -> Result<Self::Artifact, CompileError> {
        CraneliftConfig::speed().compile(program)
    }
}
impl NativeBackend for CraneliftConfig {
    type Artifact = EagerCraneliftArtifact;
    fn compile(&self, program: Arc<Program>) -> Result<Self::Artifact, CompileError> {
        program.validate_all()?;
        let units = match self.scope {
            CompilationScope::Combined => {
                vec![Some(compile_configured(program.clone(), None, *self)?)]
            }
            CompilationScope::PerRoutine => program
                .codegen_jobs()
                .map(&program.functions, 4, |_, fi, routine| {
                    if routine.attributes.compilation == cyallo_ast::CompileHint::AlwaysLazy {
                        Ok(None)
                    } else {
                        compile_configured(program.clone(), Some(fi), *self).map(Some)
                    }
                })
                .into_iter()
                .collect::<Result<Vec<_>, CompileError>>()?,
        };
        let entries = units
            .iter()
            .map(|unit| unit.as_ref().map_or(0, |unit| unit.entry_address()))
            .collect();
        Ok(EagerCraneliftArtifact {
            program,
            units,
            entries,
            config: *self,
        })
    }
}
/// An eager artifact with either combined dispatch or linked routine entries.
pub struct EagerCraneliftArtifact {
    program: Arc<Program>,
    units: Vec<Option<CraneliftUnit>>,
    entries: Box<[usize]>,
    config: CraneliftConfig,
}
/// Compatibility name for the explicit speed backend's artifact.
pub type EagerSpeedArtifact = EagerCraneliftArtifact;
/// Compatibility name for the explicit fast backend's artifact.
pub type CraneliftArtifact = EagerCraneliftArtifact;
impl EagerCraneliftArtifact {
    pub fn configuration(&self) -> CraneliftConfig {
        self.config
    }
    pub fn code_size(&self) -> usize {
        self.units
            .iter()
            .flatten()
            .map(CraneliftUnit::code_size)
            .sum()
    }
}
impl Executable for EagerCraneliftArtifact {
    fn run(&self, session: &mut Session, budget: usize) -> Result<(), EngineError> {
        if !Arc::ptr_eq(&self.program, &session.program) {
            if session.program.extends(&self.program) {
                session.run(budget);
                return Ok(());
            }

            self.program.notice(
                "invalidation",
                0,
                "stale Program artifact rejected before execution; no frame reconstruction",
            );
            return Err(EngineError::ForeignProgram);
        }
        let start = session.instruction_count();
        session.bind_native_entries(if self.config.scope == CompilationScope::PerRoutine {
            self.entries.as_ptr()
        } else {
            std::ptr::null()
        });
        let result = (|| {
            while session.instruction_count() - start < budget as u64 {
                if !session.resume_pending() {
                    break;
                }
                if matches!(
                    session.status(),
                    crate::Status::Finished | crate::Status::Failed(_)
                ) || (*session.status() == crate::Status::WaitingForInput
                    && session.context.input.is_empty())
                {
                    break;
                }
                let Some(fi) = session.active_function() else {
                    break;
                };
                let before = session.instruction_count();
                let unit = if self.config.scope == CompilationScope::Combined {
                    0
                } else {
                    fi
                };
                if self.program.functions[fi].attributes.compilation
                    == cyallo_ast::CompileHint::AlwaysLazy
                {
                    session.step();
                } else if let Some(unit) = &self.units[unit] {
                    unit.run(session, (budget as u64 - (before - start)) as usize)?;
                }
                if before == session.instruction_count() {
                    return Err(EngineError::NoProgress);
                }
            }
            Ok(())
        })();
        session.bind_native_entries(std::ptr::null());
        result
    }
}
fn scalar(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Integer
            | Type::Real
            | Type::Boolean
            | Type::Char
            | Type::Context
            | Type::Registration
            | Type::ServiceKey(_)
            | Type::ServiceRef(_)
            | Type::TraitObject(_)
            | Type::Object(_)
    )
}
fn kind(ty: &Type) -> u64 {
    match ty {
        Type::Integer => native::INTEGER,
        Type::Real => native::REAL,
        Type::Boolean => native::BOOLEAN,
        Type::Char => native::CHAR,
        Type::Context => native::CONTEXT,
        Type::Registration => native::REGISTRATION,
        Type::ServiceKey(_) => native::SERVICE_KEY,
        Type::ServiceRef(_) => native::SERVICE_REF,
        Type::TraitObject(_) => native::TRAIT_OBJECT,
        Type::Object(_) => native::OBJECT,
        _ => unreachable!(),
    }
}

/// Values in slots are always recoverable. Only fuel and the pending instruction
/// count live in SSA across bytecode edges. All pointers are reloaded after Rust.
struct Lower<'a, 'b> {
    b: FunctionBuilder<'a>,
    program: &'b Program,
    session: cranelift::prelude::Value,
    view: cranelift::prelude::Value,
    depth: cranelift::prelude::Value,
    done: Block,
    targets: HashMap<(usize, usize), Block>,
    step: FuncRef,
    flush: FuncRef,
    property_write: FuncRef,
    signature: SigRef,
    inline_plans: HashMap<(usize, usize), super::inlining::Plan>,
}
impl Lower<'_, '_> {
    fn property_ic(
        &mut self,
        fi: usize,
        pc: usize,
        op: &Op,
        fuel: cranelift::prelude::Value,
        pending: cranelift::prelude::Value,
    ) {
        use crate::runtime::dynamic::{NativeCache, NativeCase, NativeObject, ObjectStats};
        use cyallo_ir::dynamic::Operation as D;
        let Op::Dynamic(destination, operation, args) = op else {
            return;
        };
        if !matches!(operation, D::Get(_) | D::GetKey | D::Set(_) | D::SetKey) {
            return;
        }
        let register = if matches!(operation, D::Set(_) | D::SetKey) {
            *args.last().unwrap()
        } else {
            *destination
        };
        if !native::property_scalar_candidate(&self.program.functions[fi].registers[register]) {
            return;
        }
        let write = matches!(operation, D::Set(_) | D::SetKey);
        let computed = matches!(operation, D::GetKey | D::SetKey);
        let miss = self.b.create_block();
        let guard = |this: &mut Self, condition| {
            let valid = this.b.create_block();
            this.b.ins().brif(condition, valid, &[], miss, &[]);
            this.b.switch_to_block(valid);
        };
        let table = self.field(offset_of!(NativeView, property_caches));
        let present = self.b.ins().icmp_imm_s(IntCC::NotEqual, table, 0);
        guard(self, present);
        let cache = self
            .b
            .ins()
            .iadd_imm_s(table, (pc * size_of::<NativeCache>()) as i64);
        let receiver = self.regptr(args[0]);
        let kind = self.load(receiver, 8);
        let kind = self.b.ins().band_imm_s(kind, 255);
        let bare = self
            .b
            .ins()
            .icmp_imm_s(IntCC::Equal, kind, native::OBJECT as i64);
        let erased = self
            .b
            .ins()
            .icmp_imm_s(IntCC::Equal, kind, native::DYNAMIC_OBJECT as i64);
        let valid = self.b.ins().bor(bare, erased);
        guard(self, valid);
        if computed {
            let key = self.regptr(args[1]);
            let kind = self.load(key, 8);
            let kind = self.b.ins().band_imm_s(kind, 255);
            let bare = self
                .b
                .ins()
                .icmp_imm_s(IntCC::Equal, kind, native::STRING_KEY as i64);
            let erased =
                self.b
                    .ins()
                    .icmp_imm_s(IntCC::Equal, kind, native::DYNAMIC_STRING_KEY as i64);
            let valid = self.b.ins().bor(bare, erased);
            guard(self, valid);
            let key = self.load(key, 0);
            let expected = self.load(cache, offset_of!(NativeCache, key));
            let equal = self.b.ins().icmp(IntCC::Equal, key, expected);
            guard(self, equal);
        }
        let id = self.load(receiver, 0);
        let count = self.field(offset_of!(NativeView, object_count));
        let in_bounds = self.b.ins().icmp(IntCC::UnsignedLessThan, id, count);
        guard(self, in_bounds);
        let table = self.field(offset_of!(NativeView, objects));
        let offset = self.b.ins().ishl_imm_s(id, 3);
        let address = self.b.ins().iadd(table, offset);
        let object = self.load(address, 0);
        let present = self.b.ins().icmp_imm_s(IntCC::NotEqual, object, 0);
        guard(self, present);
        let shape = self.load(object, offset_of!(NativeObject, shape));
        for index in 0..4 {
            let next = self.b.create_block();
            let hit = self.b.create_block();
            let case = self.b.ins().iadd_imm_s(
                cache,
                (offset_of!(NativeCache, cases) + index * size_of::<NativeCase>()) as i64,
            );
            let expected = self.load(case, offset_of!(NativeCase, shape));
            let equal = self.b.ins().icmp(IntCC::Equal, shape, expected);
            self.b.ins().brif(equal, hit, &[], next, &[]);
            self.b.switch_to_block(hit);
            let slot = self.load(case, offset_of!(NativeCase, slot));
            let length = self.load(object, offset_of!(NativeObject, length));
            let valid = self.b.ins().icmp(IntCC::UnsignedLessThan, slot, length);
            guard(self, valid);
            let slots = self.load(object, offset_of!(NativeObject, slots));
            let offset = self.b.ins().ishl_imm_s(slot, 4);
            let value = self.b.ins().iadd(slots, offset);
            let kind = self.load(value, 8);
            let expected = self.load(case, offset_of!(NativeCase, kind));
            let equal = self.b.ins().icmp(IntCC::Equal, kind, expected);
            guard(self, equal);
            if write {
                let input = self.regptr(*args.last().unwrap());
                let kind = self.load(input, 8);
                let kind = self.b.ins().band_imm_s(kind, 255);
                let expected = self.load(case, offset_of!(NativeCase, input_kind));
                let valid = self.b.ins().icmp_imm_s(IntCC::NotEqual, expected, 0);
                let equal = self.b.ins().icmp(IntCC::Equal, kind, expected);
                let valid = self.b.ins().band(valid, equal);
                guard(self, valid);
                self.store(self.view, offset_of!(NativeView, property_object), id);
                self.store(self.view, offset_of!(NativeView, property_slot), slot);
                let input = self.constant(*args.last().unwrap() as u64);
                self.store(self.view, offset_of!(NativeView, property_input), input);
                let result = self.constant(*destination as u64);
                self.store(
                    self.view,
                    offset_of!(NativeView, property_destination),
                    result,
                );
                let before = self.b.ins().iadd_imm_s(pending, -1);
                self.save(pc, before);
                self.b.ins().call(self.property_write, &[self.session]);
                let halted = self.field(offset_of!(NativeView, halted));
                let active = self.b.ins().icmp_imm_s(IntCC::Equal, halted, 0);
                let zero = self.constant(0);
                self.b.ins().brif(
                    active,
                    self.targets[&(fi, pc + 1)],
                    &[fuel.into(), zero.into()],
                    self.done,
                    &[fuel.into()],
                );
            } else {
                let result_kind = self.load(case, offset_of!(NativeCase, result_kind));
                let valid = self.b.ins().icmp_imm_s(IntCC::NotEqual, result_kind, 0);
                guard(self, valid);
                let bits = self.load(value, 0);
                self.write(*destination, bits, result_kind);
                let stats = self.field(offset_of!(NativeView, object_stats));
                self.saturating_add(stats, offset_of!(ObjectStats, native_reads), 1);
                self.next(fi, pc + 1, fuel, pending);
            }
            self.b.switch_to_block(next);
        }
        self.b.ins().jump(miss, &[]);
        self.b.switch_to_block(miss);
        let stats = self.field(offset_of!(NativeView, object_stats));
        self.saturating_add(stats, offset_of!(ObjectStats, native_misses), 1);
    }
    fn inline_guard(
        &mut self,
        safe: cranelift::prelude::Value,
        good: Block,
        fallback: Block,
        reason: Option<cranelift::prelude::Value>,
        site: (usize, usize),
        target: usize,
    ) {
        if let Some(reason) = reason {
            let failed = self.b.create_block();
            self.b.ins().brif(safe, good, &[], failed, &[]);
            self.b.switch_to_block(failed);
            self.store(
                self.view,
                offset_of!(NativeView, inline_exit_reason),
                reason,
            );
            let caller = self.constant(site.0 as u64);
            self.store(
                self.view,
                offset_of!(NativeView, inline_exit_function),
                caller,
            );
            let pc = self.constant(site.1 as u64);
            self.store(self.view, offset_of!(NativeView, inline_exit_pc), pc);
            let target = self.constant(target as u64);
            self.store(
                self.view,
                offset_of!(NativeView, inline_exit_target),
                target,
            );
            self.b.ins().jump(fallback, &[]);
        } else {
            self.b.ins().brif(safe, good, &[], fallback, &[]);
        }
        self.b.switch_to_block(good);
    }
    fn saturating_add(&mut self, base: cranelift::prelude::Value, offset: usize, amount: u64) {
        let old = self.load(base, offset);
        let amount = self.constant(amount);
        let (sum, overflow) = self.b.ins().uadd_overflow(old, amount);
        let max = self.constant(u64::MAX);
        let sum = self.b.ins().select(overflow, max, sum);
        self.store(base, offset, sum);
    }
    fn inline_body(
        &mut self,
        plan: &super::inlining::Plan,
        args: &[cranelift::prelude::Value],
        bad: &mut cranelift::prelude::Value,
    ) -> Option<cranelift::prelude::Value> {
        let f = &self.program.functions[plan.function];
        let mut values = vec![None; f.registers.len()];
        for (storage, value) in f.local_storage.iter().zip(args) {
            let crate::ir::LocalStorage::Register(r) = storage else {
                unreachable!()
            };
            values[*r] = Some(*value);
        }
        for (pc, op) in plan.body.iter().enumerate() {
            let (destination, value) = match op {
                Op::Statement | Op::Nop => continue,
                Op::Literal(r, Literal::Integer(n)) => (*r, self.constant(*n as u64)),
                Op::Literal(r, Literal::Boolean(n)) => (*r, self.constant(u64::from(*n))),
                Op::FrameLoad(r, a) | Op::FrameStore(r, a) | Op::Move(r, a) => {
                    (*r, values[*a].unwrap())
                }
                Op::Unary(r, op, a) => {
                    let a = values[*a].unwrap();
                    let value = match *op {
                        Operator::Subtract => {
                            let zero = self.constant(0);
                            let (value, overflow) = self.b.ins().ssub_overflow(zero, a);
                            *bad = self.b.ins().bor(*bad, overflow);
                            value
                        }
                        Operator::Not => self.b.ins().bxor_imm_s(a, 1),
                        _ => a,
                    };
                    (*r, value)
                }
                Op::Binary(r, op, a, b) => {
                    let (a, b) = (values[*a].unwrap(), values[*b].unwrap());
                    let value = match *op {
                        Operator::Add | Operator::Subtract | Operator::Multiply => {
                            let (value, overflow) = match *op {
                                Operator::Add => self.b.ins().sadd_overflow(a, b),
                                Operator::Subtract => self.b.ins().ssub_overflow(a, b),
                                _ => self.b.ins().smul_overflow(a, b),
                            };
                            *bad = self.b.ins().bor(*bad, overflow);
                            value
                        }
                        _ => {
                            let cc = match *op {
                                Operator::Equal => IntCC::Equal,
                                Operator::NotEqual => IntCC::NotEqual,
                                Operator::Less => IntCC::SignedLessThan,
                                Operator::Greater => IntCC::SignedGreaterThan,
                                Operator::LessEqual => IntCC::SignedLessThanOrEqual,
                                Operator::GreaterEqual => IntCC::SignedGreaterThanOrEqual,
                                _ => unreachable!(),
                            };
                            let result = self.b.ins().icmp(cc, a, b);
                            self.b.ins().uextend(types::I64, result)
                        }
                    };
                    (*r, value)
                }
                Op::Call(result, _, args) => {
                    let args: Vec<_> = args.iter().map(|r| values[*r].unwrap()).collect();
                    let value = self.inline_body(&plan.children[&pc], &args, bad);
                    if let Some(r) = result {
                        (*r, value.unwrap())
                    } else {
                        continue;
                    }
                }
                Op::Return(result) => return result.map(|r| values[r].unwrap()),
                _ => unreachable!("planner admitted unsupported inline operation"),
            };
            values[destination] = Some(value);
        }
        unreachable!("inline plan has a return")
    }
    fn inline_call(
        &mut self,
        site: (usize, usize),
        result: Option<usize>,
        args: &[usize],
        plan: &super::inlining::Plan,
        fuel: cranelift::prelude::Value,
        pending: cranelift::prelude::Value,
    ) {
        let (fi, pc) = site;
        let fallback = self.b.create_block();
        let compute = self.b.create_block();
        let enough =
            self.b
                .ins()
                .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, fuel, plan.cost as i64);
        // Frameless bodies have no VM frame for the observer to enter, so trace
        // observation does not require the fallback. Their work is recorded
        // through the logical counters on flush.
        let depth = self.field(offset_of!(NativeView, depth));
        let needed = self.b.ins().iadd_imm_s(depth, plan.depth as i64);
        let max = self.field(offset_of!(NativeView, max_depth));
        let room = self
            .b
            .ins()
            .icmp(IntCC::UnsignedLessThanOrEqual, needed, max);
        let mut safe = self.b.ins().band(enough, room);
        let observing = self.program.inspection().is_some_and(|o| o.options.notices);
        let reason = observing.then(|| {
            let one = self.constant(1);
            let three = self.constant(3);
            let four = self.constant(4);
            let reason = self.b.ins().select(room, four, three);
            self.b.ins().select(enough, reason, one)
        });
        let callee = &self.program.functions[plan.function];
        let mut values = Vec::with_capacity(args.len());
        for (arg, local) in args.iter().zip(&callee.locals) {
            let ptr = self.regptr(*arg);
            let tag = self.load(ptr, offset_of!(NativeSlot, kind));
            let tag = self.b.ins().band_imm_s(tag, !native::DIRTY as i64);
            let typed = self
                .b
                .ins()
                .icmp_imm_s(IntCC::Equal, tag, kind(&local.ty) as i64);
            safe = self.b.ins().band(safe, typed);
            values.push(self.reg(*arg));
        }
        let check_tiers = self.b.create_block();
        self.inline_guard(safe, check_tiers, fallback, reason, site, plan.function);
        let mut accounting = std::collections::BTreeMap::new();
        plan.accounting(&mut accounting);
        let limits = self.field(offset_of!(NativeView, transition_limits));
        let tiered = self.b.ins().icmp_imm_s(IntCC::NotEqual, limits, 0);
        let check = self.b.create_block();
        self.b.ins().brif(tiered, check, &[], compute, &[]);
        self.b.switch_to_block(check);
        let work = self.field(offset_of!(NativeView, function_work));
        let mut warm = self.b.ins().iconst(types::I8, 1);
        for function in accounting.keys() {
            let count = self.load(work, function * 8);
            let limit = self.load(limits, function * 8);
            let below = self.b.ins().icmp(IntCC::UnsignedLessThan, count, limit);
            warm = self.b.ins().band(warm, below);
        }
        let reason = observing.then(|| self.constant(5));
        self.inline_guard(warm, compute, fallback, reason, site, plan.function);
        let mut bad = self.b.ins().iconst(types::I8, 0);
        let value = self.inline_body(plan, &values, &mut bad);
        let commit = self.b.create_block();
        let okay = self.b.ins().icmp_imm_s(IntCC::Equal, bad, 0);
        let reason = observing.then(|| self.constant(6));
        self.inline_guard(okay, commit, fallback, reason, site, plan.function);
        if let Some(r) = result {
            self.result(
                r,
                value.unwrap(),
                kind(&self.program.functions[fi].registers[r]),
            );
        }
        let work = self.field(offset_of!(NativeView, function_work));
        let calls = self.field(offset_of!(NativeView, function_calls));
        let mut entries = 0;
        for (function, (count, entered)) in accounting {
            self.saturating_add(work, function * 8, count);
            self.saturating_add(calls, function * 8, entered);
            entries += entered;
        }
        self.saturating_add(
            self.view,
            offset_of!(NativeView, inlined_pending),
            plan.cost,
        );
        self.saturating_add(self.view, offset_of!(NativeView, inlined_calls), entries);
        let fuel = self.b.ins().iadd_imm_s(fuel, -(plan.cost as i64));
        self.next(fi, pc + 1, fuel, pending);
        self.b.switch_to_block(fallback);
    }
    fn trait_target(&mut self, fi: usize, pc: usize, target: usize) {
        use crate::runtime::services::{CallCache, NativeBinding, TargetCase};
        let done = self.b.create_block();
        let bits = self.reg(target);
        let high = self.b.ins().ushr_imm_s(bits, 32);
        let implementation = if matches!(
            self.program.functions[fi].registers[target],
            Type::ServiceRef(_)
        ) {
            let table = self.field(offset_of!(NativeView, service_tables));
            let index = self.b.ins().ishl_imm_s(high, 3);
            let ptr = self.b.ins().iadd(table, index);
            let slots = self.load(ptr, 0);
            let key = self.b.ins().band_imm_s(bits, u32::MAX as i64);
            let offset = self.b.ins().ishl_imm_s(key, 5);
            let slot = self.b.ins().iadd(slots, offset);
            let available = self.load(slot, offset_of!(NativeBinding, available));
            let active = self.b.create_block();
            let test = self.b.ins().icmp_imm_s(IntCC::NotEqual, available, 0);
            self.b.ins().brif(test, active, &[], done, &[]);
            self.b.switch_to_block(active);
            self.load(slot, offset_of!(NativeBinding, implementation))
        } else {
            high
        };
        let implementation = self.b.ins().iadd_imm_s(implementation, 1);
        let caches = self.field(offset_of!(NativeView, call_caches));
        for i in 0..4 {
            let offset = pc * size_of::<CallCache>()
                + offset_of!(CallCache, cases)
                + i * size_of::<TargetCase>();
            let expected = self.load(caches, offset);
            let test = self.b.ins().icmp(IntCC::Equal, implementation, expected);
            let hit = self.b.create_block();
            let next = self.b.create_block();
            self.b.ins().brif(test, hit, &[], next, &[]);
            self.b.switch_to_block(hit);
            let target = self.load(caches, offset + 8);
            let hint = self.b.ins().iadd_imm_s(target, 1);
            self.store(self.view, offset_of!(NativeView, trait_hint_target), hint);
            self.b.ins().jump(done, &[]);
            self.b.switch_to_block(next);
        }
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
    }
    fn load(
        &mut self,
        base: cranelift::prelude::Value,
        offset: usize,
    ) -> cranelift::prelude::Value {
        self.b
            .ins()
            .load(types::I64, MemFlagsData::new(), base, offset as i32)
    }
    fn store(
        &mut self,
        base: cranelift::prelude::Value,
        offset: usize,
        value: cranelift::prelude::Value,
    ) {
        self.b
            .ins()
            .store(MemFlagsData::new(), value, base, offset as i32);
    }
    fn field(&mut self, offset: usize) -> cranelift::prelude::Value {
        self.load(self.view, offset)
    }
    fn constant(&mut self, n: u64) -> cranelift::prelude::Value {
        self.b.ins().iconst(types::I64, n as i64)
    }
    fn regptr(&mut self, r: usize) -> cranelift::prelude::Value {
        let regs = self.field(offset_of!(NativeView, registers));
        self.b
            .ins()
            .iadd_imm_s(regs, (r * size_of::<NativeSlot>()) as i64)
    }
    fn reg(&mut self, r: usize) -> cranelift::prelude::Value {
        let ptr = self.regptr(r);
        self.load(ptr, offset_of!(NativeSlot, bits))
    }
    fn write(
        &mut self,
        r: usize,
        value: cranelift::prelude::Value,
        tag: cranelift::prelude::Value,
    ) {
        let ptr = self.regptr(r);
        self.store(ptr, offset_of!(NativeSlot, bits), value);
        let tag = self.b.ins().bor_imm_s(tag, native::DIRTY as i64);
        self.store(ptr, offset_of!(NativeSlot, kind), tag);
    }
    fn result(&mut self, r: usize, value: cranelift::prelude::Value, tag: u64) {
        let tag = self.constant(tag);
        self.write(r, value, tag);
    }
    fn cell(&mut self, a: usize) -> (cranelift::prelude::Value, cranelift::prelude::Value) {
        let id = self.reg(a);
        let table = self.field(offset_of!(NativeView, cells));
        let offset = self.b.ins().ishl_imm_s(id, 3);
        let ptr = self.b.ins().iadd(table, offset);
        (id, self.load(ptr, 0))
    }
    fn save(&mut self, pc: usize, pending: cranelift::prelude::Value) {
        let pcptr = self.field(offset_of!(NativeView, pc));
        let pc = self.constant(pc as u64);
        self.store(pcptr, 0, pc);
        self.store(self.view, offset_of!(NativeView, pending), pending);
    }
    fn guard(
        &mut self,
        error: cranelift::prelude::Value,
        fault: NativeFault,
        pc: usize,
        fuel: cranelift::prelude::Value,
        pending: cranelift::prelude::Value,
    ) {
        let fail = self.b.create_block();
        let good = self.b.create_block();
        self.b.ins().brif(error, fail, &[], good, &[]);
        self.b.switch_to_block(fail);
        self.save(pc, pending);
        let code = self.constant(fault as u64);
        self.store(self.view, offset_of!(NativeView, error), code);
        let reason = self.constant(ExitReason::Error as u64);
        self.store(self.view, offset_of!(NativeView, exit_reason), reason);
        self.b.ins().jump(self.done, &[fuel.into()]);
        self.b.switch_to_block(good);
    }
    fn yield_if(
        &mut self,
        condition: cranelift::prelude::Value,
        reason: ExitReason,
        pc: usize,
        fuel: cranelift::prelude::Value,
        pending: cranelift::prelude::Value,
    ) {
        let exit = self.b.create_block();
        let next = self.b.create_block();
        self.b.ins().brif(condition, exit, &[], next, &[]);
        self.b.switch_to_block(exit);
        self.save(pc, pending);
        let reason = self.constant(reason as u64);
        self.store(self.view, offset_of!(NativeView, exit_reason), reason);
        self.b.ins().jump(self.done, &[fuel.into()]);
        self.b.switch_to_block(next);
    }
    fn poll(
        &mut self,
        pc: usize,
        header: bool,
        speed: bool,
        fuel: cranelift::prelude::Value,
        pending: cranelift::prelude::Value,
    ) {
        if !speed {
            if header {
                let osr = self.field(offset_of!(NativeView, osr_requested));
                let osr = self.b.ins().icmp_imm_s(IntCC::NotEqual, osr, 0);
                self.yield_if(osr, ExitReason::Osr, pc, fuel, pending);
            }
            let hot = self.field(offset_of!(NativeView, transition_remaining));
            let hot = self
                .b
                .ins()
                .icmp(IntCC::UnsignedGreaterThanOrEqual, pending, hot);
            self.yield_if(hot, ExitReason::Hot, pc, fuel, pending);
        }
        let inlined = self.field(offset_of!(NativeView, inlined_pending));
        let elapsed = self.b.ins().iadd(pending, inlined);
        let next_poll = self.field(offset_of!(NativeView, next_poll));
        let due = self
            .b
            .ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, elapsed, next_poll);
        let check = self.b.create_block();
        let end = self.b.create_block();
        self.b.ins().brif(due, check, &[], end, &[]);
        self.b.switch_to_block(check);
        let epoch = self.field(offset_of!(NativeView, control_epoch));
        let present = self.b.ins().icmp_imm_s(IntCC::NotEqual, epoch, 0);
        let read = self.b.create_block();
        let unchanged = self.b.create_block();
        self.b.ins().brif(present, read, &[], unchanged, &[]);
        self.b.switch_to_block(read);
        // A notification only: the execution-thread mailbox drain acquires it.
        let current = self
            .b
            .ins()
            .atomic_load(types::I64, MemFlagsData::new(), epoch);
        let seen = self.field(offset_of!(NativeView, observed_epoch));
        let ready = self.b.ins().icmp(IntCC::NotEqual, current, seen);
        self.yield_if(ready, ExitReason::CodeReady, pc, fuel, pending);
        self.b.ins().jump(unchanged, &[]);
        self.b.switch_to_block(unchanged);
        let interval = self.field(offset_of!(NativeView, poll_interval));
        let next = self.b.ins().iadd(elapsed, interval);
        self.store(self.view, offset_of!(NativeView, next_poll), next);
        self.b.ins().jump(end, &[]);
        self.b.switch_to_block(end);
    }
    fn float(&mut self, r: usize, ty: &Type) -> cranelift::prelude::Value {
        let value = self.reg(r);
        if *ty == Type::Integer {
            self.b.ins().fcvt_from_sint(types::F64, value)
        } else {
            self.b.ins().bitcast(types::F64, MemFlagsData::new(), value)
        }
    }
    fn next(
        &mut self,
        fi: usize,
        pc: usize,
        fuel: cranelift::prelude::Value,
        pending: cranelift::prelude::Value,
    ) {
        self.b
            .ins()
            .jump(self.targets[&(fi, pc)], &[fuel.into(), pending.into()]);
    }
    fn inline(
        &mut self,
        fi: usize,
        pc: usize,
        op: &Op,
        fuel: cranelift::prelude::Value,
        pending: cranelift::prelude::Value,
    ) -> bool {
        let f = &self.program.functions[fi];
        match op {
            Op::ServiceKey(r, key) => {
                let value = self.constant(*key as u64);
                self.result(*r, value, native::SERVICE_KEY);
            }
            Op::Service(r, crate::services::Operation::Reference, args) => {
                let view = self.reg(args[0]);
                let key = self.reg(args[1]);
                let high = self.b.ins().ishl_imm_s(view, 32);
                let value = self.b.ins().bor(high, key);
                self.result(*r, value, native::SERVICE_REF);
            }
            Op::Statement | Op::Nop => {}
            Op::Literal(r, literal) => {
                let (bits, tag) = match literal {
                    Literal::Integer(n) => (*n as u64, native::INTEGER),
                    Literal::Real(n) => (n.to_bits(), native::REAL),
                    Literal::Boolean(n) => (u64::from(*n), native::BOOLEAN),
                    Literal::Char(n) => (*n as u64, native::CHAR),
                    _ => return false,
                };
                let value = self.constant(bits);
                self.result(*r, value, tag);
            }
            Op::Address(r, slot) => {
                let ids = self.field(if slot.global {
                    offset_of!(NativeView, globals)
                } else {
                    offset_of!(NativeView, locals)
                });
                let index = if slot.global {
                    slot.index
                } else {
                    f.cell_slot(slot.index)
                };
                let id = self.load(ids, index * size_of::<usize>());
                self.result(*r, id, native::ADDRESS);
            }
            Op::Load(r, a) | Op::FrameLoad(r, a) if scalar(&f.registers[*r]) => {
                let ptr = if matches!(op, Op::FrameLoad(..)) {
                    self.regptr(*a)
                } else {
                    self.cell(*a).1
                };
                let tag = self.load(ptr, offset_of!(NativeSlot, kind));
                let missing = self.b.ins().icmp_imm_s(IntCC::Equal, tag, 0);
                self.guard(missing, NativeFault::UninitializedRead, pc, fuel, pending);
                let value = self.load(ptr, offset_of!(NativeSlot, bits));
                self.write(*r, value, tag);
            }
            Op::Store(a, r) | Op::FrameStore(a, r) if scalar(&f.registers[*r]) => {
                let mut value = self.reg(*r);
                if *f.registers[*a].pointee_or_self() == Type::Real
                    && f.registers[*r] == Type::Integer
                {
                    let real = self.b.ins().fcvt_from_sint(types::F64, value);
                    value = self.b.ins().bitcast(types::I64, MemFlagsData::new(), real);
                }
                if matches!(op, Op::FrameStore(..)) {
                    self.result(*a, value, kind(f.registers[*a].pointee_or_self()));
                } else {
                    let (id, ptr) = self.cell(*a);
                    let old = self.load(ptr, offset_of!(NativeSlot, kind));
                    let dirty = self.b.ins().band_imm_s(old, native::DIRTY as i64);
                    let dirty = self.b.ins().icmp_imm_s(IntCC::NotEqual, dirty, 0);
                    let enqueue = self.b.create_block();
                    let write = self.b.create_block();
                    self.b.ins().brif(dirty, write, &[], enqueue, &[]);
                    self.b.switch_to_block(enqueue);
                    let count = self.field(offset_of!(NativeView, dirty_count));
                    let ids = self.field(offset_of!(NativeView, dirty_ids));
                    let offset = self.b.ins().ishl_imm_s(count, 3);
                    let dest = self.b.ins().iadd(ids, offset);
                    self.store(dest, 0, id);
                    let count = self.b.ins().iadd_imm_s(count, 1);
                    self.store(self.view, offset_of!(NativeView, dirty_count), count);
                    self.b.ins().jump(write, &[]);
                    self.b.switch_to_block(write);
                    self.store(ptr, offset_of!(NativeSlot, bits), value);
                    let tag =
                        self.constant(kind(f.registers[*a].pointee_or_self()) | native::DIRTY);
                    self.store(ptr, offset_of!(NativeSlot, kind), tag);
                }
            }
            Op::Move(r, a) if scalar(&f.registers[*r]) => {
                let value = self.reg(*a);
                let ptr = self.regptr(*a);
                let tag = self.load(ptr, offset_of!(NativeSlot, kind));
                self.write(*r, value, tag);
            }
            Op::Unary(r, op, a) if scalar(&f.registers[*a]) => {
                let value = self.reg(*a);
                let value = match *op {
                    Operator::Subtract if f.registers[*a] == Type::Integer => {
                        let zero = self.constant(0);
                        let (value, overflow) = self.b.ins().ssub_overflow(zero, value);
                        self.guard(overflow, NativeFault::IntegerOverflow, pc, fuel, pending);
                        value
                    }
                    Operator::Subtract => self.b.ins().bxor_imm_s(value, i64::MIN),
                    Operator::Not => self.b.ins().bxor_imm_s(value, 1),
                    _ => value,
                };
                self.result(*r, value, kind(&f.registers[*r]));
            }
            Op::Binary(r, op, a, c) if scalar(&f.registers[*a]) && scalar(&f.registers[*c]) => {
                let at = &f.registers[*a];
                let ct = &f.registers[*c];
                let numeric = matches!(at, Type::Integer | Type::Real)
                    && matches!(ct, Type::Integer | Type::Real);
                let floating = numeric
                    && (*at == Type::Real || *ct == Type::Real || *op == Operator::RealDivide);
                let a = if floating {
                    self.float(*a, at)
                } else {
                    self.reg(*a)
                };
                let c = if floating {
                    self.float(*c, ct)
                } else {
                    self.reg(*c)
                };
                let value = match *op {
                    Operator::Add | Operator::Subtract | Operator::Multiply
                        if !floating && numeric =>
                    {
                        let (value, overflow) = match *op {
                            Operator::Add => self.b.ins().sadd_overflow(a, c),
                            Operator::Subtract => self.b.ins().ssub_overflow(a, c),
                            _ => self.b.ins().smul_overflow(a, c),
                        };
                        self.guard(overflow, NativeFault::IntegerOverflow, pc, fuel, pending);
                        value
                    }
                    Operator::Add
                    | Operator::Subtract
                    | Operator::Multiply
                    | Operator::Divide
                    | Operator::RealDivide
                        if floating =>
                    {
                        // CAIE division fails on a zero divisor; modern float division is IEEE.
                        if *op == Operator::RealDivide {
                            let zero = self.b.ins().f64const(0.0);
                            let zero = self.b.ins().fcmp(FloatCC::Equal, c, zero);
                            self.guard(zero, NativeFault::DivisionByZero, pc, fuel, pending);
                        }
                        let value = match *op {
                            Operator::Add => self.b.ins().fadd(a, c),
                            Operator::Subtract => self.b.ins().fsub(a, c),
                            Operator::Multiply => self.b.ins().fmul(a, c),
                            _ => self.b.ins().fdiv(a, c),
                        };
                        self.b.ins().bitcast(types::I64, MemFlagsData::new(), value)
                    }
                    Operator::IntegerDivide | Operator::Remainder if !floating && numeric => {
                        let zero = self.b.ins().icmp_imm_s(IntCC::Equal, c, 0);
                        self.guard(zero, NativeFault::DivisionByZero, pc, fuel, pending);
                        let min = self.b.ins().icmp_imm_s(IntCC::Equal, a, i64::MIN);
                        let neg = self.b.ins().icmp_imm_s(IntCC::Equal, c, -1);
                        let overflow = self.b.ins().band(min, neg);
                        if *op == Operator::IntegerDivide {
                            self.guard(overflow, NativeFault::IntegerOverflow, pc, fuel, pending);
                            self.b.ins().sdiv(a, c)
                        } else {
                            let one = self.constant(1);
                            let divisor = self.b.ins().select(overflow, one, c);
                            self.b.ins().srem(a, divisor)
                        }
                    }
                    Operator::Equal
                    | Operator::NotEqual
                    | Operator::Less
                    | Operator::Greater
                    | Operator::LessEqual
                    | Operator::GreaterEqual => {
                        let value = if floating {
                            let cc = match *op {
                                Operator::Equal => FloatCC::Equal,
                                Operator::NotEqual => FloatCC::NotEqual,
                                Operator::Less => FloatCC::LessThan,
                                Operator::Greater => FloatCC::GreaterThan,
                                Operator::LessEqual => FloatCC::LessThanOrEqual,
                                _ => FloatCC::GreaterThanOrEqual,
                            };
                            self.b.ins().fcmp(cc, a, c)
                        } else {
                            let cc = match *op {
                                Operator::Equal => IntCC::Equal,
                                Operator::NotEqual => IntCC::NotEqual,
                                Operator::Less => IntCC::SignedLessThan,
                                Operator::Greater => IntCC::SignedGreaterThan,
                                Operator::LessEqual => IntCC::SignedLessThanOrEqual,
                                _ => IntCC::SignedGreaterThanOrEqual,
                            };
                            self.b.ins().icmp(cc, a, c)
                        };
                        self.b.ins().uextend(types::I64, value)
                    }
                    _ => return false,
                };
                self.result(*r, value, kind(&f.registers[*r]));
            }
            Op::CheckStep(r) => {
                let value = self.reg(*r);
                let zero = self.b.ins().icmp_imm_s(IntCC::Equal, value, 0);
                self.guard(zero, NativeFault::ZeroStep, pc, fuel, pending);
            }
            Op::Jump(target) => {
                self.next(fi, *target, fuel, pending);
                return true;
            }
            Op::Branch(r, yes, no) => {
                let condition = self.reg(*r);
                let condition = self.b.ins().icmp_imm_s(IntCC::NotEqual, condition, 0);
                self.b.ins().brif(
                    condition,
                    self.targets[&(fi, *yes)],
                    &[fuel.into(), pending.into()],
                    self.targets[&(fi, *no)],
                    &[fuel.into(), pending.into()],
                );
                return true;
            }
            _ => return false,
        }
        self.next(fi, pc + 1, fuel, pending);
        true
    }
}

pub(super) fn compile_unit(
    program: Arc<Program>,
    function: usize,
    speed: bool,
    inlining: bool,
) -> Result<CraneliftUnit, CompileError> {
    let config = CraneliftConfig {
        inlining,
        ..CraneliftConfig::tiered(speed)
    };
    compile_configured(program, Some(function), config)
}
fn compile_configured(
    program: Arc<Program>,
    function: Option<usize>,
    config: CraneliftConfig,
) -> Result<CraneliftUnit, CompileError> {
    let version = program.next_code_version();
    let mut artifact = compile_part(program.clone(), function, config, false, version)?;
    if config.resume == ResumeStrategy::LoopHeaders {
        artifact.continuation = Some(Box::new(compile_part(
            program, function, config, true, version,
        )?));
    }
    Ok(artifact)
}
fn compile_part(
    program: Arc<Program>,
    function: Option<usize>,
    config: CraneliftConfig,
    continuation: bool,
    version: u64,
) -> Result<CraneliftUnit, CompileError> {
    if let Some(fi) = function
        && program.functions[fi].attributes.compilation == cyallo_ast::CompileHint::AlwaysLazy
    {
        return Err(CompileError::AlwaysLazy);
    }
    if let Some(fi) = function {
        program.functions[fi].checked()?;
    } else {
        program.validate_all()?;
    }
    let speed = config.resume == ResumeStrategy::LoopHeaders;
    let frame_bodies = if config.inlining {
        super::inlining::frame_bodies(
            &program,
            function,
            config.inlining && config.procedure_inlining,
        )
    } else {
        Default::default()
    };
    let flags = config.flags();
    let mut jit = JITBuilder::with_flags(&flags, default_libcall_names())?;
    jit.symbol("caie_native_step", native_step as *const u8)
        .symbol(
            "cyallo_native_property_write",
            native_property_write as *const u8,
        )
        .symbol("caie_native_flush", native_flush as *const u8)
        .symbol("caie_native_enter", native_enter as *const u8);
    let mut module = JITModule::new(jit);
    let ptr = module.target_config().pointer_type();
    // This private ABI currently targets the 64-bit architectures supported here.
    if ptr != types::I64 {
        return Err(CompileError::PointerWidth);
    }
    let step_id = import(&mut module, "caie_native_step", ptr)?;
    let property_write_id = import(&mut module, "cyallo_native_property_write", ptr)?;
    let flush_id = import(&mut module, "caie_native_flush", ptr)?;
    let enter_id = import(&mut module, "caie_native_enter", ptr)?;
    let mut ctx = module.make_context();
    ctx.func.signature.params = vec![
        AbiParam::new(ptr),
        AbiParam::new(ptr),
        AbiParam::new(types::I64),
    ];
    ctx.func.signature.returns = vec![AbiParam::new(types::I64)];
    let id = module.declare_function("caie_entry", Linkage::Local, &ctx.func.signature)?;
    let step = module.declare_func_in_func(step_id, &mut ctx.func);
    let property_write = module.declare_func_in_func(property_write_id, &mut ctx.func);
    let flush = module.declare_func_in_func(flush_id, &mut ctx.func);
    let enter = module.declare_func_in_func(enter_id, &mut ctx.func);
    let engine = if speed || config.optimization == OptimizationLevel::Speed {
        ExecutionEngine::CraneliftSpeed
    } else {
        ExecutionEngine::Cranelift
    };
    let entry_signature = ctx.func.signature.clone();
    let signature = ctx.func.import_signature(entry_signature);
    let mut fbctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbctx);
        let entry = b.create_block();
        let dispatch = b.create_block();
        let choose = b.create_block();
        let done = b.create_block();
        let procedure_return = (!frame_bodies.is_empty()).then(|| {
            let block = b.create_block();
            b.append_block_param(block, types::I64);
            block
        });
        b.append_block_params_for_function_params(entry);
        b.append_block_param(dispatch, types::I64);
        b.append_block_param(done, types::I64);
        b.switch_to_block(entry);
        let session = b.block_params(entry)[0];
        let view = b.block_params(entry)[1];
        let budget = b.block_params(entry)[2];
        let engine_value = b.ins().iconst(types::I64, engine as i64);
        b.ins().store(
            MemFlagsData::new(),
            engine_value,
            view,
            offset_of!(NativeView, engine) as i32,
        );
        let depth = b.ins().load(
            types::I64,
            MemFlagsData::new(),
            view,
            offset_of!(NativeView, depth) as i32,
        );
        b.ins().jump(dispatch, &[budget.into()]);
        let mut l = Lower {
            b,
            program: &program,
            session,
            view,
            depth,
            done,
            targets: HashMap::new(),
            step,
            flush,
            property_write,
            signature,
            inline_plans: super::inlining::plans(
                &program,
                function,
                config.inlining,
                config.procedure_inlining,
            ),
        };
        l.b.switch_to_block(dispatch);
        let version_value = l.constant(version);
        l.store(l.view, offset_of!(NativeView, code_version), version_value);
        let remaining = l.b.block_params(dispatch)[0];
        let empty = l.b.ins().icmp_imm_s(IntCC::Equal, remaining, 0);
        let halted = l.field(offset_of!(NativeView, halted));
        let halted = l.b.ins().icmp_imm_s(IntCC::NotEqual, halted, 0);
        let depth = l.field(offset_of!(NativeView, depth));
        let returned = l.b.ins().icmp(IntCC::UnsignedLessThan, depth, l.depth);
        let stop = l.b.ins().bor(empty, halted);
        let stop = l.b.ins().bor(stop, returned);
        let reason = l.field(offset_of!(NativeView, exit_reason));
        let exiting = l.b.ins().icmp_imm_s(IntCC::NotEqual, reason, 0);
        let stop = l.b.ins().bor(stop, exiting);
        l.b.ins().brif(stop, done, &[remaining.into()], choose, &[]);
        l.b.switch_to_block(choose);
        let fi = l.field(offset_of!(NativeView, function));
        if let Some(function) = function {
            let valid = l.b.create_block();
            let mut matches = l.b.ins().icmp_imm_s(IntCC::Equal, fi, function as i64);
            for body in &frame_bodies {
                let embedded = l.b.ins().icmp_imm_s(IntCC::Equal, fi, *body as i64);
                matches = l.b.ins().bor(matches, embedded);
            }
            l.b.ins()
                .brif(matches, valid, &[], done, &[remaining.into()]);
            l.b.switch_to_block(valid);
        }
        let tracing = l.field(offset_of!(NativeView, reporting));
        let tracing = l.b.ins().icmp_imm_s(IntCC::NotEqual, tracing, 0);
        let observe = l.b.create_block();
        let resume = l.b.create_block();
        l.b.ins().brif(tracing, observe, &[], resume, &[]);
        l.b.switch_to_block(observe);
        l.b.ins().call(enter, &[session]);
        l.b.ins().jump(resume, &[]);
        l.b.switch_to_block(resume);
        let pcptr = l.field(offset_of!(NativeView, pc));
        let pc = l.load(pcptr, 0);
        let fi = l.b.ins().ishl_imm_s(fi, 32);
        let key = l.b.ins().bor(fi, pc);
        let mut switch = Switch::new();
        let mut blocks = vec![];
        for (fi, f) in program.functions.iter().enumerate() {
            if f.attributes.compilation == cyallo_ast::CompileHint::AlwaysLazy {
                continue;
            }
            if function.is_some_and(|wanted| wanted != fi) && !frame_bodies.contains(&fi) {
                continue;
            }
            for pc in 0..f.code.len() {
                let thunk = l.b.create_block();
                let block = l.b.create_block();
                l.b.append_block_param(block, types::I64);
                l.b.append_block_param(block, types::I64);
                l.targets.insert((fi, pc), block);
                if !speed || continuation || f.resume.accepts(pc) {
                    switch.set_entry(((fi as u128) << 32) | pc as u128, thunk);
                }
                blocks.push((thunk, block, fi, pc));
            }
        }
        let unknown = l.b.create_block();
        switch.emit(&mut l.b, key, unknown);
        l.b.switch_to_block(unknown);
        l.b.ins().jump(done, &[remaining.into()]);
        for (thunk, block, fi, pc) in &blocks {
            if speed && !continuation && !program.functions[*fi].resume.accepts(*pc) {
                continue;
            }
            l.b.switch_to_block(*thunk);
            let zero = l.constant(0);
            l.b.ins().jump(*block, &[remaining.into(), zero.into()]);
        }
        for (_, block, fi, pc) in blocks {
            l.b.switch_to_block(block);
            let fuel = l.b.block_params(block)[0];
            let pending = l.b.block_params(block)[1];
            if continuation && program.functions[fi].resume.accepts(pc) {
                // Acyclic optimized continuations stop at the next full-speed
                // entry. All-PC resume edges stay out of the hot loop's IR.
                l.save(pc, pending);
                let reason = l.constant(ExitReason::Continuation as u64);
                l.store(l.view, offset_of!(NativeView, exit_reason), reason);
                l.b.ins().jump(done, &[fuel.into()]);
                continue;
            }
            let empty = l.b.ins().icmp_imm_s(IntCC::Equal, fuel, 0);
            let suspend = l.b.create_block();
            let execute = l.b.create_block();
            l.b.ins().brif(empty, suspend, &[], execute, &[]);
            l.b.switch_to_block(suspend);
            l.save(pc, pending);
            let reason = l.constant(ExitReason::Budget as u64);
            l.store(l.view, offset_of!(NativeView, exit_reason), reason);
            l.b.ins().jump(done, &[fuel.into()]);
            l.b.switch_to_block(execute);
            if program.functions[fi].resume.poll_points[pc] {
                l.poll(
                    pc,
                    program.functions[fi].resume.accepts(pc),
                    speed,
                    fuel,
                    pending,
                );
            }
            l.b.set_srcloc(cranelift::codegen::ir::SourceLoc::new(
                program.bytecode.functions[fi].spans[pc].start,
            ));
            let next = l.b.ins().iadd_imm_s(fuel, -1);
            let count = l.b.ins().iadd_imm_s(pending, 1);
            let op = program.bytecode.functions[fi].decode(pc);
            let effects = program.functions[fi].gc.instructions[pc];
            if let Op::Call(result, _, args) = &op
                && let Some(plan) = l.inline_plans.remove(&(fi, pc))
            {
                l.inline_call((fi, pc), *result, args, &plan, next, count);
                // The successful branch continues to the next caller PC;
                // this block is the unchanged ordinary-call fallback.
            }
            l.property_ic(fi, pc, &op, next, count);
            if !effects.may_collect && !effects.may_reenter && l.inline(fi, pc, &op, next, count) {
                continue;
            }
            // Complex operations and call setup see fully materialized VM state.
            let helper_depth = l.field(offset_of!(NativeView, depth));
            l.save(pc, pending);
            if let Op::TraitCall(_, target, _, _) = op {
                l.trait_target(fi, pc, target);
            }
            l.b.ins().call(l.step, &[l.session]);
            if let Op::Call(_, callee, _) = op
                && frame_bodies.contains(&callee)
            {
                let target = l.field(offset_of!(NativeView, function));
                let matches = l.b.ins().icmp_imm_s(IntCC::Equal, target, callee as i64);
                let halted = l.field(offset_of!(NativeView, halted));
                let active = l.b.ins().icmp_imm_s(IntCC::Equal, halted, 0);
                let tracing = l.field(offset_of!(NativeView, reporting));
                let unobserved = l.b.ins().icmp_imm_s(IntCC::Equal, tracing, 0);
                let enter = l.b.ins().band(matches, active);
                let enter = l.b.ins().band(enter, unobserved);
                let body = l.b.create_block();
                let fallback = l.b.create_block();
                l.b.ins().brif(enter, body, &[], fallback, &[]);
                l.b.switch_to_block(body);
                l.saturating_add(l.view, offset_of!(NativeView, body_inline_calls), 1);
                let zero = l.constant(0);
                l.next(callee, 0, next, zero);
                l.b.switch_to_block(fallback);
            }
            if matches!(
                op,
                Op::Call(..)
                    | Op::TraitCall(..)
                    | Op::Invoke(..)
                    | Op::Lookup(_, crate::lookup::Operation::Mapped, _)
            ) {
                let table = l.field(offset_of!(NativeView, entries));
                let linked = l.b.create_block();
                let resume = l.b.create_block();
                let present = l.b.ins().icmp_imm_s(IntCC::NotEqual, table, 0);
                l.b.ins().brif(present, linked, &[], resume, &[]);
                l.b.switch_to_block(linked);
                let entry = if let Op::Call(_, callee, _) = op {
                    l.load(table, callee * size_of::<usize>())
                } else {
                    let callee = l.field(offset_of!(NativeView, function));
                    let offset = l.b.ins().ishl_imm_s(callee, 3);
                    let ptr = l.b.ins().iadd(table, offset);
                    l.load(ptr, 0)
                };
                let halted = l.field(offset_of!(NativeView, halted));
                let active = l.b.ins().icmp_imm_s(IntCC::Equal, halted, 0);
                let present = l.b.ins().icmp_imm_s(IntCC::NotEqual, entry, 0);
                let callable = l.b.ins().band(active, present);
                // Bound machine-stack growth independently of VM recursion.
                // At the limit, continue through VM dispatch at the safepoint.
                let depth = l.field(offset_of!(NativeView, native_depth));
                let room = l.b.ins().icmp_imm_s(IntCC::UnsignedLessThan, depth, 64);
                let callable = l.b.ins().band(callable, room);
                let call = l.b.create_block();
                l.b.ins().brif(callable, call, &[], resume, &[]);
                l.b.switch_to_block(call);
                let nested = l.b.ins().iadd_imm_s(depth, 1);
                l.store(l.view, offset_of!(NativeView, native_depth), nested);
                let call =
                    l.b.ins()
                        .call_indirect(l.signature, entry, &[l.session, l.view, next]);
                let remaining = l.b.inst_results(call)[0];
                l.store(l.view, offset_of!(NativeView, native_depth), depth);
                // A callee can yield in a deeper frame. Flush that frame before
                // inspecting the dispatch key or unwinding to the execution loop.
                l.b.ins().call(l.flush, &[l.session]);
                let engine_value = l.constant(engine as u64);
                l.store(l.view, offset_of!(NativeView, engine), engine_value);
                let version_value = l.constant(version);
                l.store(l.view, offset_of!(NativeView, code_version), version_value);
                if function.is_some() {
                    let active_depth = l.field(offset_of!(NativeView, depth));
                    let same = l.b.ins().icmp(IntCC::Equal, active_depth, helper_depth);
                    let active_function = l.field(offset_of!(NativeView, function));
                    let same_function =
                        l.b.ins()
                            .icmp_imm_s(IntCC::Equal, active_function, fi as i64);
                    let same = l.b.ins().band(same, same_function);
                    let halted = l.field(offset_of!(NativeView, halted));
                    let active = l.b.ins().icmp_imm_s(IntCC::Equal, halted, 0);
                    let valid = l.b.ins().band(same, active);
                    let reason = l.field(offset_of!(NativeView, exit_reason));
                    let continuing = l.b.ins().icmp_imm_s(IntCC::Equal, reason, 0);
                    let valid = l.b.ins().band(valid, continuing);
                    let continuation = l.b.create_block();
                    l.b.ins()
                        .brif(valid, continuation, &[], done, &[remaining.into()]);
                    l.b.switch_to_block(continuation);
                    let tracing = l.field(offset_of!(NativeView, reporting));
                    let tracing = l.b.ins().icmp_imm_s(IntCC::NotEqual, tracing, 0);
                    let observe = l.b.create_block();
                    let observed = l.b.create_block();
                    l.b.ins().brif(tracing, observe, &[], observed, &[]);
                    l.b.switch_to_block(observe);
                    l.b.ins().call(enter, &[session]);
                    l.b.ins().jump(observed, &[]);
                    l.b.switch_to_block(observed);
                    let zero = l.constant(0);
                    l.next(fi, pc + 1, remaining, zero);
                } else {
                    l.b.ins().jump(dispatch, &[remaining.into()]);
                }
                l.b.switch_to_block(resume);
            }
            if function.is_some() {
                if frame_bodies.contains(&fi) && matches!(op, Op::Return(..)) {
                    l.b.ins().jump(procedure_return.unwrap(), &[next.into()]);
                    continue;
                }
                if matches!(
                    op,
                    Op::Lookup(_, crate::lookup::Operation::Mapped, _)
                        | Op::Call(..)
                        | Op::Invoke(..)
                        | Op::TraitCall(..)
                        | Op::Service(..)
                        | Op::Return(..)
                ) {
                    l.b.ins().jump(done, &[next.into()]);
                } else {
                    let halted = l.field(offset_of!(NativeView, halted));
                    let mut valid = l.b.ins().icmp_imm_s(IntCC::Equal, halted, 0);
                    if effects.may_reenter {
                        let depth = l.field(offset_of!(NativeView, depth));
                        let same_depth = l.b.ins().icmp(IntCC::Equal, depth, helper_depth);
                        let function = l.field(offset_of!(NativeView, function));
                        let same_function = l.b.ins().icmp_imm_s(IntCC::Equal, function, fi as i64);
                        let same = l.b.ins().band(same_depth, same_function);
                        valid = l.b.ins().band(valid, same);
                    }
                    let zero = l.constant(0);
                    l.b.ins().brif(
                        valid,
                        l.targets[&(fi, pc + 1)],
                        &[next.into(), zero.into()],
                        done,
                        &[next.into()],
                    );
                }
            } else if frame_bodies.contains(&fi) && matches!(op, Op::Return(..)) {
                l.b.ins().jump(procedure_return.unwrap(), &[next.into()]);
            } else {
                l.b.ins().jump(dispatch, &[next.into()]);
            }
        }
        if let Some(procedure_return) = procedure_return {
            l.b.switch_to_block(procedure_return);
            let fuel = l.b.block_params(procedure_return)[0];
            let tracing = l.field(offset_of!(NativeView, reporting));
            let tracing = l.b.ins().icmp_imm_s(IntCC::NotEqual, tracing, 0);
            let unobserved = l.b.create_block();
            l.b.ins()
                .brif(tracing, dispatch, &[fuel.into()], unobserved, &[]);
            l.b.switch_to_block(unobserved);
            let halted = l.field(offset_of!(NativeView, halted));
            let halted = l.b.ins().icmp_imm_s(IntCC::NotEqual, halted, 0);
            let depth = l.field(offset_of!(NativeView, depth));
            let returned = l.b.ins().icmp(IntCC::UnsignedLessThan, depth, l.depth);
            let empty = l.b.ins().icmp_imm_s(IntCC::Equal, fuel, 0);
            let stop = l.b.ins().bor(halted, returned);
            let stop = l.b.ins().bor(stop, empty);
            let reason = l.field(offset_of!(NativeView, exit_reason));
            let exiting = l.b.ins().icmp_imm_s(IntCC::NotEqual, reason, 0);
            let stop = l.b.ins().bor(stop, exiting);
            let resume = l.b.create_block();
            l.b.ins().brif(stop, done, &[fuel.into()], resume, &[]);
            l.b.switch_to_block(resume);
            let fi = l.field(offset_of!(NativeView, function));
            let pcptr = l.field(offset_of!(NativeView, pc));
            let pc = l.load(pcptr, 0);
            let fi = l.b.ins().ishl_imm_s(fi, 32);
            let key = l.b.ins().bor(fi, pc);
            let unknown = l.b.create_block();
            let mut switch = Switch::new();
            let mut landings = vec![];
            for (fi, f) in program.functions.iter().enumerate() {
                if f.attributes.compilation == cyallo_ast::CompileHint::AlwaysLazy {
                    continue;
                }
                if function.is_some_and(|wanted| wanted != fi) && !frame_bodies.contains(&fi) {
                    continue;
                }
                for (pc, instruction) in f.code.iter().enumerate() {
                    if let Op::Call(_, to, _) = instruction.op
                        && frame_bodies.contains(&to)
                    {
                        let block = l.b.create_block();
                        switch.set_entry(((fi as u128) << 32) | (pc + 1) as u128, block);
                        landings.push((block, fi, pc + 1));
                    }
                }
            }
            switch.emit(&mut l.b, key, unknown);
            l.b.switch_to_block(unknown);
            l.b.ins().jump(done, &[fuel.into()]);
            for (block, fi, pc) in landings {
                l.b.switch_to_block(block);
                let zero = l.constant(0);
                // Internal return continuations accept any caller PC, even when
                // external speed entries are restricted to loop headers.
                l.next(fi, pc, fuel, zero);
            }
        }
        l.b.switch_to_block(done);
        let remaining = l.b.block_params(done)[0];
        l.b.ins().return_(&[remaining]);
        l.b.seal_all_blocks();
        l.b.finalize(module.target_config());
    }
    let observer = program.inspection();
    let prefix = format!(
        "p{}-f{}-v{version}-{:?}-{}",
        program.snapshot_id,
        function.map_or("all".into(), |fi| fi.to_string()),
        config.optimization,
        if continuation {
            "continuation"
        } else {
            "entry"
        }
    );
    if let Some(observer) = observer
        && observer.options.emit_clif
    {
        observer.write(
            &format!("{prefix}.before.clif"),
            &ctx.func.display().to_string(),
        );
    }
    module.define_function(id, &mut ctx)?;
    if let Some(observer) = observer
        && observer.options.emit_clif
    {
        observer.write(
            &format!("{prefix}.after.clif"),
            &ctx.func.display().to_string(),
        );
    }
    program.notice(
        "opt",
        function.unwrap_or(0),
        format!(
            "Cranelift {:?}; scope {:?}; resume {:?}; version {version}",
            config.optimization, config.scope, config.resume
        ),
    );
    let code_bytes = ctx
        .compiled_code()
        .expect("defined function")
        .code_buffer()
        .len();
    module.finalize_definitions()?;
    let code = module.get_finalized_function(id);
    // SAFETY: the emitted signature and calling convention exactly match Entry.
    let entry = unsafe { std::mem::transmute::<*const u8, Entry>(code) };
    Ok(CraneliftUnit {
        program,
        module: Some(module),
        entry,
        engine,
        function,
        version,
        continuation: None,
        config,
        code_bytes,
    })
}
