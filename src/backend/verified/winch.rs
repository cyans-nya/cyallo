//! Winch selection over the shared scalar native plan.

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
pub(super) fn emit(
    plan: std::sync::Arc<super::PreparedFunction>,
) -> Result<super::UnlinkedImage, super::EmitError> {
    Err(super::EmitError::UnsupportedTarget(plan.target()))
}

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub(super) use host::emit;

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
mod host {
    use super::super::{
        AbiLayout, EmitError, Failure, MachineRange, NativeHome, NativeTarget, NativeTier,
        Operation, PreparedFunction, ScalarOutcome, Site, UnlinkedImage,
        abi::{Edge, scalar_word},
        image::checked_source,
    };
    use crate::bytecode::verified::{InstructionId, IntegerOp, Scalar};
    use std::{collections::BTreeMap, sync::Arc};
    use winch_codegen::native::{Arithmetic, Assembler, Condition, Label};

    // Logical Winch native-assembler registers. Its prologue saves context,
    // environment, and result-place operands as registers 0 through 2.
    const CONTEXT: u8 = 0;
    const RESULT_PLACE: u8 = 2;
    const SLOTS: u8 = 3;
    const LEFT: u8 = 4;
    const RIGHT: u8 = 5;
    const VALUE: u8 = 6;
    const TEMP: u8 = 7;
    const ADDRESS: u8 = 8;
    const SAVED_DIVIDEND: u8 = 9;

    // Winch's AArch64 load/store seam accepts a signed unscaled imm9.
    const UNSCALED_DISPLACEMENT_MAX: u32 = (1 << 8) - 1;

    struct Select {
        machine: Assembler,
        labels: BTreeMap<InstructionId, Label>,
        abi: AbiLayout,
        exit: Label,
    }

    pub(in crate::backend::verified) fn emit(
        plan: Arc<PreparedFunction>,
    ) -> Result<UnlinkedImage, EmitError> {
        if NativeTarget::host() != Some(plan.target()) {
            return Err(EmitError::UnsupportedTarget(plan.target()));
        }
        let mut machine = Assembler::new();
        let exit = machine.label();
        let labels = plan
            .sites()
            .iter()
            .map(|site| (site.origin.instruction, machine.label()))
            .collect();
        let mut select = Select {
            machine,
            labels,
            abi: plan.abi(),
            exit,
        };
        select.entry()?;
        for site in plan.sites() {
            select.site(site, &plan)?;
        }
        select.machine.bind(exit);
        select.machine.epilogue();
        let (bytes, ranges) = select
            .machine
            .finish_image()
            .ok_or(EmitError::UnexpectedRelocation)?;
        let ranges = ranges
            .into_iter()
            .map(|range| MachineRange {
                instruction: InstructionId(range.site),
                start: range.start,
                end: range.end,
            })
            .collect();
        Ok(UnlinkedImage {
            relocations: plan.relocations().collect(),
            plan,
            tier: NativeTier::Winch,
            bytes: bytes.into_boxed_slice(),
            ranges,
            cranelift: None,
        })
    }

    impl Select {
        fn label(&self, pc: InstructionId) -> Result<Label, EmitError> {
            self.labels
                .get(&pc)
                .copied()
                .ok_or(EmitError::MissingSite(pc))
        }

        fn load_context(&mut self, dst: u8, offset: usize) -> Result<(), EmitError> {
            self.machine.load(dst, CONTEXT, context_offset(offset)?);
            Ok(())
        }

        fn store_context(&mut self, src: u8, offset: usize) -> Result<(), EmitError> {
            self.machine.store(src, CONTEXT, context_offset(offset)?);
            Ok(())
        }

        fn entry(&mut self) -> Result<(), EmitError> {
            self.machine.prologue();
            self.load_context(SLOTS, self.abi.slots)?;
            self.load_context(VALUE, self.abi.next)?;
            for (&instruction, &label) in &self.labels {
                self.machine.imm(TEMP, u64::from(instruction.0));
                self.machine.compare(VALUE, TEMP);
                self.machine.branch(Condition::Eq, label);
            }
            self.machine.imm(TEMP, Failure::Trap as u64);
            self.store_context(TEMP, self.abi.failure)?;
            self.finish(ScalarOutcome::Failed);
            Ok(())
        }

        fn site(&mut self, site: &Site, plan: &PreparedFunction) -> Result<(), EmitError> {
            let pc = site.origin.instruction;
            self.machine.bind(self.label(pc)?);
            self.machine.start_site(checked_source(pc)?);
            let pause = self.machine.label();
            self.load_context(VALUE, self.abi.budget)?;
            self.machine.imm(TEMP, site.fuel);
            self.machine.compare(VALUE, TEMP);
            self.machine.branch(Condition::Lo, pause);
            self.machine.binary(Arithmetic::Sub, VALUE, VALUE, TEMP);
            self.store_context(VALUE, self.abi.budget)?;
            self.operation(site, plan)?;
            self.machine.bind(pause);
            self.exit_at(ScalarOutcome::Paused, pc)?;
            self.machine.end_site();
            Ok(())
        }

        fn operation(&mut self, site: &Site, plan: &PreparedFunction) -> Result<(), EmitError> {
            use Operation::*;
            let pc = site.origin.instruction;
            match site.operation {
                StorageLive(_) | StorageDead(_) => {}
                Constant { destination, value } => {
                    self.machine.imm(VALUE, scalar_word(value));
                    self.store_home(VALUE, destination);
                }
                Copy {
                    destination,
                    source,
                } => {
                    self.load_home(VALUE, source);
                    self.store_home(VALUE, destination);
                }
                Integer {
                    operation,
                    destination,
                    left,
                    right,
                } => {
                    self.load_home(LEFT, left);
                    self.load_home(RIGHT, right);
                    self.integer(operation, pc)?;
                    self.store_home(VALUE, destination);
                }
                Jump(to) => {
                    self.arrive(pc, Edge::Jump)?;
                    self.machine.jump(self.label(to)?);
                }
                Branch { condition, yes, no } => {
                    self.load_home(VALUE, condition);
                    self.machine.imm(TEMP, 0);
                    self.machine.compare(VALUE, TEMP);
                    let taken = self.machine.label();
                    self.machine.branch(Condition::Ne, taken);
                    self.arrive(pc, Edge::No)?;
                    self.machine.jump(self.label(no)?);
                    self.machine.bind(taken);
                    self.arrive(pc, Edge::Yes)?;
                    self.machine.jump(self.label(yes)?);
                }
                Return(value) => {
                    if let Some(home) = value {
                        self.load_home(VALUE, home);
                        self.machine.store(VALUE, RESULT_PLACE, 0);
                    }
                    self.exit_at(ScalarOutcome::Returned, pc)?;
                }
                Call(call_pc) => {
                    let call = plan.call(call_pc).ok_or(EmitError::MissingCall(call_pc))?;
                    self.load_context(LEFT, self.abi.links)?;
                    let (base, offset) = self.address(LEFT, call.link.offset.0);
                    self.machine.load(VALUE, base, offset);
                    self.store_context(VALUE, self.abi.callee)?;
                    self.exit_at(ScalarOutcome::Call, pc)?;
                }
                Trap => self.fail(pc, Failure::Trap)?,
            }
            if !matches!(site.operation, Call(_))
                && let Some(next) = site.successor
            {
                self.arrive(pc, Edge::Next)?;
                self.machine.jump(self.label(next)?);
            }
            Ok(())
        }

        fn integer(&mut self, operation: IntegerOp, pc: InstructionId) -> Result<(), EmitError> {
            use IntegerOp::*;
            match operation {
                Add => self.checked(Arithmetic::Add, pc)?,
                Subtract => self.checked(Arithmetic::Sub, pc)?,
                Multiply => self.checked(Arithmetic::Mul, pc)?,
                Divide | Remainder => self.division(operation, pc)?,
                comparison => {
                    let is_true = self.machine.label();
                    let done = self.machine.label();
                    self.machine.compare(LEFT, RIGHT);
                    self.machine.branch(condition(comparison), is_true);
                    self.machine.imm(VALUE, 0);
                    self.machine.jump(done);
                    self.machine.bind(is_true);
                    self.machine.imm(VALUE, 1);
                    self.machine.bind(done);
                }
            }
            Ok(())
        }

        fn checked(&mut self, operation: Arithmetic, pc: InstructionId) -> Result<(), EmitError> {
            let overflow = self.machine.label();
            let done = self.machine.label();
            self.machine
                .checked(operation, VALUE, LEFT, RIGHT, overflow);
            self.machine.jump(done);
            self.machine.bind(overflow);
            self.fail(pc, Failure::Overflow)?;
            self.machine.bind(done);
            Ok(())
        }

        fn division(&mut self, operation: IntegerOp, pc: InstructionId) -> Result<(), EmitError> {
            let zero = self.machine.label();
            let ordinary = self.machine.label();
            let done = self.machine.label();
            self.machine.imm(TEMP, 0);
            self.machine.compare(RIGHT, TEMP);
            self.machine.branch(Condition::Eq, zero);
            self.machine.imm(TEMP, scalar_word(Scalar::I64(i64::MIN)));
            self.machine.compare(LEFT, TEMP);
            self.machine.branch(Condition::Ne, ordinary);
            self.machine.imm(TEMP, scalar_word(Scalar::I64(-1)));
            self.machine.compare(RIGHT, TEMP);
            self.machine.branch(Condition::Ne, ordinary);
            match operation {
                IntegerOp::Divide => self.fail(pc, Failure::Overflow)?,
                IntegerOp::Remainder => {
                    self.machine.imm(VALUE, 0);
                    self.machine.jump(done);
                }
                _ => unreachable!("only division-family operations select this path"),
            }
            self.machine.bind(ordinary);
            self.machine.mov(SAVED_DIVIDEND, LEFT);
            self.machine.divide(VALUE, LEFT, RIGHT);
            if operation == IntegerOp::Remainder {
                self.machine.binary(Arithmetic::Mul, VALUE, VALUE, RIGHT);
                self.machine
                    .binary(Arithmetic::Sub, VALUE, SAVED_DIVIDEND, VALUE);
            }
            self.machine.jump(done);
            self.machine.bind(zero);
            self.fail(pc, Failure::DivisionByZero)?;
            self.machine.bind(done);
            Ok(())
        }

        fn fail(&mut self, pc: InstructionId, failure: Failure) -> Result<(), EmitError> {
            self.machine.imm(TEMP, failure as u64);
            self.store_context(TEMP, self.abi.failure)?;
            self.exit_at(ScalarOutcome::Failed, pc)
        }

        fn exit_at(&mut self, outcome: ScalarOutcome, pc: InstructionId) -> Result<(), EmitError> {
            self.machine.imm(TEMP, u64::from(pc.0));
            self.store_context(TEMP, self.abi.next)?;
            self.finish(outcome);
            Ok(())
        }

        fn finish(&mut self, outcome: ScalarOutcome) {
            self.machine.imm(RESULT_PLACE, outcome as u64);
            self.machine.jump(self.exit);
        }

        fn arrive(&mut self, pc: InstructionId, edge: Edge) -> Result<(), EmitError> {
            self.machine.imm(TEMP, u64::from(pc.0));
            self.store_context(TEMP, self.abi.predecessor)?;
            self.machine.imm(TEMP, edge as u64);
            self.store_context(TEMP, self.abi.edge)
        }

        fn address(&mut self, base: u8, offset: u32) -> (u8, u32) {
            if offset <= UNSCALED_DISPLACEMENT_MAX {
                (base, offset)
            } else {
                self.machine.imm(ADDRESS, u64::from(offset));
                self.machine.binary(Arithmetic::Add, ADDRESS, base, ADDRESS);
                (ADDRESS, 0)
            }
        }

        fn load_home(&mut self, dst: u8, home: NativeHome) {
            let (base, offset) = self.address(SLOTS, home.offset.0);
            self.machine.load(dst, base, offset);
        }

        fn store_home(&mut self, src: u8, home: NativeHome) {
            let (base, offset) = self.address(SLOTS, home.offset.0);
            self.machine.store(src, base, offset);
        }
    }

    fn context_offset(offset: usize) -> Result<u32, EmitError> {
        u32::try_from(offset)
            .ok()
            .filter(|offset| *offset <= UNSCALED_DISPLACEMENT_MAX)
            .ok_or(EmitError::AbiOffsetOutOfRange)
    }

    fn condition(operation: IntegerOp) -> Condition {
        match operation {
            IntegerOp::Less => Condition::Lt,
            IntegerOp::LessEqual => Condition::Le,
            IntegerOp::Greater => Condition::Gt,
            IntegerOp::GreaterEqual => Condition::Ge,
            IntegerOp::Equal => Condition::Eq,
            IntegerOp::NotEqual => Condition::Ne,
            _ => unreachable!("only comparisons select a condition code"),
        }
    }
}
