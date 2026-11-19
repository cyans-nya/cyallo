use super::{
    BooleanOp, CallSiteId, Extremum, FloatOp, FunctionId, GlobalId, Instruction, InstructionId,
    IntegerOp, InvalidBytecode, OperandListId, Register, Scalar, TextId, TextMapOp, TextOp,
    TextTestOp,
};

// The low half of the header is the opcode and the high half is the total
// instruction width, including the header itself.
const HALF_WORD_BITS: u32 = u16::BITS;

macro_rules! opcodes {
    ($($op:ident = $code:literal, $width:literal);+ $(;)?) => {
        #[derive(Clone, Copy)]
        #[repr(u16)]
        enum Opcode { $($op = $code),+ }

        impl Opcode {
            fn from_number(value: u16) -> Option<Self> {
                match value {
                    $($code => Some(Self::$op),)+
                    _ => None,
                }
            }

            fn width(self) -> u16 {
                match self { $(Self::$op => $width),+ }
            }
        }
    };
}

opcodes! {
    ConstantI64 = 1, 4;
    ConstantBool = 2, 3;
    Copy = 3, 3;
    Add = 4, 4;
    Subtract = 5, 4;
    Multiply = 6, 4;
    Less = 7, 4;
    LessEqual = 8, 4;
    Greater = 9, 4;
    GreaterEqual = 10, 4;
    Equal = 11, 4;
    NotEqual = 12, 4;
    Jump = 13, 2;
    Branch = 14, 4;
    Call = 15, 2;
    ReturnVoid = 16, 1;
    ReturnValue = 17, 2;
    Trap = 18, 1;
    StorageLive = 19, 2;
    StorageDead = 20, 2;
    Divide = 21, 4;
    Remainder = 22, 4;
    Borrow = 23, 3;
    ReadLoan = 24, 3;
    WriteLoan = 25, 3;
    CheckStep = 26, 2;
    Tuple = 27, 3;
    Extract = 28, 4;
    EnumUnit = 29, 3;
    EnumWrap = 30, 4;
    EnumIs = 31, 4;
    EnumPayload = 32, 4;
    ConstantF64 = 33, 4;
    FloatAdd = 34, 4;
    FloatSubtract = 35, 4;
    FloatMultiply = 36, 4;
    FloatDivide = 37, 4;
    FloatCheckedDivide = 38, 4;
    FloatRemainder = 39, 4;
    FloatLess = 40, 4;
    FloatLessEqual = 41, 4;
    FloatGreater = 42, 4;
    FloatGreaterEqual = 43, 4;
    FloatEqual = 44, 4;
    FloatNotEqual = 45, 4;
    BooleanEqual = 46, 4;
    BooleanNotEqual = 47, 4;
    NegateInteger = 48, 3;
    NegateFloat = 49, 3;
    Not = 50, 3;
    IntegerToFloat = 51, 3;
    FloatToInteger = 52, 3;
    RealDivide = 53, 4;
    TextConstant = 54, 3;
    TextConcat = 55, 4;
    TextEqual = 56, 4;
    TextNotEqual = 57, 4;
    TextLess = 58, 4;
    TextLessEqual = 59, 4;
    TextGreater = 60, 4;
    TextGreaterEqual = 61, 4;
    TextLength = 62, 3;
    TextLeft = 63, 4;
    TextRight = 64, 4;
    TextMid = 65, 4;
    ArrayDeclare = 66, 2;
    ArrayNew = 67, 3;
    ArrayLoad = 68, 4;
    ArrayStore = 69, 4;
    ArrayLength = 70, 3;
    Closure = 71, 4;
    LoanField = 72, 4;
    LoanPayload = 73, 4;
    LoanElement = 74, 4;
    BorrowGlobal = 75, 3;
    GcNew = 76, 3;
    GcBorrow = 77, 3;
    Move = 78, 3;
    Drop = 79, 2;
    RcNew = 80, 3;
    RcClone = 81, 3;
    RcDowngrade = 82, 3;
    RcUpgrade = 83, 3;
    RcStrongCount = 84, 3;
    EnumTake = 85, 4;
    ConstantU64 = 86, 4;
    MoveOut = 87, 3;
    SliceOf = 88, 3;
    SliceLength = 89, 3;
    Sum = 90, 3;
    RangeNew = 91, 4;
    RangeInclusive = 92, 4;
    VecFrom = 93, 3;
    VecNew = 94, 3;
    VecPush = 95, 3;
    VecLength = 96, 3;
    CellNew = 97, 3;
    CellRead = 98, 3;
    CellWrite = 99, 3;
    CellReplace = 100, 4;
    Clone = 101, 3;
    TextTrim = 102, 3;
    TextUpper = 103, 3;
    TextLower = 104, 3;
    TextContains = 105, 4;
    TextStartsWith = 106, 4;
    TextEndsWith = 107, 4;
    TextReplace = 108, 4;
    ToText = 109, 3;
    Abs = 110, 3;
    Min = 111, 4;
    Max = 112, 4;
    ParseInteger = 113, 3;
    CountToInteger = 114, 3;
    IntegerToCount = 115, 3;
}

impl TextOp {
    fn opcode(self) -> Opcode {
        match self {
            Self::Equal => Opcode::TextEqual,
            Self::NotEqual => Opcode::TextNotEqual,
            Self::Less => Opcode::TextLess,
            Self::LessEqual => Opcode::TextLessEqual,
            Self::Greater => Opcode::TextGreater,
            Self::GreaterEqual => Opcode::TextGreaterEqual,
        }
    }
}

impl FloatOp {
    fn opcode(self) -> Opcode {
        match self {
            Self::Add => Opcode::FloatAdd,
            Self::Subtract => Opcode::FloatSubtract,
            Self::Multiply => Opcode::FloatMultiply,
            Self::Divide => Opcode::FloatDivide,
            Self::CheckedDivide => Opcode::FloatCheckedDivide,
            Self::Remainder => Opcode::FloatRemainder,
            Self::Less => Opcode::FloatLess,
            Self::LessEqual => Opcode::FloatLessEqual,
            Self::Greater => Opcode::FloatGreater,
            Self::GreaterEqual => Opcode::FloatGreaterEqual,
            Self::Equal => Opcode::FloatEqual,
            Self::NotEqual => Opcode::FloatNotEqual,
        }
    }
}

impl IntegerOp {
    fn opcode(self) -> Opcode {
        match self {
            Self::Add => Opcode::Add,
            Self::Subtract => Opcode::Subtract,
            Self::Multiply => Opcode::Multiply,
            Self::Divide => Opcode::Divide,
            Self::Remainder => Opcode::Remainder,
            Self::Less => Opcode::Less,
            Self::LessEqual => Opcode::LessEqual,
            Self::Greater => Opcode::Greater,
            Self::GreaterEqual => Opcode::GreaterEqual,
            Self::Equal => Opcode::Equal,
            Self::NotEqual => Opcode::NotEqual,
        }
    }
}

/// Encode instructions in their canonical word representation. The resulting
/// stream still needs module verification before execution or native lowering.
pub fn encode(instructions: impl IntoIterator<Item = Instruction>) -> Box<[u32]> {
    let mut words = Vec::new();
    for instruction in instructions {
        let (opcode, operands, arity) = match instruction {
            Instruction::StorageLive(reg) => (Opcode::StorageLive, [reg.0, 0, 0], 1),
            Instruction::StorageDead(reg) => (Opcode::StorageDead, [reg.0, 0, 0], 1),
            Instruction::Constant(dst, Scalar::I64(value)) => {
                let bits = value as u64;
                (
                    Opcode::ConstantI64,
                    [dst.0, bits as u32, (bits >> u32::BITS) as u32],
                    3,
                )
            }
            Instruction::Constant(dst, Scalar::Bool(value)) => {
                (Opcode::ConstantBool, [dst.0, u32::from(value), 0], 2)
            }
            Instruction::Constant(dst, Scalar::U64(value)) => (
                Opcode::ConstantU64,
                [dst.0, value as u32, (value >> u32::BITS) as u32],
                3,
            ),
            Instruction::Constant(dst, Scalar::F64(bits)) => (
                Opcode::ConstantF64,
                [dst.0, bits as u32, (bits >> u32::BITS) as u32],
                3,
            ),
            Instruction::Float(op, dst, lhs, rhs) => (op.opcode(), [dst.0, lhs.0, rhs.0], 3),
            Instruction::RealDivide(dst, lhs, rhs) => {
                (Opcode::RealDivide, [dst.0, lhs.0, rhs.0], 3)
            }
            Instruction::Boolean(op, dst, lhs, rhs) => {
                let opcode = match op {
                    BooleanOp::Equal => Opcode::BooleanEqual,
                    BooleanOp::NotEqual => Opcode::BooleanNotEqual,
                };
                (opcode, [dst.0, lhs.0, rhs.0], 3)
            }
            Instruction::NegateInteger(dst, src) => (Opcode::NegateInteger, [dst.0, src.0, 0], 2),
            Instruction::NegateFloat(dst, src) => (Opcode::NegateFloat, [dst.0, src.0, 0], 2),
            Instruction::Not(dst, src) => (Opcode::Not, [dst.0, src.0, 0], 2),
            Instruction::IntegerToFloat(dst, src) => (Opcode::IntegerToFloat, [dst.0, src.0, 0], 2),
            Instruction::FloatToInteger(dst, src) => (Opcode::FloatToInteger, [dst.0, src.0, 0], 2),
            Instruction::TextConstant(dst, text) => (Opcode::TextConstant, [dst.0, text.0, 0], 2),
            Instruction::TextConcat(dst, lhs, rhs) => {
                (Opcode::TextConcat, [dst.0, lhs.0, rhs.0], 3)
            }
            Instruction::TextCompare(op, dst, lhs, rhs) => (op.opcode(), [dst.0, lhs.0, rhs.0], 3),
            Instruction::TextLength(dst, text) => (Opcode::TextLength, [dst.0, text.0, 0], 2),
            Instruction::TextLeft(dst, text, count) => {
                (Opcode::TextLeft, [dst.0, text.0, count.0], 3)
            }
            Instruction::TextRight(dst, text, count) => {
                (Opcode::TextRight, [dst.0, text.0, count.0], 3)
            }
            Instruction::TextMid(dst, text, list) => (Opcode::TextMid, [dst.0, text.0, list.0], 3),
            Instruction::Move(dst, src) => (Opcode::Move, [dst.0, src.0, 0], 2),
            Instruction::RangeNew(dst, start, end) => {
                (Opcode::RangeNew, [dst.0, start.0, end.0], 3)
            }
            Instruction::RangeInclusive(dst, start, end) => {
                (Opcode::RangeInclusive, [dst.0, start.0, end.0], 3)
            }
            Instruction::VecFrom(dst, range) => (Opcode::VecFrom, [dst.0, range.0, 0], 2),
            Instruction::VecNew(dst, list) => (Opcode::VecNew, [dst.0, list.0, 0], 2),
            Instruction::VecPush(target, value) => (Opcode::VecPush, [target.0, value.0, 0], 2),
            Instruction::VecLength(dst, vector) => (Opcode::VecLength, [dst.0, vector.0, 0], 2),
            Instruction::CellNew(dst, value) => (Opcode::CellNew, [dst.0, value.0, 0], 2),
            Instruction::CellRead(dst, cell) => (Opcode::CellRead, [dst.0, cell.0, 0], 2),
            Instruction::CellWrite(cell, value) => (Opcode::CellWrite, [cell.0, value.0, 0], 2),
            Instruction::CellReplace(dst, cell, value) => {
                (Opcode::CellReplace, [dst.0, cell.0, value.0], 3)
            }
            Instruction::Clone(dst, src) => (Opcode::Clone, [dst.0, src.0, 0], 2),
            Instruction::TextMap(op, dst, src) => {
                let opcode = match op {
                    TextMapOp::Trim => Opcode::TextTrim,
                    TextMapOp::Upper => Opcode::TextUpper,
                    TextMapOp::Lower => Opcode::TextLower,
                };
                (opcode, [dst.0, src.0, 0], 2)
            }
            Instruction::TextTest(op, dst, text, other) => {
                let opcode = match op {
                    TextTestOp::Contains => Opcode::TextContains,
                    TextTestOp::StartsWith => Opcode::TextStartsWith,
                    TextTestOp::EndsWith => Opcode::TextEndsWith,
                };
                (opcode, [dst.0, text.0, other.0], 3)
            }
            Instruction::TextReplace(dst, text, list) => {
                (Opcode::TextReplace, [dst.0, text.0, list.0], 3)
            }
            Instruction::ToText(dst, src) => (Opcode::ToText, [dst.0, src.0, 0], 2),
            Instruction::ParseInteger(dst, text) => (Opcode::ParseInteger, [dst.0, text.0, 0], 2),
            Instruction::CountToInteger(dst, src) => (Opcode::CountToInteger, [dst.0, src.0, 0], 2),
            Instruction::IntegerToCount(dst, src) => (Opcode::IntegerToCount, [dst.0, src.0, 0], 2),
            Instruction::Abs(dst, src) => (Opcode::Abs, [dst.0, src.0, 0], 2),
            Instruction::Extremum(op, dst, lhs, rhs) => {
                let opcode = match op {
                    Extremum::Min => Opcode::Min,
                    Extremum::Max => Opcode::Max,
                };
                (opcode, [dst.0, lhs.0, rhs.0], 3)
            }
            Instruction::SliceOf(dst, src) => (Opcode::SliceOf, [dst.0, src.0, 0], 2),
            Instruction::SliceLength(dst, src) => (Opcode::SliceLength, [dst.0, src.0, 0], 2),
            Instruction::Sum(dst, src) => (Opcode::Sum, [dst.0, src.0, 0], 2),
            Instruction::MoveOut(dst, loan) => (Opcode::MoveOut, [dst.0, loan.0, 0], 2),
            Instruction::Drop(reg) => (Opcode::Drop, [reg.0, 0, 0], 1),
            Instruction::RcNew(dst, value) => (Opcode::RcNew, [dst.0, value.0, 0], 2),
            Instruction::RcClone(dst, src) => (Opcode::RcClone, [dst.0, src.0, 0], 2),
            Instruction::RcDowngrade(dst, src) => (Opcode::RcDowngrade, [dst.0, src.0, 0], 2),
            Instruction::RcUpgrade(dst, src) => (Opcode::RcUpgrade, [dst.0, src.0, 0], 2),
            Instruction::RcStrongCount(dst, src) => (Opcode::RcStrongCount, [dst.0, src.0, 0], 2),
            Instruction::EnumTake(dst, src, variant) => {
                (Opcode::EnumTake, [dst.0, src.0, variant], 3)
            }
            Instruction::GcNew(dst, value) => (Opcode::GcNew, [dst.0, value.0, 0], 2),
            Instruction::GcBorrow(dst, handle) => (Opcode::GcBorrow, [dst.0, handle.0, 0], 2),
            Instruction::BorrowGlobal(dst, global) => {
                (Opcode::BorrowGlobal, [dst.0, global.0, 0], 2)
            }
            Instruction::LoanField(dst, src, field) => {
                (Opcode::LoanField, [dst.0, src.0, field], 3)
            }
            Instruction::LoanPayload(dst, src, variant) => {
                (Opcode::LoanPayload, [dst.0, src.0, variant], 3)
            }
            Instruction::LoanElement(dst, src, list) => {
                (Opcode::LoanElement, [dst.0, src.0, list.0], 3)
            }
            Instruction::Closure(dst, target, list) => {
                (Opcode::Closure, [dst.0, target.0, list.0], 3)
            }
            Instruction::ArrayDeclare(dst) => (Opcode::ArrayDeclare, [dst.0, 0, 0], 1),
            Instruction::ArrayNew(dst, list) => (Opcode::ArrayNew, [dst.0, list.0, 0], 2),
            Instruction::ArrayLoad(dst, array, list) => {
                (Opcode::ArrayLoad, [dst.0, array.0, list.0], 3)
            }
            Instruction::ArrayStore(array, list, value) => {
                (Opcode::ArrayStore, [array.0, list.0, value.0], 3)
            }
            Instruction::ArrayLength(dst, array) => (Opcode::ArrayLength, [dst.0, array.0, 0], 2),
            Instruction::Copy(dst, src) => (Opcode::Copy, [dst.0, src.0, 0], 2),
            Instruction::Borrow(dst, owner) => (Opcode::Borrow, [dst.0, owner.0, 0], 2),
            Instruction::ReadLoan(dst, loan) => (Opcode::ReadLoan, [dst.0, loan.0, 0], 2),
            Instruction::WriteLoan(loan, value) => (Opcode::WriteLoan, [loan.0, value.0, 0], 2),
            Instruction::Integer(op, dst, lhs, rhs) => (op.opcode(), [dst.0, lhs.0, rhs.0], 3),
            Instruction::Jump(target) => (Opcode::Jump, [target.0, 0, 0], 1),
            Instruction::Branch(cond, yes, no) => (Opcode::Branch, [cond.0, yes.0, no.0], 3),
            Instruction::Call(site) => (Opcode::Call, [site.0, 0, 0], 1),
            Instruction::CheckStep(step) => (Opcode::CheckStep, [step.0, 0, 0], 1),
            Instruction::Tuple(dst, list) => (Opcode::Tuple, [dst.0, list.0, 0], 2),
            Instruction::Extract(dst, src, field) => (Opcode::Extract, [dst.0, src.0, field], 3),
            Instruction::EnumUnit(dst, variant) => (Opcode::EnumUnit, [dst.0, variant, 0], 2),
            Instruction::EnumWrap(dst, variant, payload) => {
                (Opcode::EnumWrap, [dst.0, variant, payload.0], 3)
            }
            Instruction::EnumIs(dst, src, variant) => (Opcode::EnumIs, [dst.0, src.0, variant], 3),
            Instruction::EnumPayload(dst, src, variant) => {
                (Opcode::EnumPayload, [dst.0, src.0, variant], 3)
            }
            Instruction::Return(None) => (Opcode::ReturnVoid, [0; 3], 0),
            Instruction::Return(Some(value)) => (Opcode::ReturnValue, [value.0, 0, 0], 1),
            Instruction::Trap => (Opcode::Trap, [0; 3], 0),
        };
        assert_eq!(usize::from(opcode.width()), arity + 1);
        words.push((u32::from(opcode.width()) << HALF_WORD_BITS) | opcode as u32);
        words.extend_from_slice(&operands[..arity]);
    }
    words.into_boxed_slice()
}

pub(super) fn decode(words: &[u32]) -> Result<Box<[Instruction]>, InvalidBytecode> {
    let mut decoded = Vec::new();
    let mut word = 0;
    while let Some(&header) = words.get(word) {
        let opcode = header as u16;
        let encoded = (header >> HALF_WORD_BITS) as u16;
        let op =
            Opcode::from_number(opcode).ok_or(InvalidBytecode::UnknownOpcode { word, opcode })?;
        let expected = op.width();
        if encoded != expected {
            return Err(InvalidBytecode::InstructionWidth {
                word,
                encoded,
                expected,
            });
        }
        let end = word + usize::from(expected);
        let values = words
            .get(word + 1..end)
            .ok_or(InvalidBytecode::TruncatedInstruction { word })?;
        let reg = |index: usize| Register(values[index]);
        let target = |index: usize| InstructionId(values[index]);
        let integer = |op| Instruction::Integer(op, reg(0), reg(1), reg(2));
        let float = |op| Instruction::Float(op, reg(0), reg(1), reg(2));
        let boolean = |op| Instruction::Boolean(op, reg(0), reg(1), reg(2));
        let text = |op| Instruction::TextCompare(op, reg(0), reg(1), reg(2));
        let list = |index: usize| OperandListId(values[index]);
        let instruction = match op {
            Opcode::StorageLive => Instruction::StorageLive(reg(0)),
            Opcode::StorageDead => Instruction::StorageDead(reg(0)),
            Opcode::ConstantI64 => {
                let bits = u64::from(values[1]) | (u64::from(values[2]) << u32::BITS);
                Instruction::Constant(reg(0), Scalar::I64(bits as i64))
            }
            Opcode::ConstantBool => Instruction::Constant(
                reg(0),
                Scalar::Bool(match values[1] {
                    0 => false,
                    1 => true,
                    value => return Err(InvalidBytecode::InvalidBoolean { word, value }),
                }),
            ),
            Opcode::Copy => Instruction::Copy(reg(0), reg(1)),
            Opcode::Borrow => Instruction::Borrow(reg(0), reg(1)),
            Opcode::ReadLoan => Instruction::ReadLoan(reg(0), reg(1)),
            Opcode::WriteLoan => Instruction::WriteLoan(reg(0), reg(1)),
            Opcode::Add => integer(IntegerOp::Add),
            Opcode::Subtract => integer(IntegerOp::Subtract),
            Opcode::Multiply => integer(IntegerOp::Multiply),
            Opcode::Divide => integer(IntegerOp::Divide),
            Opcode::Remainder => integer(IntegerOp::Remainder),
            Opcode::Less => integer(IntegerOp::Less),
            Opcode::LessEqual => integer(IntegerOp::LessEqual),
            Opcode::Greater => integer(IntegerOp::Greater),
            Opcode::GreaterEqual => integer(IntegerOp::GreaterEqual),
            Opcode::Equal => integer(IntegerOp::Equal),
            Opcode::NotEqual => integer(IntegerOp::NotEqual),
            Opcode::Jump => Instruction::Jump(target(0)),
            Opcode::Branch => Instruction::Branch(reg(0), target(1), target(2)),
            Opcode::Call => Instruction::Call(CallSiteId(values[0])),
            Opcode::CheckStep => Instruction::CheckStep(reg(0)),
            Opcode::ConstantU64 => {
                let value = u64::from(values[1]) | (u64::from(values[2]) << u32::BITS);
                Instruction::Constant(reg(0), Scalar::U64(value))
            }
            Opcode::ConstantF64 => {
                let bits = u64::from(values[1]) | (u64::from(values[2]) << u32::BITS);
                Instruction::Constant(reg(0), Scalar::F64(bits))
            }
            Opcode::FloatAdd => float(FloatOp::Add),
            Opcode::FloatSubtract => float(FloatOp::Subtract),
            Opcode::FloatMultiply => float(FloatOp::Multiply),
            Opcode::FloatDivide => float(FloatOp::Divide),
            Opcode::FloatCheckedDivide => float(FloatOp::CheckedDivide),
            Opcode::FloatRemainder => float(FloatOp::Remainder),
            Opcode::FloatLess => float(FloatOp::Less),
            Opcode::FloatLessEqual => float(FloatOp::LessEqual),
            Opcode::FloatGreater => float(FloatOp::Greater),
            Opcode::FloatGreaterEqual => float(FloatOp::GreaterEqual),
            Opcode::FloatEqual => float(FloatOp::Equal),
            Opcode::FloatNotEqual => float(FloatOp::NotEqual),
            Opcode::BooleanEqual => boolean(BooleanOp::Equal),
            Opcode::BooleanNotEqual => boolean(BooleanOp::NotEqual),
            Opcode::TextConstant => Instruction::TextConstant(reg(0), TextId(values[1])),
            Opcode::TextConcat => Instruction::TextConcat(reg(0), reg(1), reg(2)),
            Opcode::TextEqual => text(TextOp::Equal),
            Opcode::TextNotEqual => text(TextOp::NotEqual),
            Opcode::TextLess => text(TextOp::Less),
            Opcode::TextLessEqual => text(TextOp::LessEqual),
            Opcode::TextGreater => text(TextOp::Greater),
            Opcode::TextGreaterEqual => text(TextOp::GreaterEqual),
            Opcode::TextLength => Instruction::TextLength(reg(0), reg(1)),
            Opcode::TextLeft => Instruction::TextLeft(reg(0), reg(1), reg(2)),
            Opcode::TextRight => Instruction::TextRight(reg(0), reg(1), reg(2)),
            Opcode::TextMid => Instruction::TextMid(reg(0), reg(1), list(2)),
            Opcode::Move => Instruction::Move(reg(0), reg(1)),
            Opcode::RangeNew => Instruction::RangeNew(reg(0), reg(1), reg(2)),
            Opcode::RangeInclusive => Instruction::RangeInclusive(reg(0), reg(1), reg(2)),
            Opcode::VecFrom => Instruction::VecFrom(reg(0), reg(1)),
            Opcode::VecNew => Instruction::VecNew(reg(0), list(1)),
            Opcode::VecPush => Instruction::VecPush(reg(0), reg(1)),
            Opcode::VecLength => Instruction::VecLength(reg(0), reg(1)),
            Opcode::CellNew => Instruction::CellNew(reg(0), reg(1)),
            Opcode::CellRead => Instruction::CellRead(reg(0), reg(1)),
            Opcode::CellWrite => Instruction::CellWrite(reg(0), reg(1)),
            Opcode::CellReplace => Instruction::CellReplace(reg(0), reg(1), reg(2)),
            Opcode::Clone => Instruction::Clone(reg(0), reg(1)),
            Opcode::TextTrim => Instruction::TextMap(TextMapOp::Trim, reg(0), reg(1)),
            Opcode::TextUpper => Instruction::TextMap(TextMapOp::Upper, reg(0), reg(1)),
            Opcode::TextLower => Instruction::TextMap(TextMapOp::Lower, reg(0), reg(1)),
            Opcode::TextContains => {
                Instruction::TextTest(TextTestOp::Contains, reg(0), reg(1), reg(2))
            }
            Opcode::TextStartsWith => {
                Instruction::TextTest(TextTestOp::StartsWith, reg(0), reg(1), reg(2))
            }
            Opcode::TextEndsWith => {
                Instruction::TextTest(TextTestOp::EndsWith, reg(0), reg(1), reg(2))
            }
            Opcode::TextReplace => Instruction::TextReplace(reg(0), reg(1), list(2)),
            Opcode::ToText => Instruction::ToText(reg(0), reg(1)),
            Opcode::ParseInteger => Instruction::ParseInteger(reg(0), reg(1)),
            Opcode::CountToInteger => Instruction::CountToInteger(reg(0), reg(1)),
            Opcode::IntegerToCount => Instruction::IntegerToCount(reg(0), reg(1)),
            Opcode::Abs => Instruction::Abs(reg(0), reg(1)),
            Opcode::Min => Instruction::Extremum(Extremum::Min, reg(0), reg(1), reg(2)),
            Opcode::Max => Instruction::Extremum(Extremum::Max, reg(0), reg(1), reg(2)),
            Opcode::SliceOf => Instruction::SliceOf(reg(0), reg(1)),
            Opcode::SliceLength => Instruction::SliceLength(reg(0), reg(1)),
            Opcode::Sum => Instruction::Sum(reg(0), reg(1)),
            Opcode::MoveOut => Instruction::MoveOut(reg(0), reg(1)),
            Opcode::Drop => Instruction::Drop(reg(0)),
            Opcode::RcNew => Instruction::RcNew(reg(0), reg(1)),
            Opcode::RcClone => Instruction::RcClone(reg(0), reg(1)),
            Opcode::RcDowngrade => Instruction::RcDowngrade(reg(0), reg(1)),
            Opcode::RcUpgrade => Instruction::RcUpgrade(reg(0), reg(1)),
            Opcode::RcStrongCount => Instruction::RcStrongCount(reg(0), reg(1)),
            Opcode::EnumTake => Instruction::EnumTake(reg(0), reg(1), values[2]),
            Opcode::GcNew => Instruction::GcNew(reg(0), reg(1)),
            Opcode::GcBorrow => Instruction::GcBorrow(reg(0), reg(1)),
            Opcode::BorrowGlobal => Instruction::BorrowGlobal(reg(0), GlobalId(values[1])),
            Opcode::LoanField => Instruction::LoanField(reg(0), reg(1), values[2]),
            Opcode::LoanPayload => Instruction::LoanPayload(reg(0), reg(1), values[2]),
            Opcode::LoanElement => Instruction::LoanElement(reg(0), reg(1), list(2)),
            Opcode::Closure => Instruction::Closure(reg(0), FunctionId(values[1]), list(2)),
            Opcode::ArrayDeclare => Instruction::ArrayDeclare(reg(0)),
            Opcode::ArrayNew => Instruction::ArrayNew(reg(0), list(1)),
            Opcode::ArrayLoad => Instruction::ArrayLoad(reg(0), reg(1), list(2)),
            Opcode::ArrayStore => Instruction::ArrayStore(reg(0), list(1), reg(2)),
            Opcode::ArrayLength => Instruction::ArrayLength(reg(0), reg(1)),
            Opcode::RealDivide => Instruction::RealDivide(reg(0), reg(1), reg(2)),
            Opcode::NegateInteger => Instruction::NegateInteger(reg(0), reg(1)),
            Opcode::NegateFloat => Instruction::NegateFloat(reg(0), reg(1)),
            Opcode::Not => Instruction::Not(reg(0), reg(1)),
            Opcode::IntegerToFloat => Instruction::IntegerToFloat(reg(0), reg(1)),
            Opcode::FloatToInteger => Instruction::FloatToInteger(reg(0), reg(1)),
            Opcode::Tuple => Instruction::Tuple(reg(0), OperandListId(values[1])),
            Opcode::Extract => Instruction::Extract(reg(0), reg(1), values[2]),
            Opcode::EnumUnit => Instruction::EnumUnit(reg(0), values[1]),
            Opcode::EnumWrap => Instruction::EnumWrap(reg(0), values[1], reg(2)),
            Opcode::EnumIs => Instruction::EnumIs(reg(0), reg(1), values[2]),
            Opcode::EnumPayload => Instruction::EnumPayload(reg(0), reg(1), values[2]),
            Opcode::ReturnVoid => Instruction::Return(None),
            Opcode::ReturnValue => Instruction::Return(Some(reg(0))),
            Opcode::Trap => Instruction::Trap,
        };
        decoded.push(instruction);
        word = end;
    }
    Ok(decoded.into_boxed_slice())
}
