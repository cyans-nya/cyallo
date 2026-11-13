//! Baseline lowering: canonical slots at every bytecode boundary, native edges.
use crate::backend::abi::{native_enter, native_flush, native_step};
use crate::{
    Type,
    ir::{Op, Program},
    runtime::native::{self, ExitReason, NativeView},
};
use cyallo_ast::Literal;
use cyallo_ast::Operator;
use std::{collections::BTreeMap, mem::offset_of};
use winch_codegen::native::{
    Arithmetic as A, Assembler, Condition as C, FloatArithmetic as F, Label,
};
// Saved: Session, NativeView, fuel, pending, entry depth. Others are scratch.
const V: u8 = 1;
const FUEL: u8 = 2;
const P: u8 = 3;
const DEPTH: u8 = 13;
fn scalar(t: &Type) -> bool {
    matches!(
        t,
        Type::Integer
            | Type::Real
            | Type::Boolean
            | Type::Char
            | Type::Context
            | Type::Registration
            | Type::ServiceKey(_)
            | Type::ServiceRef(_)
            | Type::TraitObject(_)
    )
}
fn tag(t: &Type) -> u64 {
    match t {
        Type::Integer => native::INTEGER,
        Type::Real => native::REAL,
        Type::Boolean => native::BOOLEAN,
        Type::Char => native::CHAR,
        Type::Context => native::CONTEXT,
        Type::Registration => native::REGISTRATION,
        Type::ServiceKey(_) => native::SERVICE_KEY,
        Type::ServiceRef(_) => native::SERVICE_REF,
        Type::TraitObject(_) => native::TRAIT_OBJECT,
        _ => unreachable!(),
    }
}
fn comparison(op: cyallo_ast::Operator) -> C {
    match op {
        Operator::Equal => C::Eq,
        Operator::NotEqual => C::Ne,
        Operator::Less => C::Lt,
        Operator::LessEqual => C::Le,
        Operator::Greater => C::Gt,
        Operator::GreaterEqual => C::Ge,
        _ => unreachable!(),
    }
}
struct Emit {
    a: Assembler,
    targets: Vec<Label>,
    errors: BTreeMap<(usize, u64), Label>,
    // Write-through cache: all values remain canonical for errors/OSR. A
    // straight-line successor can reuse the last scalar in scratch register 7.
    cached: Option<usize>,
}
impl Emit {
    fn trait_target(&mut self, pc: usize, target: usize, ty: &Type) {
        use crate::runtime::services::{CallCache, NativeBinding, TargetCase};
        let done = self.a.label();
        self.read(target, 4);
        self.imm(5, 1u64 << 32);
        self.a.divide(6, 4, 5);
        if matches!(ty, Type::ServiceRef(_)) {
            self.field(8, offset_of!(NativeView, service_tables));
            self.a.shift(6, 6, 3);
            self.a.binary(A::Add, 8, 8, 6);
            self.a.load(8, 8, 0);
            self.imm(5, u32::MAX as u64);
            self.a.binary(A::And, 4, 4, 5);
            self.a.shift(4, 4, 5);
            self.a.binary(A::Add, 8, 8, 4);
            self.a
                .load(5, 8, offset_of!(NativeBinding, available) as u32);
            self.zero(5, C::Eq, done);
            self.a
                .load(6, 8, offset_of!(NativeBinding, implementation) as u32);
        }
        self.inc(6, 1);
        self.field(8, offset_of!(NativeView, call_caches));
        self.imm(5, (pc * size_of::<CallCache>()) as u64);
        self.a.binary(A::Add, 8, 8, 5);
        for i in 0..4 {
            let next = self.a.label();
            let offset = (offset_of!(CallCache, cases) + i * size_of::<TargetCase>()) as u32;
            self.a.load(10, 8, offset);
            self.a.compare(10, 6);
            self.a.branch(C::Ne, next);
            self.a.load(10, 8, offset + 8);
            self.inc(10, 1);
            self.a
                .store(10, V, offset_of!(NativeView, trait_hint_target) as u32);
            self.a.jump(done);
            self.a.bind(next);
        }
        self.a.bind(done);
        self.cached = None;
    }
    fn imm(&mut self, r: u8, n: u64) {
        if r == 7 {
            self.cached = None;
        }
        self.a.imm(r, n);
    }
    fn field(&mut self, r: u8, offset: usize) {
        if r == 7 {
            self.cached = None;
        }
        self.a.load(r, V, offset as u32);
    }
    fn inc(&mut self, r: u8, n: i64) {
        self.imm(12, n as u64);
        self.a.binary(A::Add, r, r, 12);
    }
    fn zero(&mut self, r: u8, condition: C, label: Label) {
        self.imm(12, 0);
        self.a.compare(r, 12);
        self.a.branch(condition, label);
    }
    fn regptr(&mut self, r: usize, dst: u8) {
        self.field(dst, offset_of!(NativeView, registers));
        if r != 0 {
            self.imm(12, (r * 16) as u64);
            self.a.binary(A::Add, dst, dst, 12);
        }
    }
    fn read(&mut self, r: usize, dst: u8) {
        if self.cached == Some(r) {
            if dst != 7 {
                self.a.mov(dst, 7);
            }
            return;
        }
        self.regptr(r, 10);
        self.a.load(dst, 10, 0);
        if dst == 7 {
            self.cached = Some(r);
        }
    }
    fn write(&mut self, r: usize, value: u8, kind: u8) {
        self.a.mov(9, kind);
        self.imm(12, native::DIRTY);
        self.a.binary(A::Or, 9, 9, 12);
        self.regptr(r, 10);
        self.a.store(value, 10, 0);
        self.a.store(9, 10, 8);
        self.cached = if value == 7 { Some(r) } else { None };
    }
    fn result(&mut self, r: usize, value: u8, kind: u64) {
        self.imm(8, kind);
        self.write(r, value, 8);
    }
    fn cell(&mut self, r: usize) {
        self.read(r, 4);
        self.field(5, offset_of!(NativeView, cells));
        self.a.shift(6, 4, 3);
        self.a.binary(A::Add, 6, 5, 6);
        self.a.load(6, 6, 0);
    }
    fn save(&mut self, pc: usize) {
        self.field(4, offset_of!(NativeView, pc));
        self.imm(5, pc as u64);
        self.a.store(5, 4, 0);
        self.a.store(P, V, offset_of!(NativeView, pending) as u32);
    }
    fn exit(&mut self, pc: usize, reason: ExitReason, done: Label) {
        self.save(pc);
        self.imm(4, reason as u64);
        self.a
            .store(4, V, offset_of!(NativeView, exit_reason) as u32);
        self.a.jump(done);
    }
    fn poll(&mut self, pc: usize, header: bool, done: Label) {
        let ready = self.a.label();
        let hot = self.a.label();
        let end = self.a.label();
        if header {
            let no_osr = self.a.label();
            self.field(4, offset_of!(NativeView, osr_requested));
            self.zero(4, C::Eq, no_osr);
            self.exit(pc, ExitReason::Osr, done);
            self.a.bind(no_osr);
        }
        self.field(4, offset_of!(NativeView, transition_remaining));
        self.a.compare(P, 4);
        self.a.branch(C::Hs, hot);
        self.field(4, offset_of!(NativeView, next_poll));
        self.a.compare(P, 4);
        self.a.branch(C::Lo, end);
        self.field(4, offset_of!(NativeView, control_epoch));
        let unchanged = self.a.label();
        self.zero(4, C::Eq, unchanged);
        // The aligned epoch load is only a notification. State.poll performs
        // the acquire operation before reading the completion mailbox.
        self.a.load(4, 4, 0);
        self.field(5, offset_of!(NativeView, observed_epoch));
        self.a.compare(4, 5);
        self.a.branch(C::Ne, ready);
        self.a.bind(unchanged);
        self.field(4, offset_of!(NativeView, poll_interval));
        self.a.binary(A::Add, 4, P, 4);
        self.a.store(4, V, offset_of!(NativeView, next_poll) as u32);
        self.a.jump(end);
        self.a.bind(ready);
        self.exit(pc, ExitReason::CodeReady, done);
        self.a.bind(hot);
        self.exit(pc, ExitReason::Hot, done);
        self.a.bind(end);
    }
    fn error(&mut self, pc: usize, code: u64) -> Label {
        if let Some(label) = self.errors.get(&(pc, code)) {
            *label
        } else {
            let label = self.a.label();
            self.errors.insert((pc, code), label);
            label
        }
    }
    fn float(&mut self, r: usize, ty: &Type, dst: u8) {
        self.read(r, 7);
        if *ty == Type::Integer {
            self.a.int_to_float(dst, 7);
        } else {
            self.a.bits_to_float(dst, 7);
        }
    }
    fn bool_result(&mut self, r: usize, yes: Label, end: Label) {
        self.imm(7, 0);
        self.a.jump(end);
        self.a.bind(yes);
        self.imm(7, 1);
        self.a.bind(end);
        self.result(r, 7, native::BOOLEAN);
    }
    fn inline(&mut self, fi: usize, pc: usize, op: &Op, program: &Program) -> bool {
        let types = &program.functions[fi].registers;
        match op {
            Op::ServiceKey(r, key) => {
                self.imm(7, *key as u64);
                self.result(*r, 7, native::SERVICE_KEY);
            }
            Op::Service(r, crate::services::Operation::Reference, args) => {
                self.read(args[0], 7);
                self.read(args[1], 8);
                self.a.shift(7, 7, 32);
                self.a.binary(A::Or, 7, 7, 8);
                self.result(*r, 7, native::SERVICE_REF);
            }
            Op::Statement | Op::Nop => {}
            Op::Literal(r, l) => {
                let (value, kind) = match l {
                    Literal::Integer(n) => (*n as u64, native::INTEGER),
                    Literal::Real(n) => (n.to_bits(), native::REAL),
                    Literal::Boolean(b) => (u64::from(*b), native::BOOLEAN),
                    Literal::Char(c) => (*c as u64, native::CHAR),
                    _ => return false,
                };
                self.imm(7, value);
                self.result(*r, 7, kind);
            }
            Op::Address(r, slot) => {
                self.field(
                    7,
                    if slot.global {
                        offset_of!(NativeView, globals)
                    } else {
                        offset_of!(NativeView, locals)
                    },
                );
                let index = if slot.global {
                    slot.index
                } else {
                    program.functions[fi].cell_slot(slot.index)
                };
                self.imm(8, (index * 8) as u64);
                self.a.binary(A::Add, 7, 7, 8);
                self.a.load(7, 7, 0);
                self.result(*r, 7, native::ADDRESS);
            }
            Op::Load(r, a) | Op::FrameLoad(r, a) if scalar(&types[*r]) => {
                if matches!(op, Op::FrameLoad(..)) {
                    self.regptr(*a, 6);
                } else {
                    self.cell(*a);
                }
                self.a.load(8, 6, 8);
                let missing = self.error(pc, 2);
                self.zero(8, C::Eq, missing);
                self.a.load(7, 6, 0);
                self.write(*r, 7, 8);
            }
            Op::Store(a, r) | Op::FrameStore(a, r) if scalar(&types[*r]) => {
                let frame = matches!(op, Op::FrameStore(..));
                if !frame {
                    self.cell(*a);
                }
                self.read(*r, 7);
                if *types[*a].pointee_or_self() == Type::Real && types[*r] == Type::Integer {
                    self.a.int_to_float(0, 7);
                    self.a.float_to_bits(7, 0);
                    self.cached = None;
                }
                if frame {
                    self.result(*a, 7, tag(types[*a].pointee_or_self()));
                } else {
                    self.a.load(8, 6, 8);
                    self.imm(12, native::DIRTY);
                    self.a.binary(A::And, 9, 8, 12);
                    let write = self.a.label();
                    self.zero(9, C::Ne, write);
                    self.field(5, offset_of!(NativeView, dirty_ids));
                    self.field(9, offset_of!(NativeView, dirty_count));
                    self.a.shift(8, 9, 3);
                    self.a.binary(A::Add, 8, 5, 8);
                    self.a.store(4, 8, 0);
                    self.inc(9, 1);
                    self.a
                        .store(9, V, offset_of!(NativeView, dirty_count) as u32);
                    self.a.bind(write);
                    self.a.store(7, 6, 0);
                    self.imm(8, tag(types[*a].pointee_or_self()) | native::DIRTY);
                    self.a.store(8, 6, 8);
                }
            }
            Op::Move(r, a) if scalar(&types[*r]) => {
                self.read(*a, 7);
                self.regptr(*a, 10);
                self.a.load(8, 10, 8);
                self.write(*r, 7, 8);
            }
            Op::Unary(r, op, a) if scalar(&types[*a]) => {
                self.read(*a, 7);
                match *op {
                    Operator::Subtract if types[*a] == Type::Integer => {
                        self.a.mov(8, 7);
                        self.imm(7, 0);
                        let overflow = self.error(pc, 1);
                        self.a.checked(A::Sub, 7, 7, 8, overflow);
                    }
                    Operator::Subtract => {
                        self.imm(8, 1 << 63);
                        self.a.binary(A::Xor, 7, 7, 8);
                    }
                    Operator::Not => {
                        self.imm(8, 1);
                        self.a.binary(A::Xor, 7, 7, 8);
                    }
                    _ => {}
                }
                self.result(*r, 7, tag(&types[*r]));
            }
            Op::Binary(r, op, a, b)
                if scalar(&types[*a]) && scalar(&types[*b]) && *op != Operator::Concat =>
            {
                let floating =
                    types[*a] == Type::Real || types[*b] == Type::Real || *op == Operator::Divide;
                if floating {
                    self.float(*a, &types[*a], 0);
                    self.float(*b, &types[*b], 1);
                    match *op {
                        Operator::Add
                        | Operator::Subtract
                        | Operator::Multiply
                        | Operator::Divide => {
                            if *op == Operator::Divide {
                                self.imm(7, 0);
                                self.a.bits_to_float(2, 7);
                                let zero = self.error(pc, 4);
                                self.a.float_branch(1, 2, C::Eq, zero);
                            }
                            self.a.float_binary(
                                match *op {
                                    Operator::Add => F::Add,
                                    Operator::Subtract => F::Sub,
                                    Operator::Multiply => F::Mul,
                                    _ => F::Div,
                                },
                                0,
                                0,
                                1,
                            );
                            self.a.float_to_bits(7, 0);
                            self.result(*r, 7, native::REAL);
                        }
                        _ => {
                            let yes = self.a.label();
                            let end = self.a.label();
                            self.a.float_branch(0, 1, comparison(*op), yes);
                            self.bool_result(*r, yes, end);
                        }
                    }
                } else {
                    self.read(*a, 7);
                    self.read(*b, 8);
                    match *op {
                        Operator::Add | Operator::Subtract | Operator::Multiply => {
                            let overflow = self.error(pc, 1);
                            self.a.checked(
                                match *op {
                                    Operator::Add => A::Add,
                                    Operator::Subtract => A::Sub,
                                    _ => A::Mul,
                                },
                                7,
                                7,
                                8,
                                overflow,
                            );
                            self.result(*r, 7, native::INTEGER);
                        }
                        Operator::IntegerDivide | Operator::Remainder => {
                            let zero = self.error(pc, 4);
                            self.zero(8, C::Eq, zero);
                            let divide = self.a.label();
                            let end = self.a.label();
                            self.imm(9, i64::MIN as u64);
                            self.a.compare(7, 9);
                            self.a.branch(C::Ne, divide);
                            self.imm(9, u64::MAX);
                            self.a.compare(8, 9);
                            self.a.branch(C::Ne, divide);
                            if *op == Operator::IntegerDivide {
                                let overflow = self.error(pc, 1);
                                self.a.jump(overflow);
                            } else {
                                self.imm(7, 0);
                                self.a.jump(end);
                            }
                            self.a.bind(divide);
                            if *op == Operator::IntegerDivide {
                                self.a.divide(7, 7, 8);
                            } else {
                                self.a.divide(9, 7, 8);
                                self.a.binary(A::Mul, 9, 9, 8);
                                self.a.binary(A::Sub, 7, 7, 9);
                            }
                            self.a.bind(end);
                            self.result(*r, 7, native::INTEGER);
                        }
                        _ => {
                            let yes = self.a.label();
                            let end = self.a.label();
                            self.a.compare(7, 8);
                            self.a.branch(comparison(*op), yes);
                            self.bool_result(*r, yes, end);
                        }
                    }
                }
            }
            Op::CheckStep(r) => {
                self.read(*r, 7);
                let zero = self.error(pc, 3);
                self.zero(7, C::Eq, zero);
            }
            Op::Jump(target) => {
                self.a.jump(self.targets[*target]);
                return true;
            }
            Op::Branch(r, yes, no) => {
                self.read(*r, 7);
                self.zero(7, C::Ne, self.targets[*yes]);
                self.a.jump(self.targets[*no]);
                return true;
            }
            _ => return false,
        }
        self.a.jump(self.targets[pc + 1]);
        true
    }
}
pub(super) fn compile(program: &Program, fi: usize, version: u64) -> Result<Vec<u8>, String> {
    let mut a = Assembler::new();
    let done = a.label();
    let dispatch = a.label();
    let landings: Vec<_> = (0..program.functions[fi].code.len())
        .map(|_| a.label())
        .collect();
    let targets = (0..program.functions[fi].code.len())
        .map(|_| a.label())
        .collect();
    let mut e = Emit {
        a,
        targets,
        errors: BTreeMap::new(),
        cached: None,
    };
    e.a.prologue();
    e.field(DEPTH, offset_of!(NativeView, depth));
    e.imm(4, 1);
    e.a.store(4, V, offset_of!(NativeView, engine) as u32);
    e.a.bind(dispatch);
    e.imm(4, version);
    e.a.store(4, V, offset_of!(NativeView, code_version) as u32);
    e.field(4, offset_of!(NativeView, exit_reason));
    e.zero(4, C::Ne, done);
    e.zero(FUEL, C::Eq, done);
    e.field(4, offset_of!(NativeView, halted));
    e.zero(4, C::Ne, done);
    e.field(4, offset_of!(NativeView, depth));
    e.a.compare(4, DEPTH);
    e.a.branch(C::Lo, done);
    e.field(4, offset_of!(NativeView, function));
    e.imm(5, fi as u64);
    e.a.compare(4, 5);
    e.a.branch(C::Ne, done);
    let unobserved = e.a.label();
    e.field(4, offset_of!(NativeView, reporting));
    e.zero(4, C::Eq, unobserved);
    e.a.call(native_enter as *const () as usize as u64);
    e.a.bind(unobserved);
    e.field(4, offset_of!(NativeView, pc));
    e.a.load(4, 4, 0);
    for (pc, landing) in landings.iter().enumerate() {
        e.imm(5, pc as u64);
        e.a.compare(4, 5);
        e.a.branch(C::Eq, *landing);
    }
    e.a.jump(done);
    let mut recovery = Vec::with_capacity(e.targets.len());
    for pc in 0..e.targets.len() {
        if program.functions[fi].resume.poll_points[pc] {
            e.cached = None;
        }
        recovery.push(e.cached);
        e.a.bind(e.targets[pc]);
        let execute = e.a.label();
        e.zero(FUEL, C::Ne, execute);
        e.exit(pc, ExitReason::Budget, done);
        e.a.bind(execute);
        if program.functions[fi].resume.poll_points[pc] {
            e.poll(pc, program.functions[fi].resume.accepts(pc), done);
        }
        let op = program.bytecode.functions[fi].decode(pc);
        // Reserve this instruction, then undo for a helper (which counts itself).
        e.inc(FUEL, -1);
        e.inc(P, 1);
        let effects = program.functions[fi].gc.instructions[pc];
        if !effects.may_collect && !effects.may_reenter && e.inline(fi, pc, &op, program) {
            if matches!(op, Op::Jump(..) | Op::Branch(..)) {
                e.cached = None;
            }
            continue;
        }
        e.cached = None;
        e.inc(P, -1);
        e.save(pc);
        if let Op::TraitCall(_, target, _, _) = op {
            e.trait_target(pc, target, &program.functions[fi].registers[target]);
        }
        e.a.call(native_step as *const () as usize as u64);
        e.imm(P, 0);
        if matches!(
            op,
            Op::Call(..)
                | Op::TraitCall(..)
                | Op::Invoke(..)
                | Op::Lookup(_, crate::lookup::Operation::Mapped, _)
        ) {
            let resume = e.a.label();
            e.field(4, offset_of!(NativeView, entries));
            e.zero(4, C::Eq, resume);
            if let Op::Call(_, callee, _) = op {
                e.imm(5, (callee * 8) as u64);
            } else {
                e.field(5, offset_of!(NativeView, function));
                e.a.shift(5, 5, 3);
            }
            e.a.binary(A::Add, 4, 4, 5);
            e.a.load(10, 4, 0);
            e.zero(10, C::Eq, resume);
            e.field(4, offset_of!(NativeView, halted));
            e.zero(4, C::Ne, resume);
            e.field(4, offset_of!(NativeView, native_depth));
            e.imm(5, 64);
            e.a.compare(4, 5);
            e.a.branch(C::Hs, resume);
            e.inc(4, 1);
            e.a.store(4, V, offset_of!(NativeView, native_depth) as u32);
            e.a.call_entry(10);
            e.field(4, offset_of!(NativeView, native_depth));
            e.inc(4, -1);
            e.a.store(4, V, offset_of!(NativeView, native_depth) as u32);
            e.a.call(native_flush as *const () as usize as u64);
            e.imm(4, 1);
            e.a.store(4, V, offset_of!(NativeView, engine) as u32);
            e.a.bind(resume);
        }
        e.a.jump(dispatch);
    }
    // Arbitrary-PC entries restore the cache expected by that instruction.
    // Normal edges bypass these stubs and retain the machine register value.
    for (pc, cached) in recovery.into_iter().enumerate() {
        e.a.bind(landings[pc]);
        e.cached = None;
        if let Some(reg) = cached {
            e.read(reg, 7);
        }
        e.a.jump(e.targets[pc]);
    }
    let errors = std::mem::take(&mut e.errors);
    for ((pc, code), label) in errors {
        e.a.bind(label);
        e.save(pc);
        e.imm(4, code);
        e.a.store(4, V, offset_of!(NativeView, error) as u32);
        e.imm(4, ExitReason::Error as u64);
        e.a.store(4, V, offset_of!(NativeView, exit_reason) as u32);
        e.a.jump(done);
    }
    e.a.bind(done);
    e.a.epilogue();
    Ok(e.a.finish())
}
