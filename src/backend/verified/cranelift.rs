//! Cranelift translation over the shared scalar native plan.

use super::{
    AbiLayout, Architecture, CraneliftMachineInfo, CraneliftProfile, ElementAccess, EmitError,
    Failure, LoanBase, MachineRange, MachineTrap, NativeHome, NativeTarget, NativeTier, Operation,
    Platform, PreparedFunction, RecoverySlot, ScalarOutcome, Site, UnaryOp, UnlinkedImage, WordOp,
    WordRange,
    abi::{Edge, VECTOR_HEADER_BYTES},
    image::checked_source,
};
use crate::bytecode::verified::{BooleanOp, FloatOp, InstructionId, IntegerOp, Scalar};
use cranelift::{
    codegen::{
        Context,
        control::ControlPlane,
        ir::{Function, MemFlagsData, SourceLoc},
        isa::{self, TargetFrontendConfig},
        settings,
    },
    frontend::Switch,
    prelude::*,
};
use std::{collections::BTreeMap, sync::Arc};

pub(super) fn emit(
    plan: Arc<PreparedFunction>,
    profile: CraneliftProfile,
) -> Result<UnlinkedImage, EmitError> {
    let target = plan.target();
    let mut settings = settings::builder();
    settings
        .set("machine_code_cfg_info", "true")
        .map_err(EmitError::Configuration)?;
    // The higher-quality allocator for both profiles; `single_pass` compiles
    // faster but produces slower code, and the profiles already differ in
    // optimization level.
    settings
        .set("regalloc_algorithm", "backtracking")
        .map_err(EmitError::Configuration)?;
    settings
        .set(
            "opt_level",
            match profile {
                CraneliftProfile::LowLatency => "none",
                CraneliftProfile::Optimized => "speed",
            },
        )
        .map_err(EmitError::Configuration)?;
    let target_isa = isa::lookup_by_name(triple(target))
        .map_err(|_| EmitError::UnsupportedTarget(target))?
        .finish(settings::Flags::new(settings))
        .map_err(EmitError::Codegen)?;
    let mut ctx = Context::new();
    ctx.func.signature = Signature::new(target_isa.default_call_conv());
    // Pointers to context, capture-free environment and result place.
    for _ in [
        super::AbiArgument::Context,
        super::AbiArgument::Environment,
        super::AbiArgument::ResultPlace,
    ] {
        ctx.func.signature.params.push(AbiParam::new(types::I64));
    }
    ctx.func.signature.returns.push(AbiParam::new(types::I64));
    build(&mut ctx.func, &plan, target_isa.frontend_config())?;
    let compiled = ctx
        .compile(&*target_isa, &mut ControlPlane::default())
        .map_err(|error| EmitError::Codegen(error.inner))?;
    if !compiled.buffer.relocs().is_empty() {
        return Err(EmitError::UnexpectedRelocation);
    }
    let ranges = compiled
        .buffer
        .get_srclocs_sorted()
        .iter()
        .filter_map(|record| {
            if record.loc.is_rel() {
                return None;
            }
            let loc = record.loc.as_abs();
            (!loc.is_default()).then_some(MachineRange {
                instruction: InstructionId(loc.bits()),
                start: record.start,
                end: record.end,
            })
        })
        .collect();
    let bytes = compiled.code_buffer().into();
    let machine = CraneliftMachineInfo {
        blocks: compiled.bb_starts.clone().into_boxed_slice(),
        edges: compiled.bb_edges.clone().into_boxed_slice(),
        traps: compiled
            .buffer
            .traps()
            .iter()
            .map(|trap| MachineTrap {
                offset: trap.offset,
                code: trap.code,
            })
            .collect(),
    };
    Ok(UnlinkedImage {
        relocations: plan.relocations().collect(),
        plan,
        tier: NativeTier::Cranelift(profile),
        bytes,
        ranges,
        cranelift: Some(machine),
    })
}

fn triple(target: NativeTarget) -> &'static str {
    match (target.platform, target.architecture) {
        (Platform::MacOs, Architecture::Aarch64) => "aarch64-apple-darwin",
        (Platform::MacOs, Architecture::X86_64) => "x86_64-apple-darwin",
        (Platform::Linux, Architecture::Aarch64) => "aarch64-unknown-linux-gnu",
        (Platform::Linux, Architecture::X86_64) => "x86_64-unknown-linux-gnu",
    }
}

fn build(
    function: &mut Function,
    plan: &PreparedFunction,
    frontend: TargetFrontendConfig,
) -> Result<(), EmitError> {
    let mut context = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(function, &mut context);
    let entry = builder.create_block();
    let bad_pc = builder.create_block();
    let labels = plan
        .sites()
        .iter()
        .map(|site| (site.origin.instruction, builder.create_block()))
        .collect::<BTreeMap<_, _>>();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let &[context, _environment, result] = builder.block_params(entry) else {
        unreachable!("scalar entry declares the three ABI parameters");
    };
    let abi = plan.abi();
    let slots = builder
        .ins()
        .load(types::I64, MemFlagsData::new(), context, offset(abi.slots)?);
    let pc = builder
        .ins()
        .load(types::I64, MemFlagsData::new(), context, offset(abi.next)?);
    // Use SSA variables across hot edges. Only the live homes at a handoff
    // are materialized, so Cranelift can coalesce moves and allocate registers.
    let slot_count = usize::try_from(plan.frame_bytes())
        .map_err(|_| EmitError::AbiOffsetOutOfRange)?
        / abi.slot_bytes;
    let mut homes = Vec::with_capacity(slot_count);
    for index in 0..slot_count {
        let off = index
            .checked_mul(abi.slot_bytes)
            .ok_or(EmitError::AbiOffsetOutOfRange)?;
        homes.push(load_variable(&mut builder, slots, off)?);
    }
    let budget = load_variable(&mut builder, context, abi.budget)?;
    let predecessor = load_variable(&mut builder, context, abi.predecessor)?;
    let edge = load_variable(&mut builder, context, abi.edge)?;
    let mut switch = Switch::new();
    for (&pc, &block) in &labels {
        switch.set_entry(u128::from(pc.0), block);
    }
    switch.emit(&mut builder, pc, bad_pc);
    let mut lower = Lower {
        builder: &mut builder,
        context,
        result,
        slots,
        homes,
        borrowed: (0..plan.frame_bytes() / 8)
            .map(|word| word * 8)
            .filter(|&offset| plan.is_borrowed(offset))
            .collect(),
        budget,
        predecessor,
        edge,
        labels,
        abi,
    };
    lower.builder.switch_to_block(bad_pc);
    lower.store_context(abi.failure, Failure::Trap as u64)?;
    lower.exit(ScalarOutcome::Failed)?;
    for site in plan.sites() {
        lower.site(site, plan)?;
    }
    builder.seal_all_blocks();
    builder.finalize(frontend);
    Ok(())
}

fn load_variable(
    builder: &mut FunctionBuilder<'_>,
    base: Value,
    field: usize,
) -> Result<Variable, EmitError> {
    let variable = builder.declare_var(types::I64);
    let value = builder
        .ins()
        .load(types::I64, MemFlagsData::new(), base, offset(field)?);
    builder.def_var(variable, value);
    Ok(variable)
}

struct Lower<'builder, 'function> {
    builder: &'builder mut FunctionBuilder<'function>,
    context: Value,
    result: Value,
    slots: Value,
    homes: Vec<Variable>,
    /// Words that loans point at; they live in the frame, not in variables.
    borrowed: std::collections::BTreeSet<u32>,
    budget: Variable,
    predecessor: Variable,
    edge: Variable,
    labels: BTreeMap<InstructionId, Block>,
    abi: AbiLayout,
}

impl Lower<'_, '_> {
    fn label(&self, pc: InstructionId) -> Result<Block, EmitError> {
        self.labels
            .get(&pc)
            .copied()
            .ok_or(EmitError::MissingSite(pc))
    }

    fn site(&mut self, site: &Site, plan: &PreparedFunction) -> Result<(), EmitError> {
        let pc = site.origin.instruction;
        self.builder.set_srcloc(SourceLoc::new(checked_source(pc)?));
        self.builder.switch_to_block(self.label(pc)?);
        let running = self.builder.create_block();
        let paused = self.builder.create_block();
        self.builder.set_cold_block(paused);
        let budget = self.builder.use_var(self.budget);
        let can_run = self.builder.ins().icmp_imm_u(
            IntCC::UnsignedGreaterThanOrEqual,
            budget,
            word(site.fuel),
        );
        self.builder.ins().brif(can_run, running, &[], paused, &[]);
        self.builder.switch_to_block(paused);
        self.spill(site, plan)?;
        self.exit_at(ScalarOutcome::Paused, pc)?;
        self.builder.switch_to_block(running);
        let cost = self.builder.ins().iconst(types::I64, word(site.fuel));
        let remaining = self.builder.ins().isub(budget, cost);
        self.builder.def_var(self.budget, remaining);
        self.operation(site, plan)
    }

    fn operation(&mut self, site: &Site, plan: &PreparedFunction) -> Result<(), EmitError> {
        use Operation::*;
        let pc = site.origin.instruction;
        match site.operation {
            StorageLive(_) | StorageDead(_) => {}
            Constant { destination, value } => {
                let bits = match value {
                    Scalar::I64(n) => n,
                    Scalar::F64(bits) => i64::from_ne_bytes(bits.to_ne_bytes()),
                    Scalar::U64(value) => i64::from_ne_bytes(value.to_ne_bytes()),
                    Scalar::Bool(b) => i64::from(b),
                };
                let value = self.builder.ins().iconst(types::I64, bits);
                self.store(destination, value)?;
            }
            Copy {
                destination,
                source,
            } => {
                let value = self.load(source)?;
                self.store(destination, value)?;
            }
            Integer {
                operation,
                destination,
                left,
                right,
            } => {
                let left = self.load(left)?;
                let right = self.load(right)?;
                let value = self.integer(operation, pc, left, right)?;
                self.store(destination, value)?;
            }
            Float {
                operation,
                destination,
                left,
                right,
            } => {
                let left = self.load(left)?;
                let right = self.load(right)?;
                let value = self.float(operation, pc, left, right)?;
                self.store(destination, value)?;
            }
            Boolean {
                operation,
                destination,
                left,
                right,
            } => {
                let left = self.load(left)?;
                let right = self.load(right)?;
                let condition = match operation {
                    BooleanOp::Equal => IntCC::Equal,
                    BooleanOp::NotEqual => IntCC::NotEqual,
                };
                let result = self.builder.ins().icmp(condition, left, right);
                let value = self.builder.ins().uextend(types::I64, result);
                self.store(destination, value)?;
            }
            RealDivide {
                destination,
                left,
                right,
            } => {
                let left = self.load(left)?;
                let right = self.load(right)?;
                let zero = self.builder.ins().icmp_imm_s(IntCC::Equal, right, 0);
                self.guard(zero, pc, Failure::DivisionByZero)?;
                let left = self.builder.ins().fcvt_from_sint(types::F64, left);
                let right = self.builder.ins().fcvt_from_sint(types::F64, right);
                let quotient = self.builder.ins().fdiv(left, right);
                let value = self.float_as_bits(quotient);
                self.store(destination, value)?;
            }
            Unary {
                operation,
                destination,
                source,
            } => {
                let source = self.load(source)?;
                let value = self.unary(operation, pc, source)?;
                self.store(destination, value)?;
            }
            Words(range) => self.words(plan, range)?,
            Range {
                destination,
                start,
                end,
                inclusive,
            } => {
                let start = self.load(start)?;
                let end = self.load(end)?;
                let end = if inclusive {
                    let one = self.builder.ins().iconst(types::I64, 1);
                    self.integer(IntegerOp::Add, pc, end, one)?
                } else {
                    end
                };
                self.store_at(destination.offset.0, start)?;
                self.store_at(destination.offset.0 + 8, end)?;
            }
            VecLength { destination, base } => {
                let buffer = self.buffer(base)?;
                let length = self
                    .builder
                    .ins()
                    .load(types::I64, MemFlagsData::new(), buffer, 0);
                self.store(destination, length)?;
            }
            ArrayLoad {
                destination,
                array,
                access,
            } => {
                let buffer = self.load(array)?;
                let (element, written_flag) = self.element(plan, access, buffer, pc)?;
                if written_flag {
                    let written =
                        self.builder
                            .ins()
                            .load(types::I64, MemFlagsData::new(), element, 0);
                    let unwritten = self.builder.ins().icmp_imm_s(IntCC::Equal, written, 0);
                    self.guard(unwritten, pc, Failure::UninitializedElement)?;
                }
                let value = self.builder.ins().load(
                    types::I64,
                    MemFlagsData::new(),
                    element,
                    i32::from(written_flag) * 8,
                );
                self.store(destination, value)?;
            }
            ArrayStore {
                array,
                value,
                access,
            } => {
                let buffer = self.load(array)?;
                let (element, written_flag) = self.element(plan, access, buffer, pc)?;
                let value = self.load(value)?;
                if written_flag {
                    let one = self.builder.ins().iconst(types::I64, 1);
                    self.builder
                        .ins()
                        .store(MemFlagsData::new(), one, element, 0);
                }
                self.builder.ins().store(
                    MemFlagsData::new(),
                    value,
                    element,
                    i32::from(written_flag) * 8,
                );
            }
            ElementLoan {
                destination,
                base,
                access,
            } => {
                let buffer = self.buffer(base)?;
                let (element, written_flag) = self.element(plan, access, buffer, pc)?;
                // Arrays tag the value address so writes also set the flag.
                let loan = if written_flag {
                    self.builder.ins().iadd_imm_s(element, 9)
                } else {
                    element
                };
                self.store(destination, loan)?;
            }
            Bridge(_) | Runtime => {
                self.spill(site, plan)?;
                return self.exit_at(ScalarOutcome::Bridge, pc);
            }
            Borrow { destination, owner } => {
                let address = self
                    .builder
                    .ins()
                    .iadd_imm_s(self.slots, i64::from(owner.0));
                self.store(destination, address)?;
            }
            Project {
                destination,
                base,
                offset,
                variant,
            } => {
                let address = match base {
                    LoanBase::Loan(home) => self.load(home)?,
                    LoanBase::Owner(owner) => self
                        .builder
                        .ins()
                        .iadd_imm_s(self.slots, i64::from(owner.0)),
                };
                if let Some(variant) = variant {
                    let held = self
                        .builder
                        .ins()
                        .load(types::I64, MemFlagsData::new(), address, 0);
                    let other =
                        self.builder
                            .ins()
                            .icmp_imm_s(IntCC::NotEqual, held, i64::from(variant));
                    self.guard(other, pc, Failure::EnumVariant)?;
                }
                let projected = self.builder.ins().iadd_imm_s(address, i64::from(offset));
                self.store(destination, projected)?;
            }
            ReadLoan {
                destination,
                loan,
                words,
            } => {
                let tagged = self.load(loan)?;
                let address = self.builder.ins().band_imm_s(tagged, -2);
                for word in 0..words {
                    let at = i32::try_from(word * 8).map_err(|_| EmitError::AbiOffsetOutOfRange)?;
                    let value =
                        self.builder
                            .ins()
                            .load(types::I64, MemFlagsData::new(), address, at);
                    self.store_at(destination.0 + word * 8, value)?;
                }
            }
            WriteLoan {
                loan,
                source,
                words,
            } => {
                let tagged = self.load(loan)?;
                let address = self.builder.ins().band_imm_s(tagged, -2);
                for word in 0..words {
                    let at = i32::try_from(word * 8).map_err(|_| EmitError::AbiOffsetOutOfRange)?;
                    let value = self.load_at(source.0 + word * 8)?;
                    self.builder
                        .ins()
                        .store(MemFlagsData::new(), value, address, at);
                }
                // A write through an element loan marks the element written.
                let mark = self.builder.create_block();
                let after = self.builder.create_block();
                let is_element = self.builder.ins().band_imm_s(tagged, 1);
                self.builder.ins().brif(is_element, mark, &[], after, &[]);
                self.builder.switch_to_block(mark);
                let one = self.builder.ins().iconst(types::I64, 1);
                self.builder
                    .ins()
                    .store(MemFlagsData::new(), one, address, -8);
                self.builder.ins().jump(after, &[]);
                self.builder.switch_to_block(after);
            }
            NonZero(step) => {
                let value = self.load(step)?;
                let zero = self.builder.ins().icmp_imm_s(IntCC::Equal, value, 0);
                self.guard(zero, pc, Failure::ZeroStep)?;
            }
            TagIs {
                destination,
                base,
                variant,
            } => {
                let held = match base {
                    LoanBase::Owner(tag) => self.load_at(tag.0)?,
                    LoanBase::Loan(home) => {
                        let address = self.load(home)?;
                        self.builder
                            .ins()
                            .load(types::I64, MemFlagsData::new(), address, 0)
                    }
                };
                let is = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::Equal, held, i64::from(variant));
                let value = self.builder.ins().uextend(types::I64, is);
                self.store(destination, value)?;
            }
            Variant { tag, variant, then } => {
                let held = self.load_at(tag.0)?;
                let other =
                    self.builder
                        .ins()
                        .icmp_imm_s(IntCC::NotEqual, held, i64::from(variant));
                self.guard(other, pc, Failure::EnumVariant)?;
                self.words(plan, then)?;
            }
            Jump(to) => {
                self.arrive(pc, Edge::Jump);
                let block = self.label(to)?;
                self.builder.ins().jump(block, &[]);
            }
            Branch { condition, yes, no } => {
                let value = self.load(condition)?;
                let yes_edge = self.builder.create_block();
                let no_edge = self.builder.create_block();
                self.builder.ins().brif(value, yes_edge, &[], no_edge, &[]);
                for (edge, block, target) in [(Edge::Yes, yes_edge, yes), (Edge::No, no_edge, no)] {
                    self.builder.switch_to_block(block);
                    self.arrive(pc, edge);
                    let target = self.label(target)?;
                    self.builder.ins().jump(target, &[]);
                }
            }
            Return(value) => {
                if let Some(home) = value
                    && let Some(id) = home.layout
                {
                    // A composite result is read from the frame by the caller.
                    let words = plan
                        .layout(id)
                        .ok_or(EmitError::AbiOffsetOutOfRange)?
                        .words();
                    for word in 0..words {
                        let at = home.offset.0 + word * 8;
                        if self.borrowed.contains(&at) {
                            continue;
                        }
                        let value = self.load_at(at)?;
                        let offset =
                            i32::try_from(at).map_err(|_| EmitError::AbiOffsetOutOfRange)?;
                        self.builder
                            .ins()
                            .store(MemFlagsData::new(), value, self.slots, offset);
                    }
                } else if let Some(home) = value {
                    let value = self.load(home)?;
                    self.builder
                        .ins()
                        .store(MemFlagsData::new(), value, self.result, 0);
                }
                self.exit_at(ScalarOutcome::Returned, pc)?;
            }
            Call(call_pc) => {
                let call = plan.call(call_pc).ok_or(EmitError::MissingCall(call_pc))?;
                self.spill(site, plan)?;
                let links = self.builder.ins().load(
                    types::I64,
                    MemFlagsData::new(),
                    self.context,
                    offset(self.abi.links)?,
                );
                let field = i32::try_from(call.link.offset.0)
                    .map_err(|_| EmitError::AbiOffsetOutOfRange)?;
                let callee = self
                    .builder
                    .ins()
                    .load(types::I64, MemFlagsData::new(), links, field);
                self.builder.ins().store(
                    MemFlagsData::new(),
                    callee,
                    self.context,
                    offset(self.abi.callee)?,
                );
                return self.exit_at(ScalarOutcome::Call, pc);
            }
            Trap => self.fail(pc, Failure::Trap)?,
        }
        if let Some(next) = site.successor {
            self.arrive(pc, Edge::Next);
            let block = self.label(next)?;
            self.builder.ins().jump(block, &[]);
        }
        Ok(())
    }

    fn integer(
        &mut self,
        operation: IntegerOp,
        pc: InstructionId,
        left: Value,
        right: Value,
    ) -> Result<Value, EmitError> {
        use IntegerOp::*;
        let (value, overflow) = match operation {
            Add => self.builder.ins().sadd_overflow(left, right),
            Subtract => self.builder.ins().ssub_overflow(left, right),
            Multiply => self.builder.ins().smul_overflow(left, right),
            Divide | Remainder => return self.division(operation, pc, left, right),
            comparison => {
                let result = self.builder.ins().icmp(condition(comparison), left, right);
                return Ok(self.builder.ins().uextend(types::I64, result));
            }
        };
        self.guard(overflow, pc, Failure::Overflow)?;
        Ok(value)
    }

    fn buffer(&mut self, base: LoanBase) -> Result<Value, EmitError> {
        match base {
            LoanBase::Owner(owner) => self.load_at(owner.0),
            LoanBase::Loan(home) => {
                let place = self.load(home)?;
                let place = self.builder.ins().band_imm_s(place, -2);
                Ok(self
                    .builder
                    .ins()
                    .load(types::I64, MemFlagsData::new(), place, 0))
            }
        }
    }

    /// Checks bounds and returns the element address and whether it has a written flag.
    fn element(
        &mut self,
        plan: &PreparedFunction,
        access: u32,
        buffer: Value,
        pc: InstructionId,
    ) -> Result<(Value, bool), EmitError> {
        let access = plan
            .element_access(access)
            .ok_or(EmitError::AbiOffsetOutOfRange)?;
        let (bounds, indices) = match access {
            ElementAccess::Vec { index } => {
                let index = self.load(*index)?;
                let length = self
                    .builder
                    .ins()
                    .load(types::I64, MemFlagsData::new(), buffer, 0);
                // Unsigned comparison also rejects negative signed indices.
                let bad = self
                    .builder
                    .ins()
                    .icmp(IntCC::UnsignedGreaterThanOrEqual, index, length);
                self.guard(bad, pc, Failure::IndexOutOfBounds)?;
                let bytes = self.builder.ins().ishl_imm_s(index, 3);
                let data = self
                    .builder
                    .ins()
                    .iadd_imm_s(buffer, i64::from(VECTOR_HEADER_BYTES));
                return Ok((self.builder.ins().iadd(data, bytes), false));
            }
            ElementAccess::Array(array) => (&array.bounds, &array.indices),
        };
        let mut offset = self.builder.ins().iconst(types::I64, 0);
        let mut bad: Option<Value> = None;
        for (&(lower, upper), &home) in bounds.iter().zip(indices.iter()) {
            let index = self.load(home)?;
            let below = self
                .builder
                .ins()
                .icmp_imm_s(IntCC::SignedLessThan, index, lower);
            let above = self
                .builder
                .ins()
                .icmp_imm_s(IntCC::SignedGreaterThan, index, upper);
            let out = self.builder.ins().bor(below, above);
            bad = Some(match bad {
                Some(earlier) => self.builder.ins().bor(earlier, out),
                None => out,
            });
            let extent = upper.wrapping_sub(lower).wrapping_add(1);
            offset = self.builder.ins().imul_imm_s(offset, extent);
            let position = self.builder.ins().iadd_imm_s(index, lower.wrapping_neg());
            offset = self.builder.ins().iadd(offset, position);
        }
        if let Some(bad) = bad {
            self.guard(bad, pc, Failure::IndexOutOfBounds)?;
        }
        let bytes = self.builder.ins().ishl_imm_s(offset, 4);
        Ok((self.builder.ins().iadd(buffer, bytes), true))
    }

    fn bits_as_float(&mut self, word: Value) -> Value {
        self.builder
            .ins()
            .bitcast(types::F64, MemFlagsData::new(), word)
    }

    fn float_as_bits(&mut self, float: Value) -> Value {
        self.builder
            .ins()
            .bitcast(types::I64, MemFlagsData::new(), float)
    }

    fn float(
        &mut self,
        operation: FloatOp,
        pc: InstructionId,
        left: Value,
        right: Value,
    ) -> Result<Value, EmitError> {
        use FloatOp::*;
        let left = self.bits_as_float(left);
        let right = self.bits_as_float(right);
        let arithmetic = match operation {
            Add => self.builder.ins().fadd(left, right),
            Subtract => self.builder.ins().fsub(left, right),
            Multiply => self.builder.ins().fmul(left, right),
            Divide => self.builder.ins().fdiv(left, right),
            CheckedDivide => {
                let zero = self.builder.ins().f64const(0.0);
                let is_zero = self.builder.ins().fcmp(FloatCC::Equal, right, zero);
                self.guard(is_zero, pc, Failure::DivisionByZero)?;
                self.builder.ins().fdiv(left, right)
            }
            Remainder => return Err(EmitError::UnsupportedOperation(pc)),
            comparison => {
                // NaN compares unequal to everything, itself included.
                let condition = match comparison {
                    Less => FloatCC::LessThan,
                    LessEqual => FloatCC::LessThanOrEqual,
                    Greater => FloatCC::GreaterThan,
                    GreaterEqual => FloatCC::GreaterThanOrEqual,
                    Equal => FloatCC::Equal,
                    NotEqual => FloatCC::NotEqual,
                    _ => unreachable!("arithmetic handled above"),
                };
                let result = self.builder.ins().fcmp(condition, left, right);
                return Ok(self.builder.ins().uextend(types::I64, result));
            }
        };
        Ok(self.float_as_bits(arithmetic))
    }

    fn unary(
        &mut self,
        operation: UnaryOp,
        pc: InstructionId,
        source: Value,
    ) -> Result<Value, EmitError> {
        Ok(match operation {
            UnaryOp::NegateInteger => {
                let minimum = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::Equal, source, i64::MIN);
                self.guard(minimum, pc, Failure::Overflow)?;
                self.builder.ins().ineg(source)
            }
            // Flipping the sign bit negates every binary64 value, NaN included.
            UnaryOp::NegateFloat => self.builder.ins().bxor_imm_s(source, i64::MIN),
            UnaryOp::Not => self.builder.ins().bxor_imm_s(source, 1),
            UnaryOp::CountConversion => {
                let high_bit = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::SignedLessThan, source, 0);
                self.guard(high_bit, pc, Failure::ConversionOutOfRange)?;
                source
            }
            UnaryOp::IntegerToFloat => {
                let float = self.builder.ins().fcvt_from_sint(types::F64, source);
                self.float_as_bits(float)
            }
            UnaryOp::FloatToInteger => {
                let float = self.bits_as_float(source);
                let truncated = self.builder.ins().trunc(float);
                // 2^63 is not an i64, so the upper bound is exclusive; NaN
                // fails both comparisons.
                let low = self.builder.ins().f64const(-9_223_372_036_854_775_808.0);
                let high = self.builder.ins().f64const(9_223_372_036_854_775_808.0);
                let above = self
                    .builder
                    .ins()
                    .fcmp(FloatCC::GreaterThanOrEqual, truncated, low);
                let below = self.builder.ins().fcmp(FloatCC::LessThan, truncated, high);
                let in_range = self.builder.ins().band(above, below);
                let out_of_range = self.builder.ins().bxor_imm_s(in_range, 1);
                self.guard(out_of_range, pc, Failure::ConversionOutOfRange)?;
                self.builder.ins().fcvt_to_sint_sat(types::I64, truncated)
            }
        })
    }

    fn division(
        &mut self,
        operation: IntegerOp,
        pc: InstructionId,
        left: Value,
        right: Value,
    ) -> Result<Value, EmitError> {
        let zero = self.builder.ins().icmp_imm_s(IntCC::Equal, right, 0);
        self.guard(zero, pc, Failure::DivisionByZero)?;
        let minimum = self.builder.ins().icmp_imm_s(IntCC::Equal, left, i64::MIN);
        let negative_one = self.builder.ins().icmp_imm_s(IntCC::Equal, right, -1);
        let extreme = self.builder.ins().band(minimum, negative_one);
        if operation == IntegerOp::Divide {
            self.guard(extreme, pc, Failure::Overflow)?;
            Ok(self.builder.ins().sdiv(left, right))
        } else {
            // MIN % -1 is zero. Substituting a safe divisor only on this edge
            // preserves that result without exposing a native quotient trap.
            let one = self.builder.ins().iconst(types::I64, 1);
            let divisor = self.builder.ins().select(extreme, one, right);
            Ok(self.builder.ins().srem(left, divisor))
        }
    }

    fn guard(
        &mut self,
        condition: Value,
        pc: InstructionId,
        failure: Failure,
    ) -> Result<(), EmitError> {
        let error = self.builder.create_block();
        let success = self.builder.create_block();
        self.builder.set_cold_block(error);
        self.builder.ins().brif(condition, error, &[], success, &[]);
        self.builder.switch_to_block(error);
        self.fail(pc, failure)?;
        self.builder.switch_to_block(success);
        Ok(())
    }

    fn load(&mut self, home: NativeHome) -> Result<Value, EmitError> {
        self.load_at(home.offset.0)
    }

    fn store(&mut self, home: NativeHome, value: Value) -> Result<(), EmitError> {
        self.store_at(home.offset.0, value)
    }

    fn load_at(&mut self, offset: u32) -> Result<Value, EmitError> {
        if self.borrowed.contains(&offset) {
            let at = i32::try_from(offset).map_err(|_| EmitError::AbiOffsetOutOfRange)?;
            return Ok(self
                .builder
                .ins()
                .load(types::I64, MemFlagsData::new(), self.slots, at));
        }
        let variable = self.home_at(offset)?;
        Ok(self.builder.use_var(variable))
    }

    fn store_at(&mut self, offset: u32, value: Value) -> Result<(), EmitError> {
        if self.borrowed.contains(&offset) {
            let at = i32::try_from(offset).map_err(|_| EmitError::AbiOffsetOutOfRange)?;
            self.builder
                .ins()
                .store(MemFlagsData::new(), value, self.slots, at);
            return Ok(());
        }
        let variable = self.home_at(offset)?;
        self.builder.def_var(variable, value);
        Ok(())
    }

    fn words(&mut self, plan: &PreparedFunction, range: WordRange) -> Result<(), EmitError> {
        for &op in plan.words(range) {
            match op {
                WordOp::Copy {
                    destination,
                    source,
                } => {
                    let value = self.load_at(source.0)?;
                    self.store_at(destination.0, value)?;
                }
                WordOp::Constant { destination, value } => {
                    let value = self.builder.ins().iconst(types::I64, word(value));
                    self.store_at(destination.0, value)?;
                }
            }
        }
        Ok(())
    }

    fn home_at(&self, offset: u32) -> Result<Variable, EmitError> {
        let offset = usize::try_from(offset).map_err(|_| EmitError::AbiOffsetOutOfRange)?;
        if offset % self.abi.slot_bytes != 0 {
            return Err(EmitError::AbiOffsetOutOfRange);
        }
        self.homes
            .get(offset / self.abi.slot_bytes)
            .copied()
            .ok_or(EmitError::AbiOffsetOutOfRange)
    }

    fn spill(&mut self, site: &Site, plan: &PreparedFunction) -> Result<(), EmitError> {
        for &slot in &site.recover {
            if let RecoverySlot::Initialized(home) = slot {
                let words = match home.layout {
                    Some(id) => plan
                        .layout(id)
                        .ok_or(EmitError::AbiOffsetOutOfRange)?
                        .words(),
                    None => 1,
                };
                for word in 0..words {
                    let at = home.offset.0 + word * 8;
                    if self.borrowed.contains(&at) {
                        continue;
                    }
                    let value = self.load_at(at)?;
                    let offset = i32::try_from(at).map_err(|_| EmitError::AbiOffsetOutOfRange)?;
                    self.builder
                        .ins()
                        .store(MemFlagsData::new(), value, self.slots, offset);
                }
            }
        }
        Ok(())
    }

    fn arrive(&mut self, pc: InstructionId, edge: Edge) {
        let predecessor = self.builder.ins().iconst(types::I64, i64::from(pc.0));
        self.builder.def_var(self.predecessor, predecessor);
        let edge = self.builder.ins().iconst(types::I64, word(edge as u64));
        self.builder.def_var(self.edge, edge);
    }

    fn store_context(&mut self, field: usize, value: u64) -> Result<(), EmitError> {
        let value = self.builder.ins().iconst(types::I64, word(value));
        self.builder
            .ins()
            .store(MemFlagsData::new(), value, self.context, offset(field)?);
        Ok(())
    }

    fn fail(&mut self, pc: InstructionId, failure: Failure) -> Result<(), EmitError> {
        self.store_context(self.abi.failure, failure as u64)?;
        self.exit_at(ScalarOutcome::Failed, pc)
    }

    fn exit_at(&mut self, outcome: ScalarOutcome, pc: InstructionId) -> Result<(), EmitError> {
        self.store_context(self.abi.next, u64::from(pc.0))?;
        self.exit(outcome)
    }

    fn exit(&mut self, outcome: ScalarOutcome) -> Result<(), EmitError> {
        for (field, variable) in [
            (self.abi.budget, self.budget),
            (self.abi.predecessor, self.predecessor),
            (self.abi.edge, self.edge),
        ] {
            let value = self.builder.use_var(variable);
            self.builder
                .ins()
                .store(MemFlagsData::new(), value, self.context, offset(field)?);
        }
        let value = self.builder.ins().iconst(types::I64, word(outcome as u64));
        self.builder.ins().return_(&[value]);
        Ok(())
    }
}

fn word(value: u64) -> i64 {
    i64::from_ne_bytes(value.to_ne_bytes())
}

fn offset(value: usize) -> Result<i32, EmitError> {
    i32::try_from(value).map_err(|_| EmitError::AbiOffsetOutOfRange)
}

fn condition(operation: IntegerOp) -> IntCC {
    match operation {
        IntegerOp::Less => IntCC::SignedLessThan,
        IntegerOp::LessEqual => IntCC::SignedLessThanOrEqual,
        IntegerOp::Greater => IntCC::SignedGreaterThan,
        IntegerOp::GreaterEqual => IntCC::SignedGreaterThanOrEqual,
        IntegerOp::Equal => IntCC::Equal,
        IntegerOp::NotEqual => IntCC::NotEqual,
        _ => unreachable!("only comparisons select a condition code"),
    }
}
