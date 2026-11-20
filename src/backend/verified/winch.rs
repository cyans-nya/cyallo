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
    use super::super::{LoanBase, UnaryOp, WordOp, WordRange};
    use crate::bytecode::verified::{BooleanOp, FloatOp, InstructionId, IntegerOp, Scalar};
    use std::{collections::BTreeMap, sync::Arc};
    use winch_codegen::native::{Arithmetic, Assembler, Condition, FloatArithmetic, Label};

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
                Float {
                    operation,
                    destination,
                    left,
                    right,
                } => {
                    self.load_home(LEFT, left);
                    self.load_home(RIGHT, right);
                    self.float(operation, pc)?;
                    self.store_home(VALUE, destination);
                }
                Boolean {
                    operation,
                    destination,
                    left,
                    right,
                } => {
                    self.load_home(LEFT, left);
                    self.load_home(RIGHT, right);
                    let condition = match operation {
                        BooleanOp::Equal => Condition::Eq,
                        BooleanOp::NotEqual => Condition::Ne,
                    };
                    self.boolean_from(condition);
                    self.store_home(VALUE, destination);
                }
                RealDivide {
                    destination,
                    left,
                    right,
                } => {
                    self.load_home(LEFT, left);
                    self.load_home(RIGHT, right);
                    let zero = self.machine.label();
                    let done = self.machine.label();
                    self.machine.imm(TEMP, 0);
                    self.machine.compare(RIGHT, TEMP);
                    self.machine.branch(Condition::Eq, zero);
                    self.machine.int_to_float(0, LEFT);
                    self.machine.int_to_float(1, RIGHT);
                    self.machine.float_binary(FloatArithmetic::Div, 2, 0, 1);
                    self.machine.float_to_bits(VALUE, 2);
                    self.machine.jump(done);
                    self.machine.bind(zero);
                    self.fail(pc, Failure::DivisionByZero)?;
                    self.machine.bind(done);
                    self.store_home(VALUE, destination);
                }
                Unary {
                    operation,
                    destination,
                    source,
                } => {
                    self.load_home(LEFT, source);
                    self.unary(operation, pc)?;
                    self.store_home(VALUE, destination);
                }
                Words(range) => self.words(plan, range),
                Borrow { destination, owner } => {
                    self.machine.imm(TEMP, u64::from(owner.0));
                    self.machine.binary(Arithmetic::Add, VALUE, SLOTS, TEMP);
                    self.store_home(VALUE, destination);
                }
                Project {
                    destination,
                    base,
                    offset,
                    variant,
                } => {
                    match base {
                        LoanBase::Loan(home) => self.load_home(LEFT, home),
                        LoanBase::Owner(owner) => {
                            self.machine.imm(TEMP, u64::from(owner.0));
                            self.machine.binary(Arithmetic::Add, LEFT, SLOTS, TEMP);
                        }
                    }
                    if let Some(variant) = variant {
                        let held = self.machine.label();
                        self.machine.load(VALUE, LEFT, 0);
                        self.machine.imm(TEMP, u64::from(variant));
                        self.machine.compare(VALUE, TEMP);
                        self.machine.branch(Condition::Eq, held);
                        self.fail(pc, Failure::EnumVariant)?;
                        self.machine.bind(held);
                    }
                    self.machine.imm(TEMP, u64::from(offset));
                    self.machine.binary(Arithmetic::Add, VALUE, LEFT, TEMP);
                    self.store_home(VALUE, destination);
                }
                ReadLoan {
                    destination,
                    loan,
                    words,
                } => {
                    self.load_home(LEFT, loan);
                    for word in 0..words {
                        let (base, offset) = self.through(LEFT, word * 8);
                        self.machine.load(VALUE, base, offset);
                        self.store_at(VALUE, destination.0 + word * 8);
                    }
                }
                WriteLoan {
                    loan,
                    source,
                    words,
                } => {
                    self.load_home(LEFT, loan);
                    for word in 0..words {
                        self.load_at(VALUE, source.0 + word * 8);
                        let (base, offset) = self.through(LEFT, word * 8);
                        self.machine.store(VALUE, base, offset);
                    }
                }
                NonZero(step) => {
                    let zero = self.machine.label();
                    let done = self.machine.label();
                    self.load_home(LEFT, step);
                    self.machine.imm(TEMP, 0);
                    self.machine.compare(LEFT, TEMP);
                    self.machine.branch(Condition::Eq, zero);
                    self.machine.jump(done);
                    self.machine.bind(zero);
                    self.fail(pc, Failure::ZeroStep)?;
                    self.machine.bind(done);
                }
                TagIs {
                    destination,
                    base,
                    variant,
                } => {
                    let is_true = self.machine.label();
                    let done = self.machine.label();
                    match base {
                        LoanBase::Owner(tag) => self.load_at(LEFT, tag.0),
                        LoanBase::Loan(home) => {
                            self.load_home(LEFT, home);
                            self.machine.load(LEFT, LEFT, 0);
                        }
                    }
                    self.machine.imm(RIGHT, u64::from(variant));
                    self.machine.compare(LEFT, RIGHT);
                    self.machine.branch(Condition::Eq, is_true);
                    self.machine.imm(VALUE, 0);
                    self.machine.jump(done);
                    self.machine.bind(is_true);
                    self.machine.imm(VALUE, 1);
                    self.machine.bind(done);
                    self.store_home(VALUE, destination);
                }
                Variant { tag, variant, then } => {
                    let held = self.machine.label();
                    self.load_at(LEFT, tag.0);
                    self.machine.imm(RIGHT, u64::from(variant));
                    self.machine.compare(LEFT, RIGHT);
                    self.machine.branch(Condition::Eq, held);
                    self.fail(pc, Failure::EnumVariant)?;
                    self.machine.bind(held);
                    self.words(plan, then);
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

        /// VALUE = 1 when LEFT and RIGHT satisfy the integer condition, else 0.
        fn boolean_from(&mut self, condition: Condition) {
            let is_true = self.machine.label();
            let done = self.machine.label();
            self.machine.compare(LEFT, RIGHT);
            self.machine.branch(condition, is_true);
            self.machine.imm(VALUE, 0);
            self.machine.jump(done);
            self.machine.bind(is_true);
            self.machine.imm(VALUE, 1);
            self.machine.bind(done);
        }

        fn float(&mut self, operation: FloatOp, pc: InstructionId) -> Result<(), EmitError> {
            use FloatOp::*;
            self.machine.bits_to_float(0, LEFT);
            self.machine.bits_to_float(1, RIGHT);
            let arithmetic = match operation {
                Add => Some(FloatArithmetic::Add),
                Subtract => Some(FloatArithmetic::Sub),
                Multiply => Some(FloatArithmetic::Mul),
                Divide | CheckedDivide => Some(FloatArithmetic::Div),
                Remainder => return Err(EmitError::UnsupportedOperation(pc)),
                _ => None,
            };
            if let Some(arithmetic) = arithmetic {
                let done = self.machine.label();
                if operation == CheckedDivide {
                    let zero = self.machine.label();
                    self.machine.imm(TEMP, 0);
                    self.machine.bits_to_float(3, TEMP);
                    self.machine.float_branch(1, 3, Condition::Eq, zero);
                    self.machine.float_binary(arithmetic, 2, 0, 1);
                    self.machine.float_to_bits(VALUE, 2);
                    self.machine.jump(done);
                    self.machine.bind(zero);
                    self.fail(pc, Failure::DivisionByZero)?;
                } else {
                    self.machine.float_binary(arithmetic, 2, 0, 1);
                    self.machine.float_to_bits(VALUE, 2);
                }
                self.machine.bind(done);
                return Ok(());
            }
            let condition = match operation {
                Less => Condition::Lt,
                LessEqual => Condition::Le,
                Greater => Condition::Gt,
                GreaterEqual => Condition::Ge,
                Equal => Condition::Eq,
                NotEqual => Condition::Ne,
                _ => unreachable!("arithmetic handled above"),
            };
            let is_true = self.machine.label();
            let done = self.machine.label();
            self.machine.float_branch(0, 1, condition, is_true);
            self.machine.imm(VALUE, 0);
            self.machine.jump(done);
            self.machine.bind(is_true);
            self.machine.imm(VALUE, 1);
            self.machine.bind(done);
            Ok(())
        }

        fn unary(&mut self, operation: UnaryOp, pc: InstructionId) -> Result<(), EmitError> {
            match operation {
                UnaryOp::NegateInteger => {
                    let minimum = self.machine.label();
                    let done = self.machine.label();
                    self.machine.imm(TEMP, scalar_word(Scalar::I64(i64::MIN)));
                    self.machine.compare(LEFT, TEMP);
                    self.machine.branch(Condition::Eq, minimum);
                    self.machine.imm(TEMP, 0);
                    self.machine.binary(Arithmetic::Sub, VALUE, TEMP, LEFT);
                    self.machine.jump(done);
                    self.machine.bind(minimum);
                    self.fail(pc, Failure::Overflow)?;
                    self.machine.bind(done);
                }
                UnaryOp::NegateFloat => {
                    self.machine.imm(TEMP, scalar_word(Scalar::I64(i64::MIN)));
                    self.machine.binary(Arithmetic::Xor, VALUE, LEFT, TEMP);
                }
                UnaryOp::Not => {
                    self.machine.imm(TEMP, 1);
                    self.machine.binary(Arithmetic::Xor, VALUE, LEFT, TEMP);
                }
                UnaryOp::IntegerToFloat => {
                    self.machine.int_to_float(0, LEFT);
                    self.machine.float_to_bits(VALUE, 0);
                }
                UnaryOp::FloatToInteger => return Err(EmitError::UnsupportedOperation(pc)),
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

        /// A base register and displacement for the word `offset` bytes past
        /// the address in `pointer`. Large displacements use the scratch register.
        fn through(&mut self, pointer: u8, offset: u32) -> (u8, u32) {
            if offset <= UNSCALED_DISPLACEMENT_MAX {
                (pointer, offset)
            } else {
                self.machine.imm(ADDRESS, u64::from(offset));
                self.machine
                    .binary(Arithmetic::Add, ADDRESS, pointer, ADDRESS);
                (ADDRESS, 0)
            }
        }

        fn load_home(&mut self, dst: u8, home: NativeHome) {
            self.load_at(dst, home.offset.0);
        }

        fn load_at(&mut self, dst: u8, offset: u32) {
            let (base, offset) = self.address(SLOTS, offset);
            self.machine.load(dst, base, offset);
        }

        fn store_at(&mut self, src: u8, offset: u32) {
            let (base, offset) = self.address(SLOTS, offset);
            self.machine.store(src, base, offset);
        }

        fn words(&mut self, plan: &PreparedFunction, range: WordRange) {
            for &op in plan.words(range) {
                match op {
                    WordOp::Copy {
                        destination,
                        source,
                    } => {
                        self.load_at(VALUE, source.0);
                        self.store_at(VALUE, destination.0);
                    }
                    WordOp::Constant { destination, value } => {
                        self.machine.imm(VALUE, value);
                        self.store_at(VALUE, destination.0);
                    }
                }
            }
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
