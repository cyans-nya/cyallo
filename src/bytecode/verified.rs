//! Opt-in verified register bytecode for scalar routines. Legacy engine selection
//! is independent of this module. Source lowering is confined to the adapter.

mod bound;
mod cfg;
mod codec;
mod debug;
mod disasm;
mod frame;
mod layout;
mod lifetimes;
mod loans;
mod lower;
mod run;
mod sdk;
mod shapes;

use crate::Span;
use bound::ScalarHostBinding;
use std::{fmt, sync::Arc};

pub use crate::ir::TextStyle;
pub use bound::{HostFailure, ScalarSdk};
pub use codec::encode;
pub use debug::{StatementArrival, StatementBoundary};
pub use frame::{FrameLayout, FrameSlot};
pub use lower::LowerError;
pub(crate) use run::ScalarFrameSnapshot;
pub use run::ScalarRecoveryError;
pub use run::{
    Activation, ExecutionLimits, ExecutionPosition, ExecutionStatus, HeapStats, OutputSink,
    RunError,
};
pub use sdk::SdkCompileError;

/// Version 18 adds counted owners, moves and drops. Version 17 adds GC handles and the managed-type table. Version 16 adds module-level variables. Version 15 adds closures that borrow. Version 14 generalizes loans to any pointee and adds projected loans.
/// Version 13 adds closures: construction and indirect calls. Version 12 adds strings and fixed-size arrays. Version 11 adds integer real division. Version 10 adds binary64 values, unary operators and numeric conversion.
/// Version 9 adds composite registers: tuples and enums. Version 8 added
/// output call targets and the loop step check. Version 7
/// carried scalar loans and borrowed call parameters.
pub const FORMAT_VERSION: u32 = 18;

macro_rules! index {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub u32);

        impl $name {
            fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

index!(Register);
index!(InstructionId);
index!(FunctionId);
index!(CallSiteId);
index!(HostBindingId);
index!(OutputId);
index!(ShapeId);
index!(OperandListId);
index!(TextId);
index!(SignatureId);
index!(GlobalId);
index!(ManagedId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScalarType {
    I64,
    /// An unsigned 64-bit count, such as `usize`. Only counts, copies and
    /// output are supported.
    U64,
    F64,
    Bool,
}

/// A value type: a scalar, or a composite described by a module shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueType {
    Scalar(ScalarType),
    /// An immutable string. Copies share the same characters.
    Text,
    Composite(ShapeId),
    /// A closure with this call signature.
    Function(SignatureId),
    /// A traced reference to a heap object of the managed type.
    Gc(ManagedId),
    /// A counted owner of an object of the managed type.
    Rc(ManagedId),
    /// A weak observer of a counted object.
    Weak(ManagedId),
}

/// A callable's parameter and result types, without its captures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub parameters: Box<[ValueType]>,
    pub result: Option<ValueType>,
}

impl From<ScalarType> for ValueType {
    fn from(value: ScalarType) -> Self {
        Self::Scalar(value)
    }
}

impl ValueType {
    /// The managed type a handle, owner or weak reference points at.
    pub fn managed(self) -> Option<ManagedId> {
        match self {
            Self::Gc(id) | Self::Rc(id) | Self::Weak(id) => Some(id),
            _ => None,
        }
    }

    pub fn scalar(self) -> Option<ScalarType> {
        match self {
            Self::Scalar(ty) => Some(ty),
            Self::Text
            | Self::Composite(_)
            | Self::Function(_)
            | Self::Gc(_)
            | Self::Rc(_)
            | Self::Weak(_) => None,
        }
    }
}

/// Composite layout-independent structure. A composite register owns its
/// complete value; storage opens and closes with the register. Components
/// reference lower-numbered shapes, so a shape cannot contain itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Shape {
    /// Fields in declaration order.
    Tuple(Box<[ValueType]>),
    /// One optional payload per variant, in declaration order.
    Enum(Box<[Option<ValueType>]>),
    /// A fixed-size array with inclusive bounds per dimension. Elements are
    /// stored in row-major order and may be uninitialized.
    Array {
        element: ValueType,
        bounds: Box<[(i64, i64)]>,
    },
}

/// An array's element type and per-dimension inclusive bounds.
type ArrayShape<'a> = (ValueType, &'a [(i64, i64)]);

/// Most elements one array may hold. The verifier rejects larger shapes
/// before any storage is reserved for them.
pub const MAX_ARRAY_ELEMENTS: usize = 1 << 22;

/// Elements of an array with these bounds, or `None` for a bound that is
/// reversed, empty of dimensions, or larger than [`MAX_ARRAY_ELEMENTS`].
pub fn array_len(bounds: &[(i64, i64)]) -> Option<usize> {
    if bounds.is_empty() {
        return None;
    }
    let mut total = 1usize;
    for &(lower, upper) in bounds {
        let extent = usize::try_from(upper.checked_sub(lower)?.checked_add(1)?).ok()?;
        total = total.checked_mul(extent)?;
        if total > MAX_ARRAY_ELEMENTS {
            return None;
        }
    }
    Some(total)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoanAccess {
    Shared,
    Exclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoanOrigin {
    Local(Register),
    /// A loan received as this parameter. Its owner is the caller's.
    Parameter {
        register: Register,
    },
    /// A module-level variable that lives as long as the activation.
    Global(GlobalId),
    /// A heap object reached through a handle of this managed type.
    Heap(ManagedId),
}

/// A logical register's verifier-visible storage and provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterType {
    Scalar(ScalarType),
    /// A borrow of the place `origin` names, or of a projection into it. The
    /// pointee is the type of the borrowed place itself.
    Loan {
        access: LoanAccess,
        origin: LoanOrigin,
        pointee: ValueType,
    },
    /// An immutable string.
    Text,
    /// An owned composite value of a module shape.
    Composite(ShapeId),
    /// A closure: a routine and its captured values.
    Function(SignatureId),
    /// A traced reference to a heap object.
    Gc(ManagedId),
    /// A counted owner.
    Rc(ManagedId),
    /// A weak observer of a counted object.
    Weak(ManagedId),
    /// A closure that captured loans. It behaves like a loan on their root:
    /// the root stays live and borrowed while it lives, and the value cannot
    /// be stored, passed or returned by value.
    BorrowingFunction {
        signature: SignatureId,
        access: LoanAccess,
        origin: LoanOrigin,
    },
    /// Retired checked-IR register with no executable storage.
    Unused,
}

impl From<ScalarType> for RegisterType {
    fn from(value: ScalarType) -> Self {
        Self::Scalar(value)
    }
}

impl From<ValueType> for RegisterType {
    fn from(value: ValueType) -> Self {
        match value {
            ValueType::Scalar(ty) => Self::Scalar(ty),
            ValueType::Text => Self::Text,
            ValueType::Composite(shape) => Self::Composite(shape),
            ValueType::Function(signature) => Self::Function(signature),
            ValueType::Gc(managed) => Self::Gc(managed),
            ValueType::Rc(managed) => Self::Rc(managed),
            ValueType::Weak(managed) => Self::Weak(managed),
        }
    }
}

impl RegisterType {
    fn owner(self) -> Option<Register> {
        match self {
            Self::Loan {
                origin: LoanOrigin::Local(owner),
                ..
            }
            | Self::BorrowingFunction {
                origin: LoanOrigin::Local(owner),
                ..
            } => Some(owner),
            _ => None,
        }
    }

    fn is_loan(self) -> bool {
        matches!(self, Self::Loan { .. } | Self::BorrowingFunction { .. })
    }

    /// The value type a register holds, if it holds a value.
    pub fn value(self) -> Option<ValueType> {
        Some(match self {
            Self::Scalar(ty) => ValueType::Scalar(ty),
            Self::Text => ValueType::Text,
            Self::Composite(shape) => ValueType::Composite(shape),
            Self::Function(signature) => ValueType::Function(signature),
            Self::Gc(managed) => ValueType::Gc(managed),
            Self::Rc(managed) => ValueType::Rc(managed),
            Self::Weak(managed) => ValueType::Weak(managed),
            _ => return None,
        })
    }

    /// The access of a loan or a borrowing closure.
    fn loan_access(self) -> Option<LoanAccess> {
        match self {
            Self::Loan { access, .. } | Self::BorrowingFunction { access, .. } => Some(access),
            _ => None,
        }
    }

    /// The root a loan or borrowing closure borrows from.
    fn loan_origin(self) -> Option<LoanOrigin> {
        match self {
            Self::Loan { origin, .. } | Self::BorrowingFunction { origin, .. } => Some(origin),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scalar {
    I64(i64),
    U64(u64),
    /// IEEE-754 binary64 bits, so values compare and hash bitwise.
    F64(u64),
    Bool(bool),
}

impl Scalar {
    pub fn from_f64(value: f64) -> Self {
        Self::F64(value.to_bits())
    }

    pub fn as_f64(self) -> Option<f64> {
        match self {
            Self::F64(bits) => Some(f64::from_bits(bits)),
            _ => None,
        }
    }

    pub(crate) fn ty(self) -> ScalarType {
        match self {
            Self::I64(_) => ScalarType::I64,
            Self::U64(_) => ScalarType::U64,
            Self::F64(_) => ScalarType::F64,
            Self::Bool(_) => ScalarType::Bool,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegerOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Equal,
    NotEqual,
}

impl IntegerOp {
    fn result(self) -> ScalarType {
        match self {
            Self::Add | Self::Subtract | Self::Multiply | Self::Divide | Self::Remainder => {
                ScalarType::I64
            }
            Self::Less
            | Self::LessEqual
            | Self::Greater
            | Self::GreaterEqual
            | Self::Equal
            | Self::NotEqual => ScalarType::Bool,
        }
    }
}

/// IEEE-754 operations on binary64 values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatOp {
    Add,
    Subtract,
    Multiply,
    /// A zero divisor gives an infinity or NaN.
    Divide,
    /// Like `Divide`, but a zero divisor fails.
    CheckedDivide,
    Remainder,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Equal,
    NotEqual,
}

impl FloatOp {
    fn result(self) -> ScalarType {
        match self {
            Self::Add
            | Self::Subtract
            | Self::Multiply
            | Self::Divide
            | Self::CheckedDivide
            | Self::Remainder => ScalarType::F64,
            Self::Less
            | Self::LessEqual
            | Self::Greater
            | Self::GreaterEqual
            | Self::Equal
            | Self::NotEqual => ScalarType::Bool,
        }
    }
}

/// Comparison of strings by their characters' code points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextOp {
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

/// Why a text slice request fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextFailure {
    IndexOutOfBounds,
    NegativeLength,
    RangeOutOfBounds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BooleanOp {
    Equal,
    NotEqual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Instruction {
    /// Open previously dead scalar storage, initially uninitialized.
    StorageLive(Register),
    /// End live scalar storage and discard any previous value.
    StorageDead(Register),
    Constant(Register, Scalar),
    Copy(Register, Register),
    Borrow(Register, Register),
    ReadLoan(Register, Register),
    WriteLoan(Register, Register),
    /// Move the value from the source, which becomes uninitialized.
    Move(Register, Register),
    /// Release the value's counted owners and weak references, leaving the
    /// register uninitialized. Nothing happens if it was already moved out.
    Drop(Register),
    /// Move the value out of the borrowed place, leaving a hole that later
    /// drops skip. The loan must be exclusive.
    MoveOut(Register, Register),
    /// Allocate a counted object owning the value, with one strong owner.
    RcNew(Register, Register),
    /// A new strong owner of the object a loan's owner refers to.
    RcClone(Register, Register),
    /// A weak reference to the object an owner refers to.
    RcDowngrade(Register, Register),
    /// A new strong owner if the object is alive, in an option-shaped enum.
    RcUpgrade(Register, Register),
    /// The strong count of the object an owner refers to.
    RcStrongCount(Register, Register),
    /// Move the payload of the variant out of the enum, which becomes
    /// uninitialized. Fails on any other variant.
    EnumTake(Register, Register, u32),
    /// Allocate a heap object holding a copy of the value.
    GcNew(Register, Register),
    /// Borrow the heap object a handle refers to.
    GcBorrow(Register, Register),
    /// Borrow a module-level variable.
    BorrowGlobal(Register, GlobalId),
    /// Borrow one field of a tuple, from an owner or another loan.
    LoanField(Register, Register, u32),
    /// Borrow the payload of a variant. Fails when another variant is held.
    LoanPayload(Register, Register, u32),
    /// Borrow one array element by checked indices.
    LoanElement(Register, Register, OperandListId),
    Integer(IntegerOp, Register, Register, Register),
    Float(FloatOp, Register, Register, Register),
    /// Divide two integers as binary64 values; a zero divisor fails.
    RealDivide(Register, Register, Register),
    Boolean(BooleanOp, Register, Register, Register),
    /// Checked negation; the minimum integer overflows.
    NegateInteger(Register, Register),
    NegateFloat(Register, Register),
    Not(Register, Register),
    IntegerToFloat(Register, Register),
    /// Truncate toward zero; non-finite or out-of-range values fail.
    FloatToInteger(Register, Register),
    Jump(InstructionId),
    Branch(Register, InstructionId, InstructionId),
    Call(CallSiteId),
    /// Fail with a zero-step error when the integer register is zero.
    CheckStep(Register),
    /// Build a tuple from the listed field registers.
    Tuple(Register, OperandListId),
    /// Copy one field out of a tuple.
    Extract(Register, Register, u32),
    /// A variant with no payload.
    EnumUnit(Register, u32),
    /// A variant carrying the payload register.
    EnumWrap(Register, u32, Register),
    /// Whether the enum holds the variant.
    EnumIs(Register, Register, u32),
    /// Copy out the payload of the variant. Fails on any other variant.
    EnumPayload(Register, Register, u32),
    TextConstant(Register, TextId),
    TextConcat(Register, Register, Register),
    TextCompare(TextOp, Register, Register, Register),
    /// Length in characters.
    TextLength(Register, Register),
    /// The first `count` characters.
    TextLeft(Register, Register, Register),
    /// The last `count` characters.
    TextRight(Register, Register, Register),
    /// Characters from the one-based start to the count, both taken from the
    /// two-entry list.
    TextMid(Register, Register, OperandListId),
    /// A closure over the routine, capturing the listed values. They become
    /// the routine's leading parameters.
    Closure(Register, FunctionId, OperandListId),
    /// An array whose elements are all uninitialized.
    ArrayDeclare(Register),
    /// An array of the listed elements.
    ArrayNew(Register, OperandListId),
    /// Read one element by checked indices, one per dimension.
    ArrayLoad(Register, Register, OperandListId),
    /// Replace one element by checked indices.
    ArrayStore(Register, OperandListId, Register),
    /// Element count of a one-dimensional array.
    ArrayLength(Register, Register),
    /// Transfer an initialized result. All other storage must be dead.
    Return(Option<Register>),
    Trap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallTarget {
    Direct(FunctionId),
    Bound(HostBindingId),
    /// The closure held by the register. Its captures precede the arguments.
    Closure(Register),
    /// Write one line of text. The template lives in the owning function.
    Output(OutputId),
}

/// One piece of an output line, in print order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputPiece {
    /// Literal text.
    Text(Arc<str>),
    /// The call argument at this index, spelled by the template's style.
    Argument(usize),
}

/// What an output call prints. Arguments are scalar registers of the call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputTemplate {
    pub style: TextStyle,
    pub pieces: Box<[OutputPiece]>,
}

#[derive(Debug, Clone)]
pub struct CallSite {
    pub target: CallTarget,
    pub arguments: Box<[Register]>,
    /// A scalar result may be discarded. A void call has no destination.
    pub destination: Option<Register>,
}

struct RegisterAccess<'a> {
    inputs: [Option<Register>; 2],
    arguments: &'a [Register],
    write: Option<Register>,
}

impl RegisterAccess<'_> {
    fn operands(&self) -> impl Iterator<Item = Register> + '_ {
        self.inputs
            .into_iter()
            .flatten()
            .chain(self.arguments.iter().copied())
    }

    /// Each operand and, transitively, every owner it borrows from. A closure
    /// that borrows a loan's owner keeps that owner's own owners live too.
    fn reads<'a>(&'a self, types: &'a [RegisterType]) -> impl Iterator<Item = Register> + 'a {
        self.operands().flat_map(move |register| {
            std::iter::successors(Some(register), move |&current| {
                types[current.index()].owner()
            })
            .take(types.len() + 1)
        })
    }
}

impl Instruction {
    fn register_access<'a>(
        self,
        calls: &'a [CallSite],
        lists: &'a [Box<[Register]>],
    ) -> RegisterAccess<'a> {
        use Instruction::*;
        let (inputs, arguments, write): (_, &[Register], _) = match self {
            Constant(dst, _) => ([None, None], &[], Some(dst)),
            Copy(dst, src) | Borrow(dst, src) | ReadLoan(dst, src) => {
                ([Some(src), None], &[], Some(dst))
            }
            WriteLoan(loan, value) => ([Some(loan), Some(value)], &[], None),
            BorrowGlobal(dst, _) => ([None, None], &[], Some(dst)),
            GcNew(dst, value) | GcBorrow(dst, value) | RcNew(dst, value) | Move(dst, value) => {
                ([Some(value), None], &[], Some(dst))
            }
            RcClone(dst, src)
            | RcDowngrade(dst, src)
            | RcUpgrade(dst, src)
            | RcStrongCount(dst, src)
            | EnumTake(dst, src, _) => ([Some(src), None], &[], Some(dst)),
            Drop(reg) => ([Some(reg), None], &[], None),
            MoveOut(dst, loan) => ([Some(loan), None], &[], Some(dst)),
            LoanField(dst, src, _) | LoanPayload(dst, src, _) => {
                ([Some(src), None], &[], Some(dst))
            }
            LoanElement(dst, src, list) => ([Some(src), None], &lists[list.index()], Some(dst)),
            Integer(_, dst, lhs, rhs)
            | Float(_, dst, lhs, rhs)
            | RealDivide(dst, lhs, rhs)
            | Boolean(_, dst, lhs, rhs) => ([Some(lhs), Some(rhs)], &[], Some(dst)),
            NegateInteger(dst, src)
            | NegateFloat(dst, src)
            | Not(dst, src)
            | IntegerToFloat(dst, src)
            | FloatToInteger(dst, src) => ([Some(src), None], &[], Some(dst)),
            Branch(reg, ..) | Return(Some(reg)) | CheckStep(reg) => ([Some(reg), None], &[], None),
            Call(site) => {
                let call = &calls[site.index()];
                let callee = match call.target {
                    CallTarget::Closure(register) => Some(register),
                    _ => None,
                };
                ([callee, None], &call.arguments, call.destination)
            }
            Closure(dst, _, list) => ([None, None], &lists[list.index()], Some(dst)),
            Tuple(dst, list) => ([None, None], &lists[list.index()], Some(dst)),
            Extract(dst, src, _) | EnumIs(dst, src, _) | EnumPayload(dst, src, _) => {
                ([Some(src), None], &[], Some(dst))
            }
            EnumUnit(dst, _) | ArrayDeclare(dst) => ([None, None], &[], Some(dst)),
            TextConstant(dst, _) => ([None, None], &[], Some(dst)),
            TextConcat(dst, lhs, rhs) | TextCompare(_, dst, lhs, rhs) => {
                ([Some(lhs), Some(rhs)], &[], Some(dst))
            }
            TextLength(dst, src) | ArrayLength(dst, src) => ([Some(src), None], &[], Some(dst)),
            TextLeft(dst, text, count) | TextRight(dst, text, count) => {
                ([Some(text), Some(count)], &[], Some(dst))
            }
            TextMid(dst, text, list) => ([Some(text), None], &lists[list.index()], Some(dst)),
            ArrayNew(dst, list) => ([None, None], &lists[list.index()], Some(dst)),
            ArrayLoad(dst, array, list) => ([Some(array), None], &lists[list.index()], Some(dst)),
            ArrayStore(array, list, value) => {
                ([Some(array), Some(value)], &lists[list.index()], None)
            }
            EnumWrap(dst, _, payload) => ([Some(payload), None], &[], Some(dst)),
            StorageLive(_) | StorageDead(_) | Jump(_) | Return(None) | Trap => {
                ([None, None], &[], None)
            }
        };
        RegisterAccess {
            inputs,
            arguments,
            write,
        }
    }
}

/// Definite initialization and lifetime facts before a verified operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageState {
    Dead,
    Uninitialized,
    Initialized,
    MaybeInitialized,
}

impl StorageState {
    fn merge(self, incoming: Self, reg: Register) -> Result<Self, InvalidBytecode> {
        use StorageState::*;
        match (self, incoming) {
            (Dead, Dead) => Ok(Dead),
            (Dead, _) | (_, Dead) => Err(InvalidBytecode::StorageJoin(reg)),
            (Initialized, Initialized) => Ok(Initialized),
            (Uninitialized, Uninitialized) => Ok(Uninitialized),
            _ => Ok(MaybeInitialized),
        }
    }
}

/// Unverified function data. The source map and branch targets are indexed by
/// instruction number, independent of variable-width word offsets.
#[derive(Debug, Clone)]
pub struct FunctionImage {
    pub registers: Box<[RegisterType]>,
    pub parameters: Box<[Register]>,
    pub result: Option<ValueType>,
    pub calls: Box<[CallSite]>,
    /// Field lists for tuple construction.
    pub lists: Box<[Box<[Register]>]>,
    /// String constants, in `TextId` order.
    pub texts: Box<[Arc<str>]>,
    pub outputs: Box<[OutputTemplate]>,
    pub words: Box<[u32]>,
    pub spans: Box<[Span]>,
    pub statements: Box<[StatementBoundary]>,
}

#[derive(Debug, Clone)]
pub struct ModuleImage {
    pub version: u32,
    pub entry: FunctionId,
    pub shapes: Box<[Shape]>,
    pub signatures: Box<[Signature]>,
    /// The types of module-level variables, by `GlobalId`.
    pub globals: Box<[ValueType]>,
    /// What each managed type points at, by `ManagedId`.
    pub managed: Box<[ValueType]>,
    pub functions: Box<[FunctionImage]>,
}

/// Read-only input for execution and future backend preparation.
#[derive(Debug)]
pub struct VerifiedFunction {
    registers: Box<[RegisterType]>,
    parameters: Box<[Register]>,
    shapes: Arc<[Shape]>,
    signatures: Arc<[Signature]>,
    globals: Arc<[ValueType]>,
    managed: Arc<[ValueType]>,
    result: Option<ValueType>,
    calls: Box<[CallSite]>,
    lists: Box<[Box<[Register]>]>,
    texts: Box<[Arc<str>]>,
    outputs: Box<[OutputTemplate]>,
    instructions: Box<[Instruction]>,
    spans: Box<[Span]>,
    statements: Box<[StatementBoundary]>,
    frame: FrameLayout,
    storage_entries: Box<[StorageEntry]>,
}

#[derive(Debug)]
struct StorageEntry {
    start: usize,
    end: usize,
    live: Box<[(usize, StorageState)]>,
}

impl VerifiedFunction {
    pub fn registers(&self) -> &[RegisterType] {
        &self.registers
    }

    pub fn parameters(&self) -> &[Register] {
        &self.parameters
    }

    pub fn result(&self) -> Option<ValueType> {
        self.result
    }

    pub fn lists(&self) -> &[Box<[Register]>] {
        &self.lists
    }

    pub fn texts(&self) -> &[Arc<str>] {
        &self.texts
    }

    pub fn calls(&self) -> &[CallSite] {
        &self.calls
    }

    pub fn outputs(&self) -> &[OutputTemplate] {
        &self.outputs
    }

    pub fn instructions(&self) -> &[Instruction] {
        &self.instructions
    }

    pub fn spans(&self) -> &[Span] {
        &self.spans
    }

    /// Statement markers in PC order, independent of executable instructions.
    pub fn statements(&self) -> &[StatementBoundary] {
        &self.statements
    }

    pub(super) fn statement_boundaries(
        &self,
        instruction: InstructionId,
        arrival: StatementArrival,
    ) -> impl Iterator<Item = &StatementBoundary> {
        let start = self
            .statements
            .partition_point(|marker| marker.instruction < instruction);
        self.statements[start..]
            .iter()
            .take_while(move |marker| marker.instruction == instruction)
            .filter(move |marker| marker.arrival.is_none_or(|edge| edge == arrival))
    }

    /// Placement for the scalar interpreter. The bytecode uses logical registers.
    pub fn frame_layout(&self) -> &FrameLayout {
        &self.frame
    }

    /// Reconstruct storage immediately before a reachable PC. Only live
    /// registers at block entries are retained, rather than a map per PC.
    pub fn storage_before(&self, pc: InstructionId) -> Option<Box<[StorageState]>> {
        let pc = pc.index();
        let entry = self.storage_entry(pc)?;
        let mut state = vec![StorageState::Dead; self.registers.len()];
        for &(index, slot) in &entry.live {
            state[index] = slot;
        }
        for &inst in &self.instructions[entry.start..pc] {
            self.update_storage(inst, &mut state);
        }
        Some(state.into_boxed_slice())
    }

    pub fn is_reachable(&self, pc: InstructionId) -> bool {
        self.storage_entry(pc.index()).is_some()
    }

    /// Visit the storage facts for each reachable instruction in PC order.
    pub fn visit_storage<E>(
        &self,
        mut visit: impl FnMut(InstructionId, &[StorageState]) -> Result<(), E>,
    ) -> Result<(), E> {
        let mut state = vec![StorageState::Dead; self.registers.len()];
        for entry in &self.storage_entries {
            state.fill(StorageState::Dead);
            for &(index, value) in &entry.live {
                state[index] = value;
            }
            for index in entry.start..entry.end {
                // The verifier bounds the complete instruction index space.
                let Ok(pc) = u32::try_from(index) else {
                    unreachable!("verified instruction index");
                };
                visit(InstructionId(pc), &state)?;
                self.update_storage(self.instructions[index], &mut state);
            }
        }
        Ok(())
    }

    fn storage_entry(&self, pc: usize) -> Option<&StorageEntry> {
        let offset = self
            .storage_entries
            .partition_point(|entry| entry.start <= pc)
            .checked_sub(1)?;
        let entry = &self.storage_entries[offset];
        (pc < entry.end).then_some(entry)
    }

    fn register_type(&self, register: Register) -> Result<ScalarType, InvalidBytecode> {
        match self.register_kind(register)? {
            RegisterType::Scalar(ty) => Ok(ty),
            _ => Err(InvalidBytecode::ExpectedScalar(register)),
        }
    }

    fn value_type(&self, register: Register) -> Result<ValueType, InvalidBytecode> {
        match self.register_kind(register)? {
            RegisterType::Scalar(ty) => Ok(ValueType::Scalar(ty)),
            RegisterType::Text => Ok(ValueType::Text),
            RegisterType::Composite(shape) => Ok(ValueType::Composite(shape)),
            RegisterType::Function(signature) => Ok(ValueType::Function(signature)),
            RegisterType::Gc(managed) => Ok(ValueType::Gc(managed)),
            RegisterType::Rc(managed) => Ok(ValueType::Rc(managed)),
            RegisterType::Weak(managed) => Ok(ValueType::Weak(managed)),
            RegisterType::BorrowingFunction { .. } => {
                Err(InvalidBytecode::EscapingBorrow(register))
            }
            _ => Err(InvalidBytecode::ExpectedScalar(register)),
        }
    }

    /// The type of the value an owner holds. A borrowing closure can be
    /// borrowed like any value, but not moved or copied out by value.
    fn owned_type(&self, register: Register) -> Result<ValueType, InvalidBytecode> {
        match self.register_kind(register)? {
            RegisterType::BorrowingFunction { signature, .. } => Ok(ValueType::Function(signature)),
            _ => self.value_type(register),
        }
    }

    fn require_value(
        &self,
        register: Register,
        expected: ValueType,
    ) -> Result<(), InvalidBytecode> {
        let actual = self.value_type(register)?;
        if actual != expected {
            return Err(InvalidBytecode::ValueType {
                register,
                expected,
                actual,
            });
        }
        Ok(())
    }

    fn shape(&self, id: ShapeId) -> Result<&Shape, InvalidBytecode> {
        self.shapes
            .get(id.index())
            .ok_or(InvalidBytecode::UnknownShape(id))
    }

    fn tuple_fields(&self, register: Register) -> Result<&[ValueType], InvalidBytecode> {
        let RegisterType::Composite(id) = self.register_kind(register)? else {
            return Err(InvalidBytecode::ExpectedTuple(register));
        };
        match self.shape(id)? {
            Shape::Tuple(fields) => Ok(fields),
            _ => Err(InvalidBytecode::ExpectedTuple(register)),
        }
    }

    /// The variants of a verified option-shaped enum holding a counted owner:
    /// the one that carries it, then the empty one.
    fn option_variants(&self, register: Register) -> (u32, u32) {
        let variants = match self.enum_variants(register) {
            Ok(variants) => variants,
            Err(_) => unreachable!("verified enum register"),
        };
        let index = |found: Option<usize>| found.and_then(|i| u32::try_from(i).ok()).unwrap_or(0);
        (
            index(variants.iter().position(Option::is_some)),
            index(variants.iter().position(Option::is_none)),
        )
    }

    /// Bounds of the array that a verified projection starts from.
    fn projected_array_bounds(&self, source: Register) -> &[(i64, i64)] {
        let array = self
            .projection_base(source)
            .ok()
            .and_then(|(container, ..)| match container {
                ValueType::Composite(shape) => match self.shapes.get(shape.index()) {
                    Some(Shape::Array { bounds, .. }) => Some(&**bounds),
                    _ => None,
                },
                _ => None,
            });
        match array {
            Some(bounds) => bounds,
            None => unreachable!("verified array projection"),
        }
    }

    /// Bounds of a verified array register.
    fn array_bounds(&self, register: Register) -> ArrayShape<'_> {
        match self.array_shape(register) {
            Ok(shape) => shape,
            Err(_) => unreachable!("verified array register"),
        }
    }

    fn signature(&self, id: SignatureId) -> Result<&Signature, InvalidBytecode> {
        self.signatures
            .get(id.index())
            .ok_or(InvalidBytecode::UnknownSignature(id))
    }

    /// The managed type a counted operand refers to: an owner when `strong`,
    /// otherwise a weak reference. The operand is held by a register or
    /// behind a loan.
    fn counted_source(&self, source: Register, strong: bool) -> Result<ManagedId, InvalidBytecode> {
        let (held, ..) = self.projection_base(source)?;
        match (held, strong) {
            (ValueType::Rc(managed), true) | (ValueType::Weak(managed), false) => Ok(managed),
            _ => Err(InvalidBytecode::ExpectedHandle(source)),
        }
    }

    fn array_shape(&self, register: Register) -> Result<ArrayShape<'_>, InvalidBytecode> {
        let RegisterType::Composite(id) = self.register_kind(register)? else {
            return Err(InvalidBytecode::ExpectedArray(register));
        };
        match self.shape(id)? {
            Shape::Array { element, bounds } => Ok((*element, bounds)),
            _ => Err(InvalidBytecode::ExpectedArray(register)),
        }
    }

    fn require_text(&self, register: Register) -> Result<(), InvalidBytecode> {
        match self.register_kind(register)? {
            RegisterType::Text => Ok(()),
            _ => Err(InvalidBytecode::ExpectedText(register)),
        }
    }

    fn operand_list(&self, list: OperandListId) -> Result<&[Register], InvalidBytecode> {
        self.lists
            .get(list.index())
            .map(|list| &**list)
            .ok_or(InvalidBytecode::UnknownList(list))
    }

    /// Every register of the list is an integer index, one per dimension.
    fn require_indices(
        &self,
        array: Register,
        bounds: &[(i64, i64)],
        list: OperandListId,
    ) -> Result<(), InvalidBytecode> {
        let indices = self.operand_list(list)?;
        if indices.len() != bounds.len() {
            return Err(InvalidBytecode::ArrayRank {
                register: array,
                expected: bounds.len(),
                actual: indices.len(),
            });
        }
        indices
            .iter()
            .try_for_each(|&index| self.require_type(index, ScalarType::I64))
    }

    fn enum_variants(&self, register: Register) -> Result<&[Option<ValueType>], InvalidBytecode> {
        let RegisterType::Composite(id) = self.register_kind(register)? else {
            return Err(InvalidBytecode::ExpectedEnum(register));
        };
        match self.shape(id)? {
            Shape::Enum(variants) => Ok(variants),
            _ => Err(InvalidBytecode::ExpectedEnum(register)),
        }
    }

    fn variant_payload(
        &self,
        register: Register,
        variant: u32,
    ) -> Result<Option<ValueType>, InvalidBytecode> {
        self.enum_variants(register)?
            .get(variant as usize)
            .copied()
            .ok_or(InvalidBytecode::UnknownVariant { register, variant })
    }

    fn register_kind(&self, register: Register) -> Result<RegisterType, InvalidBytecode> {
        self.registers
            .get(register.index())
            .copied()
            .filter(|kind| *kind != RegisterType::Unused)
            .ok_or(InvalidBytecode::UnknownRegister(register))
    }

    fn require_type(
        &self,
        register: Register,
        expected: ScalarType,
    ) -> Result<(), InvalidBytecode> {
        let actual = self.register_type(register)?;
        if expected != actual {
            return Err(InvalidBytecode::RegisterType {
                register,
                expected,
                actual,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct VerifiedModule {
    entry: FunctionId,
    functions: Arc<[VerifiedFunction]>,
    shapes: Arc<[Shape]>,
    globals: Arc<[ValueType]>,
    host_bindings: Arc<[ScalarHostBinding]>,
}

impl VerifiedModule {
    pub fn entry(&self) -> FunctionId {
        self.entry
    }

    pub fn function(&self, id: FunctionId) -> Option<&VerifiedFunction> {
        self.functions.get(id.index())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidBytecode {
    Version(u32),
    UnknownFunction(FunctionId),
    FunctionIdOverflow,
    UnknownOpcode {
        word: usize,
        opcode: u16,
    },
    InstructionWidth {
        word: usize,
        encoded: u16,
        expected: u16,
    },
    TruncatedInstruction {
        word: usize,
    },
    InvalidBoolean {
        word: usize,
        value: u32,
    },
    SourceMapLength {
        instructions: usize,
        spans: usize,
    },
    StatementTarget(InstructionId),
    StatementOrder(InstructionId),
    StatementEdge(StatementArrival),
    EmptyFunction,
    InstructionIdOverflow,
    UnknownRegister(Register),
    ExpectedScalar(Register),
    ExpectedLoan(Register),
    ExpectedExclusiveLoan(Register),
    InvalidLoanParameter(Register),
    IncompatibleRegisters {
        destination: Register,
        source: Register,
    },
    LoanOwnerMismatch {
        loan: Register,
        owner: Register,
    },
    BorrowedOwner(Register),
    ExclusiveOwnerAccess(Register),
    ConflictingLoanArgument {
        first: Register,
        second: Register,
    },
    LoanJoin(Register),
    DuplicateParameter(Register),
    RegisterType {
        register: Register,
        expected: ScalarType,
        actual: ScalarType,
    },
    Uninitialized(Register),
    DeadStorageAccess(Register),
    StorageAlreadyLive(Register),
    StorageAlreadyDead(Register),
    StorageJoin(Register),
    LiveStorageAtReturn(Register),
    UnknownCallSite(CallSiteId),
    UnknownHostBinding(HostBindingId),
    UnknownOutput(OutputId),
    ShapeReference(ShapeId),
    UnknownShape(ShapeId),
    UnknownList(OperandListId),
    ExpectedTuple(Register),
    ExpectedArray(Register),
    ExpectedText(Register),
    UnknownText(TextId),
    UnknownGlobal(GlobalId),
    UnknownManaged(ManagedId),
    ShapeCycle(ShapeId),
    ExpectedHandle(Register),
    UnknownSignature(SignatureId),
    ExpectedFunction(Register),
    /// A closure that borrows cannot be stored, passed or returned by value.
    EscapingBorrow(Register),
    ClosureTarget(FunctionId),
    UnknownTextOperation {
        word: usize,
        value: u32,
    },
    ArrayRank {
        register: Register,
        expected: usize,
        actual: usize,
    },
    InvalidArrayBounds(ShapeId),
    ExpectedEnum(Register),
    UnknownField {
        register: Register,
        field: u32,
    },
    UnknownVariant {
        register: Register,
        variant: u32,
    },
    ValueType {
        register: Register,
        expected: ValueType,
        actual: ValueType,
    },
    OutputArgument {
        output: OutputId,
        index: usize,
    },
    OutputDestination,
    ArgumentCount {
        expected: usize,
        actual: usize,
    },
    ResultPresence,
    UnknownTarget(InstructionId),
    Fallthrough,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifyError {
    pub function: Option<FunctionId>,
    pub instruction: Option<InstructionId>,
    pub kind: InvalidBytecode,
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid verified bytecode")?;
        if let Some(id) = self.function {
            write!(f, " in function {}", id.0)?;
        }
        if let Some(id) = self.instruction {
            write!(f, " at instruction {}", id.0)?;
        }
        write!(f, ": {:?}", self.kind)
    }
}

impl std::error::Error for VerifyError {}

impl ModuleImage {
    pub fn verify(self) -> Result<VerifiedModule, VerifyError> {
        self.verify_with_host_bindings(Vec::new())
    }

    fn verify_with_host_bindings(
        self,
        host_bindings: Vec<ScalarHostBinding>,
    ) -> Result<VerifiedModule, VerifyError> {
        let image_error = |kind| VerifyError {
            function: None,
            instruction: None,
            kind,
        };
        if self.version != FORMAT_VERSION {
            return Err(image_error(InvalidBytecode::Version(self.version)));
        }
        if self.entry.index() >= self.functions.len() {
            return Err(image_error(InvalidBytecode::UnknownFunction(self.entry)));
        }
        for (index, shape) in self.shapes.iter().enumerate() {
            if let Shape::Array { bounds, .. } = shape
                && array_len(bounds).is_none()
            {
                return Err(image_error(InvalidBytecode::InvalidArrayBounds(ShapeId(
                    index as u32,
                ))));
            }
            for component in shape_components(shape) {
                match component {
                    ValueType::Function(signature)
                        if signature.index() >= self.signatures.len() =>
                    {
                        return Err(image_error(InvalidBytecode::UnknownSignature(signature)));
                    }
                    ValueType::Gc(managed) | ValueType::Rc(managed) | ValueType::Weak(managed)
                        if managed.index() >= self.managed.len() =>
                    {
                        return Err(image_error(InvalidBytecode::UnknownManaged(managed)));
                    }
                    ValueType::Composite(id) if id.index() >= self.shapes.len() => {
                        return Err(image_error(InvalidBytecode::UnknownShape(id)));
                    }
                    _ => {}
                }
            }
        }
        // A composite cannot contain itself by value. A handle is an
        // indirection, so recursion through one is fine.
        if let Some(shape) = first_shape_cycle(&self.shapes) {
            return Err(image_error(InvalidBytecode::ShapeCycle(shape)));
        }
        let managed: Arc<[ValueType]> = self.managed.into();
        let globals: Arc<[ValueType]> = self.globals.into();
        let signatures: Arc<[Signature]> = self.signatures.into();
        let shapes: Arc<[Shape]> = self.shapes.into();
        for signature in signatures.iter() {
            let components = signature.parameters.iter().chain(signature.result.iter());
            for &component in components {
                match component {
                    ValueType::Composite(id) if id.index() >= shapes.len() => {
                        return Err(image_error(InvalidBytecode::UnknownShape(id)));
                    }
                    ValueType::Function(id) if id.index() >= signatures.len() => {
                        return Err(image_error(InvalidBytecode::UnknownSignature(id)));
                    }
                    _ => {}
                }
            }
        }
        let mut functions = Vec::with_capacity(self.functions.len());
        for (index, image) in self.functions.into_vec().into_iter().enumerate() {
            let id = function_id(index).map_err(image_error)?;
            let error = |kind| VerifyError {
                function: Some(id),
                instruction: None,
                kind,
            };
            let instructions = codec::decode(&image.words).map_err(error)?;
            if instructions.is_empty() {
                return Err(error(InvalidBytecode::EmptyFunction));
            }
            if instructions.len() != image.spans.len() {
                return Err(error(InvalidBytecode::SourceMapLength {
                    instructions: instructions.len(),
                    spans: image.spans.len(),
                }));
            }
            functions.push(VerifiedFunction {
                shapes: shapes.clone(),
                signatures: signatures.clone(),
                globals: globals.clone(),
                managed: managed.clone(),
                registers: image.registers,
                parameters: image.parameters,
                result: image.result,
                calls: image.calls,
                lists: image.lists,
                texts: image.texts,
                outputs: image.outputs,
                instructions,
                spans: image.spans,
                statements: image.statements,
                frame: FrameLayout::default(),
                storage_entries: Box::new([]),
            });
        }
        for index in 0..functions.len() {
            let id = function_id(index).map_err(image_error)?;
            let (frame, storage_entries) =
                functions[index].verify(id, id == self.entry, &functions, &host_bindings)?;
            functions[index].frame = frame;
            functions[index].storage_entries = storage_entries;
        }
        Ok(VerifiedModule {
            entry: self.entry,
            shapes,
            globals,
            functions: functions.into(),
            host_bindings: host_bindings.into(),
        })
    }
}

fn shape_components(shape: &Shape) -> Vec<ValueType> {
    match shape {
        Shape::Tuple(fields) => fields.to_vec(),
        Shape::Enum(variants) => variants.iter().copied().flatten().collect(),
        Shape::Array { element, .. } => vec![*element],
    }
}

/// A shape on a cycle of by-value containment, if there is one.
fn first_shape_cycle(shapes: &[Shape]) -> Option<ShapeId> {
    // 0 = unvisited, 1 = on the current path, 2 = finished.
    let mut state = vec![0u8; shapes.len()];
    for start in 0..shapes.len() {
        if state[start] != 0 {
            continue;
        }
        let mut stack = vec![(start, shape_children(&shapes[start]), 0usize)];
        state[start] = 1;
        while let Some((node, children, next)) = stack.last_mut() {
            let node = *node;
            if let Some(&child) = children.get(*next) {
                *next += 1;
                match state[child] {
                    1 => return u32::try_from(child).ok().map(ShapeId),
                    0 => {
                        state[child] = 1;
                        stack.push((child, shape_children(&shapes[child]), 0));
                    }
                    _ => {}
                }
            } else {
                state[node] = 2;
                stack.pop();
            }
        }
    }
    None
}

fn shape_children(shape: &Shape) -> Vec<usize> {
    shape_components(shape)
        .into_iter()
        .filter_map(|component| match component {
            ValueType::Composite(id) => Some(id.index()),
            _ => None,
        })
        .collect()
}

fn function_id(index: usize) -> Result<FunctionId, InvalidBytecode> {
    u32::try_from(index)
        .map(FunctionId)
        .map_err(|_| InvalidBytecode::FunctionIdOverflow)
}

impl VerifiedFunction {
    fn verify(
        &self,
        id: FunctionId,
        is_entry: bool,
        functions: &[Self],
        host_bindings: &[ScalarHostBinding],
    ) -> Result<(FrameLayout, Box<[StorageEntry]>), VerifyError> {
        let error = |pc, kind| VerifyError {
            function: Some(id),
            instruction: pc,
            kind,
        };
        self.verify_statements()
            .map_err(|(pc, kind)| error(Some(pc), kind))?;
        let mut borrowers = std::collections::HashMap::<Register, Vec<usize>>::new();
        for (index, &kind) in self.registers.iter().enumerate() {
            if let RegisterType::BorrowingFunction { signature, .. } = kind {
                self.signature(signature)
                    .map_err(|kind| error(None, kind))?;
            }
            let (Some(access), Some(origin)) = (kind.loan_access(), kind.loan_origin()) else {
                continue;
            };
            match origin {
                LoanOrigin::Local(owner) => {
                    self.owned_type(owner).map_err(|kind| error(None, kind))?;
                    borrowers.entry(owner).or_default().push(index);
                }
                LoanOrigin::Global(global) => {
                    if self.globals.get(global.index()).is_none() {
                        return Err(error(None, InvalidBytecode::UnknownGlobal(global)));
                    }
                }
                LoanOrigin::Heap(managed) => {
                    if self.managed.get(managed.index()).is_none() {
                        return Err(error(None, InvalidBytecode::UnknownManaged(managed)));
                    }
                }
                LoanOrigin::Parameter { register } => {
                    let valid = self.parameters.contains(&register)
                        && matches!(
                            self.registers.get(register.index()),
                            Some(RegisterType::Loan {
                                origin: LoanOrigin::Parameter { register: root },
                                access: root_access,
                                ..
                            }) if *root == register
                                && (access == LoanAccess::Shared
                                    || *root_access == LoanAccess::Exclusive)
                        );
                    if !valid {
                        return Err(error(None, InvalidBytecode::InvalidLoanParameter(register)));
                    }
                }
            }
        }
        for &kind in self.registers.iter() {
            if let RegisterType::Composite(shape) = kind {
                self.shape(shape).map_err(|kind| error(None, kind))?;
            }
            if let RegisterType::Function(signature) = kind {
                self.signature(signature)
                    .map_err(|kind| error(None, kind))?;
            }
            if let RegisterType::Gc(managed)
            | RegisterType::Rc(managed)
            | RegisterType::Weak(managed) = kind
                && self.managed.get(managed.index()).is_none()
            {
                return Err(error(None, InvalidBytecode::UnknownManaged(managed)));
            }
        }
        let mut parameters = vec![StorageState::Dead; self.registers.len()];
        for &register in &self.parameters {
            match self
                .register_kind(register)
                .map_err(|kind| error(None, kind))?
            {
                RegisterType::Scalar(_)
                | RegisterType::Text
                | RegisterType::Composite(_)
                | RegisterType::Function(_)
                | RegisterType::Gc(_)
                | RegisterType::Rc(_)
                | RegisterType::Weak(_) => {}
                RegisterType::Loan {
                    origin: LoanOrigin::Parameter { register: origin },
                    ..
                } if register == origin => {}
                _ => return Err(error(None, InvalidBytecode::InvalidLoanParameter(register))),
            }
            if std::mem::replace(&mut parameters[register.index()], StorageState::Initialized)
                != StorageState::Dead
            {
                return Err(error(None, InvalidBytecode::DuplicateParameter(register)));
            }
        }
        for call in &self.calls {
            self.verify_call(call, functions, host_bindings)
                .map_err(|kind| error(None, kind))?;
        }

        // Branch targets and successors define basic blocks. Store an input
        // state per block instead of per instruction.
        let mut starts = vec![false; self.instructions.len()];
        starts[0] = true;
        for (index, inst) in self.instructions.iter().copied().enumerate() {
            let pc = u32::try_from(index)
                .map(InstructionId)
                .map_err(|_| error(None, InvalidBytecode::InstructionIdOverflow))?;
            self.verify_instruction(inst, functions)
                .map_err(|kind| error(Some(pc), kind))?;
            match inst {
                Instruction::Jump(target) => starts[target.index()] = true,
                Instruction::Branch(_, yes, no) => {
                    starts[yes.index()] = true;
                    starts[no.index()] = true;
                }
                _ => {}
            }
            if matches!(
                inst,
                Instruction::Jump(_)
                    | Instruction::Branch(..)
                    | Instruction::Return(_)
                    | Instruction::Trap
            ) && let Some(next) = starts.get_mut(index + 1)
            {
                *next = true;
            }
        }

        let blocks: Vec<_> = starts
            .iter()
            .enumerate()
            .filter_map(|(i, start)| start.then_some(i))
            .collect();
        let mut incoming = vec![None; blocks.len()];
        incoming[0] = Some(parameters);
        let mut pending = vec![0];
        let mut queued = vec![false; blocks.len()];
        queued[0] = true;
        let mut ranges = frame::LiveRanges::new(self.registers.len());
        while let Some(block) = pending.pop() {
            queued[block] = false;
            let start = blocks[block];
            let end = blocks
                .get(block + 1)
                .copied()
                .unwrap_or(self.instructions.len());
            let mut state = incoming[block]
                .as_ref()
                .expect("queued block has an input")
                .clone();
            ranges.observe_boundary(InstructionId(start as u32), &state);
            for index in start..end {
                let pc = InstructionId(index as u32);
                let inst = self.instructions[index];
                self.transfer(inst, &mut state, &borrowers)
                    .map_err(|kind| error(Some(pc), kind))?;
                ranges.observe_instruction(pc, inst);
            }
            let pc = InstructionId((end - 1) as u32);
            ranges.observe_boundary(pc, &state);
            let mut merge = |target: usize| -> Result<(), VerifyError> {
                let successor = blocks
                    .binary_search(&target)
                    .expect("verified block target");
                let changed = match &mut incoming[successor] {
                    Some(old) => {
                        let mut changed = false;
                        for (index, (old, new)) in old.iter_mut().zip(&state).enumerate() {
                            // Registers with a live state are always referenced by a u32 ID.
                            if *old != *new && self.registers[index].is_loan() {
                                return Err(error(
                                    Some(InstructionId(target as u32)),
                                    InvalidBytecode::LoanJoin(Register(index as u32)),
                                ));
                            }
                            let merged = old
                                .merge(*new, Register(index as u32))
                                .map_err(|kind| error(Some(InstructionId(target as u32)), kind))?;
                            changed |= *old != merged;
                            *old = merged;
                        }
                        changed
                    }
                    slot @ None => {
                        *slot = Some(state.clone());
                        true
                    }
                };
                if changed && !std::mem::replace(&mut queued[successor], true) {
                    pending.push(successor);
                }
                Ok(())
            };
            match self.instructions[end - 1] {
                Instruction::Jump(target) => merge(target.index())?,
                Instruction::Branch(_, yes, no) => {
                    merge(yes.index())?;
                    merge(no.index())?;
                }
                Instruction::Return(_) | Instruction::Trap => {}
                _ if end < self.instructions.len() => merge(end)?,
                _ => return Err(error(Some(pc), InvalidBytecode::Fallthrough)),
            }
        }
        let storage_entries = incoming
            .into_iter()
            .enumerate()
            .filter_map(|(block, input)| {
                let input = input?;
                let live = input
                    .into_iter()
                    .enumerate()
                    .filter(|(_, slot)| *slot != StorageState::Dead)
                    .collect();
                Some(StorageEntry {
                    start: blocks[block],
                    end: blocks
                        .get(block + 1)
                        .copied()
                        .unwrap_or(self.instructions.len()),
                    live,
                })
            })
            .collect();
        Ok((ranges.place(), storage_entries))
    }

    fn verify_statements(&self) -> Result<(), (InstructionId, InvalidBytecode)> {
        let mut previous = None;
        for marker in &self.statements {
            let pc = marker.instruction;
            if pc.index() >= self.instructions.len() {
                return Err((pc, InvalidBytecode::StatementTarget(pc)));
            }
            let order = marker.order();
            if previous.is_some_and(|previous| previous > order) {
                return Err((pc, InvalidBytecode::StatementOrder(pc)));
            }
            if let Some(edge) = marker.arrival
                && !edge.reaches(pc, &self.instructions)
            {
                return Err((pc, InvalidBytecode::StatementEdge(edge)));
            }
            previous = Some(order);
        }
        Ok(())
    }

    fn verify_call(
        &self,
        call: &CallSite,
        functions: &[Self],
        host_bindings: &[ScalarHostBinding],
    ) -> Result<(), InvalidBytecode> {
        let result = match call.target {
            CallTarget::Direct(id) => {
                let callee = functions
                    .get(id.index())
                    .ok_or(InvalidBytecode::UnknownFunction(id))?;
                self.verify_guest_arguments(&call.arguments, &callee.parameters, callee)?;
                callee.result
            }
            CallTarget::Bound(id) => {
                let binding = host_bindings
                    .get(id.index())
                    .ok_or(InvalidBytecode::UnknownHostBinding(id))?;
                self.verify_arguments(
                    &call.arguments,
                    binding.parameters.iter().map(|&ty| Ok(ty.into())),
                )?;
                Some(binding.result.into())
            }
            CallTarget::Closure(callee) => {
                let (RegisterType::Function(id)
                | RegisterType::BorrowingFunction { signature: id, .. }) =
                    self.register_kind(callee)?
                else {
                    return Err(InvalidBytecode::ExpectedFunction(callee));
                };
                let signature = self.signature(id)?;
                self.verify_arguments(
                    &call.arguments,
                    signature.parameters.iter().map(|&ty| Ok(ty)),
                )?;
                signature.result
            }
            CallTarget::Output(id) => {
                let template = self
                    .outputs
                    .get(id.index())
                    .ok_or(InvalidBytecode::UnknownOutput(id))?;
                for piece in template.pieces.iter() {
                    if let OutputPiece::Argument(index) = *piece
                        && index >= call.arguments.len()
                    {
                        return Err(InvalidBytecode::OutputArgument { output: id, index });
                    }
                }
                for &argument in call.arguments.iter() {
                    if self.value_type(argument)? != ValueType::Text {
                        self.register_type(argument)?;
                    }
                }
                if call.destination.is_some() {
                    return Err(InvalidBytecode::OutputDestination);
                }
                None
            }
        };
        if call.destination.is_some() {
            self.verify_result(call.destination, result)?;
        }
        Ok(())
    }

    fn verify_guest_arguments(
        &self,
        arguments: &[Register],
        parameters: &[Register],
        callee: &Self,
    ) -> Result<(), InvalidBytecode> {
        if arguments.len() != parameters.len() {
            return Err(InvalidBytecode::ArgumentCount {
                expected: parameters.len(),
                actual: arguments.len(),
            });
        }
        let mut loans = std::collections::HashMap::new();
        for (&arg, &parameter) in arguments.iter().zip(parameters) {
            match callee.register_kind(parameter)? {
                RegisterType::Scalar(ty) => self.require_type(arg, ty)?,
                RegisterType::Text => self.require_value(arg, ValueType::Text)?,
                RegisterType::Function(signature) => {
                    self.require_value(arg, ValueType::Function(signature))?;
                }
                RegisterType::Gc(managed) => self.require_value(arg, ValueType::Gc(managed))?,
                RegisterType::Rc(managed) => self.require_value(arg, ValueType::Rc(managed))?,
                RegisterType::Weak(managed) => self.require_value(arg, ValueType::Weak(managed))?,
                RegisterType::Composite(shape) => {
                    self.require_value(arg, ValueType::Composite(shape))?;
                }
                RegisterType::Loan {
                    access,
                    origin: LoanOrigin::Parameter { .. },
                    pointee,
                } => {
                    let (actual, origin, ty) = self.loan_type(arg)?;
                    if access == LoanAccess::Exclusive && actual != access {
                        return Err(InvalidBytecode::ExpectedExclusiveLoan(arg));
                    }
                    if ty != pointee {
                        return Err(InvalidBytecode::ValueType {
                            register: arg,
                            expected: pointee,
                            actual: ty,
                        });
                    }
                    if let Some((first, previous)) = loans.insert(origin, (arg, access))
                        && (access == LoanAccess::Exclusive || previous == LoanAccess::Exclusive)
                    {
                        return Err(InvalidBytecode::ConflictingLoanArgument {
                            first,
                            second: arg,
                        });
                    }
                }
                _ => return Err(InvalidBytecode::InvalidLoanParameter(parameter)),
            }
        }
        Ok(())
    }

    fn loan_type(
        &self,
        register: Register,
    ) -> Result<(LoanAccess, LoanOrigin, ValueType), InvalidBytecode> {
        let RegisterType::Loan {
            access,
            origin,
            pointee,
        } = self.register_kind(register)?
        else {
            return Err(InvalidBytecode::ExpectedLoan(register));
        };
        Ok((access, origin, pointee))
    }

    /// What a projection starts from: a loan's pointee, or an owned register.
    fn projection_base(
        &self,
        register: Register,
    ) -> Result<(ValueType, LoanAccess, LoanOrigin), InvalidBytecode> {
        match self.register_kind(register)? {
            RegisterType::Loan {
                access,
                origin,
                pointee,
            } => Ok((pointee, access, origin)),
            _ => Ok((
                self.owned_type(register)?,
                LoanAccess::Exclusive,
                LoanOrigin::Local(register),
            )),
        }
    }

    /// The projected loan matches its base: same root, no wider access, and
    /// the pointee the projection reaches.
    fn require_projection(
        &self,
        dst: Register,
        src: Register,
        reached: ValueType,
    ) -> Result<(), InvalidBytecode> {
        let (_, base_access, base_origin) = self.projection_base(src)?;
        let (access, origin, pointee) = self.loan_type(dst)?;
        if origin != base_origin
            || (access == LoanAccess::Exclusive && base_access != LoanAccess::Exclusive)
        {
            return Err(InvalidBytecode::LoanOwnerMismatch {
                loan: dst,
                owner: src,
            });
        }
        if pointee != reached {
            return Err(InvalidBytecode::ValueType {
                register: dst,
                expected: reached,
                actual: pointee,
            });
        }
        Ok(())
    }

    fn verify_arguments(
        &self,
        arguments: &[Register],
        expected: impl ExactSizeIterator<Item = Result<ValueType, InvalidBytecode>>,
    ) -> Result<(), InvalidBytecode> {
        if arguments.len() != expected.len() {
            return Err(InvalidBytecode::ArgumentCount {
                expected: expected.len(),
                actual: arguments.len(),
            });
        }
        for (&register, ty) in arguments.iter().zip(expected) {
            self.require_value(register, ty?)?;
        }
        Ok(())
    }

    fn verify_result(
        &self,
        value: Option<Register>,
        result: Option<ValueType>,
    ) -> Result<(), InvalidBytecode> {
        match (value, result) {
            (None, None) => Ok(()),
            (Some(register), Some(ty)) => self.require_value(register, ty),
            _ => Err(InvalidBytecode::ResultPresence),
        }
    }

    fn verify_instruction(
        &self,
        inst: Instruction,
        functions: &[Self],
    ) -> Result<(), InvalidBytecode> {
        let target = |id: InstructionId| {
            if id.index() < self.instructions.len() {
                Ok(())
            } else {
                Err(InvalidBytecode::UnknownTarget(id))
            }
        };
        match inst {
            Instruction::Trap => Ok(()),
            Instruction::StorageLive(reg) | Instruction::StorageDead(reg) => {
                self.register_kind(reg).map(|_| ())
            }
            Instruction::Constant(dst, value) => self.require_type(dst, value.ty()),
            Instruction::Copy(dst, src) => {
                match (self.register_kind(dst)?, self.register_kind(src)?) {
                    (RegisterType::Scalar(_), RegisterType::Scalar(ty)) => {
                        self.require_type(dst, ty)
                    }
                    (a, b) if a == b => Ok(()),
                    _ => Err(InvalidBytecode::IncompatibleRegisters {
                        destination: dst,
                        source: src,
                    }),
                }
            }
            Instruction::Borrow(loan, owner) => {
                let owned = self.owned_type(owner)?;
                if self.loan_type(loan)?.1 != LoanOrigin::Local(owner) {
                    return Err(InvalidBytecode::LoanOwnerMismatch { loan, owner });
                }
                self.require_projection(loan, owner, owned)
            }
            Instruction::ReadLoan(dst, loan) => self.require_value(dst, self.loan_type(loan)?.2),
            Instruction::WriteLoan(loan, value) => {
                let (access, _, ty) = self.loan_type(loan)?;
                if access != LoanAccess::Exclusive {
                    return Err(InvalidBytecode::ExpectedExclusiveLoan(loan));
                }
                self.require_value(value, ty)
            }
            Instruction::BorrowGlobal(dst, global) => {
                let declared = *self
                    .globals
                    .get(global.index())
                    .ok_or(InvalidBytecode::UnknownGlobal(global))?;
                let (_, origin, pointee) = self.loan_type(dst)?;
                if origin != LoanOrigin::Global(global) {
                    return Err(InvalidBytecode::LoanOwnerMismatch {
                        loan: dst,
                        owner: dst,
                    });
                }
                if pointee != declared {
                    return Err(InvalidBytecode::ValueType {
                        register: dst,
                        expected: declared,
                        actual: pointee,
                    });
                }
                Ok(())
            }
            Instruction::LoanField(dst, src, field) => {
                let (container, ..) = self.projection_base(src)?;
                let ValueType::Composite(shape) = container else {
                    return Err(InvalidBytecode::ExpectedTuple(src));
                };
                let Shape::Tuple(fields) = self.shape(shape)? else {
                    return Err(InvalidBytecode::ExpectedTuple(src));
                };
                let reached =
                    fields
                        .get(field as usize)
                        .copied()
                        .ok_or(InvalidBytecode::UnknownField {
                            register: src,
                            field,
                        })?;
                self.require_projection(dst, src, reached)
            }
            Instruction::LoanPayload(dst, src, variant) => {
                let (container, ..) = self.projection_base(src)?;
                let ValueType::Composite(shape) = container else {
                    return Err(InvalidBytecode::ExpectedEnum(src));
                };
                let Shape::Enum(variants) = self.shape(shape)? else {
                    return Err(InvalidBytecode::ExpectedEnum(src));
                };
                let Some(Some(reached)) = variants.get(variant as usize).copied() else {
                    return Err(InvalidBytecode::UnknownVariant {
                        register: src,
                        variant,
                    });
                };
                self.require_projection(dst, src, reached)
            }
            Instruction::LoanElement(dst, src, list) => {
                let (container, ..) = self.projection_base(src)?;
                let ValueType::Composite(shape) = container else {
                    return Err(InvalidBytecode::ExpectedArray(src));
                };
                let Shape::Array { element, bounds } = self.shape(shape)? else {
                    return Err(InvalidBytecode::ExpectedArray(src));
                };
                self.require_indices(src, bounds, list)?;
                self.require_projection(dst, src, *element)
            }
            Instruction::Integer(op, dst, lhs, rhs) => {
                self.require_type(lhs, ScalarType::I64)?;
                self.require_type(rhs, ScalarType::I64)?;
                self.require_type(dst, op.result())
            }
            Instruction::CheckStep(step) => self.require_type(step, ScalarType::I64),
            Instruction::Tuple(dst, list) => {
                let fields = self.tuple_fields(dst)?;
                let list = self
                    .lists
                    .get(list.index())
                    .ok_or(InvalidBytecode::UnknownList(list))?;
                if list.len() != fields.len() {
                    return Err(InvalidBytecode::ArgumentCount {
                        expected: fields.len(),
                        actual: list.len(),
                    });
                }
                for (&field, &ty) in list.iter().zip(fields) {
                    self.require_value(field, ty)?;
                }
                Ok(())
            }
            Instruction::Extract(dst, src, field) => {
                let ty = self.tuple_fields(src)?.get(field as usize).copied().ok_or(
                    InvalidBytecode::UnknownField {
                        register: src,
                        field,
                    },
                )?;
                self.require_value(dst, ty)
            }
            Instruction::EnumUnit(dst, variant) => {
                if self.variant_payload(dst, variant)?.is_some() {
                    return Err(InvalidBytecode::ArgumentCount {
                        expected: 1,
                        actual: 0,
                    });
                }
                Ok(())
            }
            Instruction::EnumWrap(dst, variant, payload) => {
                let Some(ty) = self.variant_payload(dst, variant)? else {
                    return Err(InvalidBytecode::ArgumentCount {
                        expected: 0,
                        actual: 1,
                    });
                };
                self.require_value(payload, ty)
            }
            Instruction::EnumIs(dst, src, variant) => {
                // The enum is held by the register or behind a loan.
                let (container, ..) = self.projection_base(src)?;
                let ValueType::Composite(shape) = container else {
                    return Err(InvalidBytecode::ExpectedEnum(src));
                };
                let Shape::Enum(variants) = self.shape(shape)? else {
                    return Err(InvalidBytecode::ExpectedEnum(src));
                };
                if variants.get(variant as usize).is_none() {
                    return Err(InvalidBytecode::UnknownVariant {
                        register: src,
                        variant,
                    });
                }
                self.require_type(dst, ScalarType::Bool)
            }
            Instruction::GcNew(dst, value) => {
                let RegisterType::Gc(managed) = self.register_kind(dst)? else {
                    return Err(InvalidBytecode::ExpectedHandle(dst));
                };
                let pointee = *self
                    .managed
                    .get(managed.index())
                    .ok_or(InvalidBytecode::UnknownManaged(managed))?;
                self.require_value(value, pointee)
            }
            Instruction::GcBorrow(dst, handle) => {
                let (RegisterType::Gc(managed) | RegisterType::Rc(managed)) =
                    self.register_kind(handle)?
                else {
                    return Err(InvalidBytecode::ExpectedHandle(handle));
                };
                let declared = *self
                    .managed
                    .get(managed.index())
                    .ok_or(InvalidBytecode::UnknownManaged(managed))?;
                let (_, origin, pointee) = self.loan_type(dst)?;
                if origin != LoanOrigin::Heap(managed) {
                    return Err(InvalidBytecode::LoanOwnerMismatch {
                        loan: dst,
                        owner: handle,
                    });
                }
                if pointee != declared {
                    return Err(InvalidBytecode::ValueType {
                        register: dst,
                        expected: declared,
                        actual: pointee,
                    });
                }
                Ok(())
            }
            Instruction::Move(dst, src) => {
                let (a, b) = (self.register_kind(dst)?, self.register_kind(src)?);
                if a != b || a.is_loan() {
                    return Err(InvalidBytecode::IncompatibleRegisters {
                        destination: dst,
                        source: src,
                    });
                }
                Ok(())
            }
            Instruction::Drop(reg) => self.value_type(reg).map(|_| ()),
            Instruction::MoveOut(dst, loan) => {
                let (access, _, pointee) = self.loan_type(loan)?;
                if access != LoanAccess::Exclusive {
                    return Err(InvalidBytecode::ExpectedExclusiveLoan(loan));
                }
                self.require_value(dst, pointee)
            }
            Instruction::RcNew(dst, value) => {
                let RegisterType::Rc(managed) = self.register_kind(dst)? else {
                    return Err(InvalidBytecode::ExpectedHandle(dst));
                };
                let pointee = *self
                    .managed
                    .get(managed.index())
                    .ok_or(InvalidBytecode::UnknownManaged(managed))?;
                self.require_value(value, pointee)
            }
            Instruction::RcClone(dst, src) => {
                let managed = self.counted_source(src, true)?;
                self.require_value(dst, ValueType::Rc(managed))
            }
            Instruction::RcDowngrade(dst, src) => {
                let managed = self.counted_source(src, true)?;
                self.require_value(dst, ValueType::Weak(managed))
            }
            Instruction::RcStrongCount(dst, src) => {
                self.counted_source(src, true)?;
                self.require_type(dst, ScalarType::U64)
            }
            Instruction::RcUpgrade(dst, src) => {
                let managed = self.counted_source(src, false)?;
                let variants = self.enum_variants(dst)?;
                let counted = Some(ValueType::Rc(managed));
                let valid = variants.len() == 2
                    && variants.iter().filter(|v| v.is_none()).count() == 1
                    && variants.contains(&counted);
                if valid {
                    Ok(())
                } else {
                    Err(InvalidBytecode::ExpectedEnum(dst))
                }
            }
            Instruction::EnumTake(dst, src, variant) => {
                let Some(ty) = self.variant_payload(src, variant)? else {
                    return Err(InvalidBytecode::UnknownVariant {
                        register: src,
                        variant,
                    });
                };
                self.require_value(dst, ty)
            }
            Instruction::EnumPayload(dst, src, variant) => {
                let Some(ty) = self.variant_payload(src, variant)? else {
                    return Err(InvalidBytecode::UnknownVariant {
                        register: src,
                        variant,
                    });
                };
                self.require_value(dst, ty)
            }
            Instruction::Float(op, dst, lhs, rhs) => {
                self.require_type(lhs, ScalarType::F64)?;
                self.require_type(rhs, ScalarType::F64)?;
                self.require_type(dst, op.result())
            }
            Instruction::RealDivide(dst, lhs, rhs) => {
                self.require_type(lhs, ScalarType::I64)?;
                self.require_type(rhs, ScalarType::I64)?;
                self.require_type(dst, ScalarType::F64)
            }
            Instruction::Boolean(_, dst, lhs, rhs) => {
                self.require_type(lhs, ScalarType::Bool)?;
                self.require_type(rhs, ScalarType::Bool)?;
                self.require_type(dst, ScalarType::Bool)
            }
            Instruction::NegateInteger(dst, src) => {
                self.require_type(src, ScalarType::I64)?;
                self.require_type(dst, ScalarType::I64)
            }
            Instruction::NegateFloat(dst, src) => {
                self.require_type(src, ScalarType::F64)?;
                self.require_type(dst, ScalarType::F64)
            }
            Instruction::Not(dst, src) => {
                self.require_type(src, ScalarType::Bool)?;
                self.require_type(dst, ScalarType::Bool)
            }
            Instruction::IntegerToFloat(dst, src) => {
                self.require_type(src, ScalarType::I64)?;
                self.require_type(dst, ScalarType::F64)
            }
            Instruction::FloatToInteger(dst, src) => {
                self.require_type(src, ScalarType::F64)?;
                self.require_type(dst, ScalarType::I64)
            }
            Instruction::Closure(dst, target, list) => {
                let (RegisterType::Function(id)
                | RegisterType::BorrowingFunction { signature: id, .. }) =
                    self.register_kind(dst)?
                else {
                    return Err(InvalidBytecode::ExpectedFunction(dst));
                };
                let signature = self.signature(id)?;
                let captures = self.operand_list(list)?;
                let callee = functions
                    .get(target.index())
                    .ok_or(InvalidBytecode::UnknownFunction(target))?;
                let declared = signature.parameters.len();
                if callee.parameters.len() != captures.len() + declared
                    || callee.result != signature.result
                {
                    return Err(InvalidBytecode::ClosureTarget(target));
                }
                self.verify_guest_arguments(
                    captures,
                    &callee.parameters[..captures.len()],
                    callee,
                )?;
                // A closure that captured loans borrows the root of each of them.
                if let RegisterType::BorrowingFunction { origin, .. } = self.register_kind(dst)? {
                    for &capture in captures {
                        if let Some(captured) = self.register_kind(capture)?.loan_origin()
                            && captured != origin
                        {
                            return Err(InvalidBytecode::LoanOwnerMismatch {
                                loan: dst,
                                owner: capture,
                            });
                        }
                    }
                }
                for (&expected, &parameter) in signature
                    .parameters
                    .iter()
                    .zip(&callee.parameters[captures.len()..])
                {
                    if callee.value_type(parameter)? != expected {
                        return Err(InvalidBytecode::ClosureTarget(target));
                    }
                }
                Ok(())
            }
            Instruction::TextConstant(dst, text) => {
                if text.index() >= self.texts.len() {
                    return Err(InvalidBytecode::UnknownText(text));
                }
                self.require_text(dst)
            }
            Instruction::TextConcat(dst, lhs, rhs) => {
                self.require_text(lhs)?;
                self.require_text(rhs)?;
                self.require_text(dst)
            }
            Instruction::TextCompare(_, dst, lhs, rhs) => {
                self.require_text(lhs)?;
                self.require_text(rhs)?;
                self.require_type(dst, ScalarType::Bool)
            }
            Instruction::TextLength(dst, text) => {
                self.require_text(text)?;
                self.require_type(dst, ScalarType::I64)
            }
            Instruction::TextLeft(dst, text, count) | Instruction::TextRight(dst, text, count) => {
                self.require_text(text)?;
                self.require_type(count, ScalarType::I64)?;
                self.require_text(dst)
            }
            Instruction::TextMid(dst, text, list) => {
                self.require_text(text)?;
                let [start, count] = self.operand_list(list)? else {
                    return Err(InvalidBytecode::ArgumentCount {
                        expected: 2,
                        actual: self.operand_list(list)?.len(),
                    });
                };
                self.require_type(*start, ScalarType::I64)?;
                self.require_type(*count, ScalarType::I64)?;
                self.require_text(dst)
            }
            Instruction::ArrayDeclare(dst) => self.array_shape(dst).map(|_| ()),
            Instruction::ArrayNew(dst, list) => {
                let (element, bounds) = self.array_shape(dst)?;
                let items = self.operand_list(list)?;
                if array_len(bounds) != Some(items.len()) {
                    return Err(InvalidBytecode::ArgumentCount {
                        expected: array_len(bounds).unwrap_or(0),
                        actual: items.len(),
                    });
                }
                items
                    .iter()
                    .try_for_each(|&item| self.require_value(item, element))
            }
            Instruction::ArrayLoad(dst, array, list) => {
                let (element, bounds) = self.array_shape(array)?;
                self.require_indices(array, bounds, list)?;
                self.require_value(dst, element)
            }
            Instruction::ArrayStore(array, list, value) => {
                let (element, bounds) = self.array_shape(array)?;
                self.require_indices(array, bounds, list)?;
                self.require_value(value, element)
            }
            Instruction::ArrayLength(dst, array) => {
                let (_, bounds) = self.array_shape(array)?;
                if bounds.len() != 1 {
                    return Err(InvalidBytecode::ArrayRank {
                        register: array,
                        expected: 1,
                        actual: bounds.len(),
                    });
                }
                self.require_type(dst, ScalarType::I64)
            }
            Instruction::Jump(to) => target(to),
            Instruction::Branch(condition, yes, no) => {
                self.require_type(condition, ScalarType::Bool)?;
                target(yes)?;
                target(no)
            }
            Instruction::Call(site) => self
                .calls
                .get(site.index())
                .map(|_| ())
                .ok_or(InvalidBytecode::UnknownCallSite(site)),
            Instruction::Return(value) => self.verify_result(value, self.result),
        }
    }

    fn transfer(
        &self,
        inst: Instruction,
        state: &mut [StorageState],
        borrowers: &std::collections::HashMap<Register, Vec<usize>>,
    ) -> Result<(), InvalidBytecode> {
        use StorageState::*;
        let has_borrower = |reg, access: Option<LoanAccess>, state: &[StorageState]| {
            borrowers.get(&reg).is_some_and(|loans| {
                loans.iter().copied().any(|index| {
                    state[index] == Initialized
                        && access.is_none_or(|expected| {
                            self.registers[index].loan_access() == Some(expected)
                        })
                })
            })
        };
        match inst {
            Instruction::StorageLive(reg) => {
                let slot = &mut state[reg.index()];
                if *slot != Dead {
                    return Err(InvalidBytecode::StorageAlreadyLive(reg));
                }
                self.update_storage(inst, state);
                return Ok(());
            }
            Instruction::StorageDead(reg) => {
                if has_borrower(reg, None, state) {
                    return Err(InvalidBytecode::BorrowedOwner(reg));
                }
                let slot = &mut state[reg.index()];
                if *slot == Dead {
                    return Err(InvalidBytecode::StorageAlreadyDead(reg));
                }
                self.update_storage(inst, state);
                return Ok(());
            }
            _ => {}
        }
        let access = inst.register_access(&self.calls, &self.lists);
        let (loan, owner) = match inst {
            Instruction::Borrow(loan, owner) => (Some(loan), Some(owner)),
            Instruction::LoanField(loan, src, _)
            | Instruction::LoanPayload(loan, src, _)
            | Instruction::LoanElement(loan, src, _)
                if !self.registers[src.index()].is_loan() =>
            {
                (Some(loan), Some(src))
            }
            _ => (None, None),
        };
        if let Some(loan) = loan
            && let Some(owner) = owner
            && let RegisterType::Loan {
                access: LoanAccess::Exclusive,
                ..
            } = self.registers[loan.index()]
            && has_borrower(owner, None, state)
        {
            return Err(InvalidBytecode::BorrowedOwner(owner));
        }
        for reg in access.operands() {
            if has_borrower(reg, Some(LoanAccess::Exclusive), state) {
                return Err(InvalidBytecode::ExclusiveOwnerAccess(reg));
            }
        }
        for reg in access.reads(&self.registers) {
            // A drop of a value that was already moved out releases nothing.
            if inst == Instruction::Drop(reg) && state[reg.index()] != Dead {
                continue;
            }
            match state[reg.index()] {
                Dead => return Err(InvalidBytecode::DeadStorageAccess(reg)),
                Uninitialized | MaybeInitialized => {
                    return Err(InvalidBytecode::Uninitialized(reg));
                }
                Initialized => {}
            }
        }
        if let Some(reg) = access.write {
            if has_borrower(reg, None, state) {
                return Err(InvalidBytecode::BorrowedOwner(reg));
            }
            let slot = &mut state[reg.index()];
            if *slot == Dead {
                return Err(InvalidBytecode::DeadStorageAccess(reg));
            }
        }
        self.update_storage(inst, state);
        if let Instruction::Return(_) = inst
            && let Some(index) = state.iter().position(|state| *state != Dead)
        {
            let Ok(index) = u32::try_from(index) else {
                unreachable!("only registers referenced by a verified u32 ID become live");
            };
            return Err(InvalidBytecode::LiveStorageAtReturn(Register(index)));
        }
        Ok(())
    }

    fn update_storage(&self, inst: Instruction, state: &mut [StorageState]) {
        use StorageState::*;
        let change = match inst {
            Instruction::StorageLive(reg) => Some((reg, Uninitialized)),
            Instruction::StorageDead(reg) | Instruction::Return(Some(reg)) => Some((reg, Dead)),
            _ => inst
                .register_access(&self.calls, &self.lists)
                .write
                .map(|reg| (reg, Initialized)),
        };
        if let Some((reg, new)) = change {
            state[reg.index()] = new;
        }
        // A move, a drop or a take leaves its source without a value.
        if let Instruction::Move(_, source)
        | Instruction::Drop(source)
        | Instruction::EnumTake(_, source, _) = inst
        {
            state[source.index()] = Uninitialized;
        }
    }
}
