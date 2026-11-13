//! Typed register bytecode: fixed-width words, indexed branches, separate source maps.
use crate::{
    Span, Type,
    ir::{self, Op},
};
use cyallo_ast::Literal;
use cyallo_ast::Operator;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Opcode {
    Statement,
    Constant,
    AddressLocal,
    AddressGlobal,
    Load,
    Store,
    Move,
    Jump,
    Branch,
    Return,
    CheckStep,
    Length,
    Negate,
    Not,
    Plus,
    Add,
    Subtract,
    Multiply,
    Divide,
    IntegerDivide,
    Modulo,
    Equal,
    NotEqual,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    Concat,
    Complex,
    Nop,
    FrameLoad,
    FrameStore,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ScalarType {
    Integer,
    Real,
    Boolean,
    Char,
    Handle,
}
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Instruction {
    pub opcode: Opcode,
    pub ty: ScalarType,
    reserved: u16,
    pub a: u32,
    pub b: u32,
    pub c: u32,
}
const _: () = assert!(size_of::<Instruction>() == 16);
#[derive(Debug, Clone)]
pub struct Function {
    pub instructions: Box<[Instruction]>,
    pub spans: Box<[Span]>,
    complex: Box<[Op]>,
}
#[derive(Debug, Clone)]
pub struct Module {
    pub functions: Box<[Routine]>,
}
#[derive(Debug, Clone)]
pub struct Routine {
    source: ir::Routine,
    lowered: std::sync::Arc<std::sync::OnceLock<Function>>,
}
impl std::ops::Deref for Routine {
    type Target = Function;
    fn deref(&self) -> &Function {
        self.lowered.get_or_init(|| Function::lower(&self.source))
    }
}
fn scalar(ty: &Type) -> ScalarType {
    match ty {
        Type::Integer => ScalarType::Integer,
        Type::Real => ScalarType::Real,
        Type::Boolean => ScalarType::Boolean,
        Type::Char => ScalarType::Char,
        _ => ScalarType::Handle,
    }
}
impl Module {
    pub fn disassemble_unit(&self, fi: usize) -> String {
        use std::fmt::Write;
        let Some(function) = self.functions.get(fi) else {
            return String::new();
        };
        let mut out = format!("function f{fi} ({} words)\n", function.instructions.len());
        for (pc, word) in function.instructions.iter().enumerate() {
            writeln!(
                out,
                "  {pc:04} {:?}.{:?} {}, {}, {} ; {:?}",
                word.opcode,
                word.ty,
                word.a,
                word.b,
                word.c,
                function.decode(pc)
            )
            .unwrap();
        }
        out
    }
    pub(crate) fn lower(functions: &[ir::Routine], jobs: cyallo_compile_jobs::Jobs) -> Self {
        Self {
            functions: jobs
                .map(functions, 4, |_, _, f| {
                    let routine = Routine {
                        source: f.clone(),
                        lowered: Default::default(),
                    };
                    // Translate already checked units in parallel. Cold units retain
                    // their independently initialized semantic and bytecode OnceLocks.
                    if f.is_analyzed() {
                        let _ = &*routine;
                    }
                    routine
                })
                .into_boxed_slice(),
        }
    }
    pub fn disassemble(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        for (fi, function) in self.functions.iter().enumerate() {
            writeln!(
                out,
                "function f{fi} ({} words, {} pooled operations)",
                function.instructions.len(),
                function.complex.len()
            )
            .unwrap();
            for (pc, word) in function.instructions.iter().enumerate() {
                writeln!(
                    out,
                    "  {pc:04} {:?}.{:?} {}, {}, {}  ; {:?}",
                    word.opcode,
                    word.ty,
                    word.a,
                    word.b,
                    word.c,
                    function.decode(pc)
                )
                .unwrap();
            }
        }
        out
    }
}
impl Function {
    fn lower(function: &ir::Function) -> Self {
        use Opcode::*;
        let mut complex = vec![];
        let instructions = function
            .code
            .iter()
            .map(|inst| {
                let mut word = Instruction {
                    opcode: Complex,
                    ty: ScalarType::Handle,
                    reserved: 0,
                    a: 0,
                    b: 0,
                    c: 0,
                };
                let (opcode, a, b, c) = match &inst.op {
                    Op::Statement => (Statement, 0, 0, 0),
                    Op::Nop => (Nop, 0, 0, 0),
                    Op::Literal(r, value) => {
                        let (ty, bits) = match value {
                            Literal::Integer(n) => (ScalarType::Integer, *n as u64),
                            Literal::Real(n) => (ScalarType::Real, n.to_bits()),
                            Literal::Boolean(n) => (ScalarType::Boolean, u64::from(*n)),
                            Literal::Char(n) => (ScalarType::Char, *n as u64),
                            _ => {
                                let id = complex.len();
                                complex.push(inst.op.clone());
                                return Instruction {
                                    a: id as u32,
                                    ..word
                                };
                            }
                        };
                        word.ty = ty;
                        (Constant, *r as u32, bits as u32, (bits >> 32) as u32)
                    }
                    Op::Address(r, slot) => (
                        if slot.global {
                            AddressGlobal
                        } else {
                            AddressLocal
                        },
                        *r as u32,
                        slot.index as u32,
                        0,
                    ),
                    Op::Load(r, a) => (Load, *r as u32, *a as u32, 0),
                    Op::Store(a, r) => (Store, *a as u32, *r as u32, 0),
                    Op::FrameLoad(r, a) => (FrameLoad, *r as u32, *a as u32, 0),
                    Op::FrameStore(a, r) => (FrameStore, *a as u32, *r as u32, 0),
                    Op::Move(r, a) => (Move, *r as u32, *a as u32, 0),
                    Op::Jump(pc) => (Jump, *pc as u32, 0, 0),
                    Op::Branch(r, a, b) => (Branch, *r as u32, *a as u32, *b as u32),
                    Op::Return(r) => (Return, r.map_or(u32::MAX, |r| r as u32), 0, 0),
                    Op::CheckStep(r) => (CheckStep, *r as u32, 0, 0),
                    Op::Length(r, a) => (Length, *r as u32, *a as u32, 0),
                    Op::Unary(r, op, a) => (
                        match *op {
                            Operator::Subtract => Negate,
                            Operator::Not => Not,
                            _ => Plus,
                        },
                        *r as u32,
                        *a as u32,
                        0,
                    ),
                    Op::Binary(r, op, a, b) => (
                        match *op {
                            Operator::Add => Add,
                            Operator::Subtract => Subtract,
                            Operator::Multiply => Multiply,
                            Operator::Divide => Divide,
                            Operator::IntegerDivide => IntegerDivide,
                            Operator::Remainder => Modulo,
                            Operator::Equal => Equal,
                            Operator::NotEqual => NotEqual,
                            Operator::Less => Less,
                            Operator::Greater => Greater,
                            Operator::LessEqual => LessEqual,
                            Operator::GreaterEqual => GreaterEqual,
                            Operator::Concat => Concat,
                            _ => unreachable!("validated operation"),
                        },
                        *r as u32,
                        *a as u32,
                        *b as u32,
                    ),
                    _ => {
                        let id = complex.len();
                        complex.push(inst.op.clone());
                        (Complex, id as u32, 0, 0)
                    }
                };
                word.opcode = opcode;
                word.a = a;
                word.b = b;
                word.c = c;
                if !matches!(
                    opcode,
                    Constant
                        | Complex
                        | Statement
                        | Nop
                        | Jump
                        | Branch
                        | Return
                        | CheckStep
                        | Store
                ) {
                    word.ty = scalar(&function.registers[a as usize]);
                }
                word
            })
            .collect();
        Self {
            instructions,
            spans: function.code.iter().map(|i| i.span).collect(),
            complex: complex.into_boxed_slice(),
        }
    }
    pub fn decode(&self, pc: usize) -> Op {
        use Opcode::*;
        let w = self.instructions[pc];
        let (a, b, c) = (w.a as usize, w.b as usize, w.c as usize);
        match w.opcode {
            Statement => Op::Statement,
            Nop => Op::Nop,
            Constant => {
                let bits = w.b as u64 | ((w.c as u64) << 32);
                Op::Literal(
                    a,
                    match w.ty {
                        ScalarType::Integer => Literal::Integer(bits as i64),
                        ScalarType::Real => Literal::Real(f64::from_bits(bits)),
                        ScalarType::Boolean => Literal::Boolean(bits != 0),
                        ScalarType::Char => {
                            Literal::Char(char::from_u32(bits as u32).expect("validated CHAR"))
                        }
                        _ => unreachable!(),
                    },
                )
            }
            AddressLocal | AddressGlobal => Op::Address(
                a,
                ir::Slot {
                    global: w.opcode == AddressGlobal,
                    index: b,
                },
            ),
            Load => Op::Load(a, b),
            Store => Op::Store(a, b),
            FrameLoad => Op::FrameLoad(a, b),
            FrameStore => Op::FrameStore(a, b),
            Move => Op::Move(a, b),
            Jump => Op::Jump(a),
            Branch => Op::Branch(a, b, c),
            Return => Op::Return((w.a != u32::MAX).then_some(a)),
            CheckStep => Op::CheckStep(a),
            Length => Op::Length(a, b),
            Negate | Not | Plus => Op::Unary(
                a,
                match w.opcode {
                    Negate => Operator::Subtract,
                    Not => Operator::Not,
                    _ => Operator::Add,
                },
                b,
            ),
            Complex => self.complex[a].clone(),
            opcode => Op::Binary(
                a,
                match opcode {
                    Add => Operator::Add,
                    Subtract => Operator::Subtract,
                    Multiply => Operator::Multiply,
                    Divide => Operator::Divide,
                    IntegerDivide => Operator::IntegerDivide,
                    Modulo => Operator::Remainder,
                    Equal => Operator::Equal,
                    NotEqual => Operator::NotEqual,
                    Less => Operator::Less,
                    Greater => Operator::Greater,
                    LessEqual => Operator::LessEqual,
                    GreaterEqual => Operator::GreaterEqual,
                    Concat => Operator::Concat,
                    _ => unreachable!(),
                },
                b,
                c,
            ),
        }
    }
    /// Wasm is the adapter bytecode for Winch. Non-scalar operations use the shared runtime ABI.
    pub fn wasm_text(&self, function: usize, types: &[Type]) -> String {
        use std::fmt::Write;
        let mut out = String::from(
            "(module (import \"caie\" \"state\" (func $state (result i64))) (import \"caie\" \"step\" (func $step)) (import \"caie\" \"get\" (func $get (param i64) (result i64))) (import \"caie\" \"put\" (func $put (param i64 i64 i64))) (func (export \"run\") (param $fuel i64) (local $key i64) (local $a i64) (local $b i64) (local $r i64) (block $exit (loop $dispatch local.get $fuel i64.eqz br_if $exit call $state local.tee $key i64.const 32 i64.shr_u ",
        );
        write!(out, "i64.const {function} i64.ne br_if $exit ").unwrap();
        for pc in (0..self.instructions.len()).rev() {
            write!(out, "(block $c{pc} ").unwrap();
        }
        out.push_str("local.get $key i32.wrap_i64 br_table ");
        for pc in 0..self.instructions.len() {
            write!(out, "$c{pc} ").unwrap();
        }
        out.push_str("$exit ");
        for pc in 0..self.instructions.len() {
            out.push_str(") ");
            match self.decode(pc) {
                Op::Binary(r, op, a, b)
                    if matches!(op, Operator::Add | Operator::Subtract)
                        && types[a] == Type::Integer
                        && types[b] == Type::Integer =>
                {
                    write!(out,"i64.const {a} call $get local.set $a i64.const {b} call $get local.set $b local.get $a local.get $b {} local.set $r i64.const {r} local.get $r ",if op==Operator::Add{"i64.add"}else{"i64.sub"}).unwrap();
                    if op == Operator::Add {
                        out.push_str("local.get $a local.get $r i64.xor local.get $b local.get $r i64.xor i64.and ");
                    } else {
                        out.push_str("local.get $a local.get $b i64.xor local.get $a local.get $r i64.xor i64.and ");
                    }
                    out.push_str("i64.const 0 i64.lt_s i64.extend_i32_u call $put ");
                }
                Op::Binary(r, op, a, b)
                    if matches!(op, Operator::Add | Operator::Subtract | Operator::Multiply)
                        && types[a] == Type::Real
                        && types[b] == Type::Real =>
                {
                    write!(out,"i64.const {r} i64.const {a} call $get f64.reinterpret_i64 i64.const {b} call $get f64.reinterpret_i64 {} i64.reinterpret_f64 i64.const 0 call $put ",match op{Operator::Add=>"f64.add",Operator::Subtract=>"f64.sub",_=>"f64.mul"}).unwrap();
                }
                _ => out.push_str("call $step "),
            }
            out.push_str("local.get $fuel i64.const 1 i64.sub local.set $fuel br $dispatch ");
        }
        out.push_str("))))");
        out
    }
}
