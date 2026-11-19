//! Cranelift translation over the shared scalar native plan.

use super::{
    AbiLayout, Architecture, CraneliftMachineInfo, CraneliftProfile, EmitError, Failure,
    MachineRange, MachineTrap, NativeHome, NativeTarget, NativeTier, Operation, Platform,
    PreparedFunction, RecoverySlot, ScalarOutcome, Site, UnlinkedImage, abi::Edge,
    image::checked_source,
};
use crate::bytecode::verified::{InstructionId, IntegerOp, Scalar};
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
                CraneliftProfile::Fast => "none",
                CraneliftProfile::Speed => "speed",
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
        self.spill(site)?;
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
                if let Some(home) = value {
                    let value = self.load(home)?;
                    self.builder
                        .ins()
                        .store(MemFlagsData::new(), value, self.result, 0);
                }
                self.exit_at(ScalarOutcome::Returned, pc)?;
            }
            Call(call_pc) => {
                let call = plan.call(call_pc).ok_or(EmitError::MissingCall(call_pc))?;
                self.spill(site)?;
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
        let variable = self.home(home)?;
        Ok(self.builder.use_var(variable))
    }

    fn store(&mut self, home: NativeHome, value: Value) -> Result<(), EmitError> {
        let variable = self.home(home)?;
        self.builder.def_var(variable, value);
        Ok(())
    }

    fn home(&self, home: NativeHome) -> Result<Variable, EmitError> {
        let offset = usize::try_from(home.offset.0).map_err(|_| EmitError::AbiOffsetOutOfRange)?;
        if offset % self.abi.slot_bytes != 0 {
            return Err(EmitError::AbiOffsetOutOfRange);
        }
        self.homes
            .get(offset / self.abi.slot_bytes)
            .copied()
            .ok_or(EmitError::AbiOffsetOutOfRange)
    }

    fn spill(&mut self, site: &Site) -> Result<(), EmitError> {
        for &slot in &site.recover {
            if let RecoverySlot::Initialized(home) = slot {
                let value = self.load(home)?;
                let offset =
                    i32::try_from(home.offset.0).map_err(|_| EmitError::AbiOffsetOutOfRange)?;
                self.builder
                    .ins()
                    .store(MemFlagsData::new(), value, self.slots, offset);
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
