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

use crate::{Span, ir::Program};
use bound::ScalarHostBinding;
use std::{collections::HashMap, fmt, sync::Arc};

pub use crate::ir::TextStyle;
pub use bound::{HostFailure, ScalarSdk};
pub use codec::encode;
pub use debug::{StatementArrival, StatementBoundary};
pub use frame::{FrameLayout, FrameSlot};
pub use lower::LowerError;
pub(crate) use run::ScalarFrameSnapshot;
pub use run::RecoveredValue;
pub use run::ScalarRecoveryError;
pub use run::{
    Activation, ExecutionLimits, ExecutionPosition, ExecutionStatus, GeneratorFault, HeapStats,
    InputSource, ObjectFault, OutputSink, PathFault, RunError, ServiceFault,
};
pub use sdk::SdkCompileError;

/// Version 35 adds debug locals and pseudo-random numbers. Version 34 adds native callables in erased values. Version 33 adds service contexts. Version 32 adds owned erased values, constant aggregates and count arithmetic. Version 31 adds run-time module loading. Version 30 adds by-value trait receivers. Version 29 adds open objects and erased values. Version 28 adds tasks and clocks. Version 27 adds channels. Version 26 adds generators. Version 25 adds retained field locations. Version 24 adds storage paths and mapped lookups. Version 23 adds interactive input. Version 22 adds trait objects and dynamic dispatch. Version 21 adds cloning, string transformations and integer helpers. Version 20 adds vectors, ranges and cells. Version 19 adds slices of arrays. Version 18 adds counted owners, moves and drops. Version 17 adds GC handles and the managed-type table. Version 16 adds module-level variables. Version 15 adds closures that borrow. Version 14 generalizes loans to any pointee and adds projected loans.
/// Version 13 adds closures: construction and indirect calls. Version 12 adds strings and fixed-size arrays. Version 11 adds integer real division. Version 10 adds binary64 values, unary operators and numeric conversion.
/// Version 9 adds composite registers: tuples and enums. Version 8 added
/// output call targets and the loop step check. Version 7
/// carried scalar loans and borrowed call parameters.
pub const FORMAT_VERSION: u32 = 35;

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
index!(ContractId);
index!(FrozenId);
index!(ShapeId);
index!(OperandListId);
index!(TextId);
index!(SignatureId);
index!(GlobalId);
index!(ManagedId);
index!(TraitId);
index!(ImplementationId);

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
    /// A shared, erased object that implements the trait.
    Object(TraitId),
    /// A relative storage recipe: a list of projections, or the missing path.
    Path,
    /// A retained reference to a field of a traced record. The managed type
    /// is the field's type.
    Location(ManagedId),
    /// A block of memory an allocator handed out. It is a token: the engine
    /// keeps no accounting of what it stands for.
    Allocation,
    /// A suspended routine that yields, resumes and returns values. Its
    /// description reuses the signature table: the parameters are what it
    /// yields and what a resume passes in, the result is what it returns.
    /// The generator owns its frame, so it is released with its owner.
    Generator(SignatureId),
    /// The sending end of a channel. The managed type is the element type.
    /// Ends are counted: releasing the last sender closes the channel for
    /// receivers once the queue drains, and releasing the last receiver
    /// closes it and discards the queue.
    Sender(ManagedId),
    /// The receiving end of a channel. The managed type is the element type.
    Receiver(ManagedId),
    /// A spawned task. The managed type is the type of its result, the unit
    /// tuple when it returns nothing.
    Task(ManagedId),
    /// A capability to sleep.
    Clock,
    /// An object with named properties and shared identity. Copies of the
    /// value are the same object.
    OpenObject,
    /// A value of any erasable type together with its type. What it holds is
    /// checked when it is read back.
    Dynamic,
    /// A failure to load a module or to view an object, with its message.
    Failure,
    /// An owned value of any type together with its type. Unlike `Dynamic` it
    /// may own counted values, so it is released and moved like an owner.
    Any,
    /// A service context: a view of which providers are visible.
    Context,
    /// The registration of a provider, which withdraws it.
    Registration,
    /// The identity of a service, by its linked declaration.
    ServiceKey,
    /// A live reference to a service the trait describes: each call finds the
    /// provider the context sees at that moment.
    ServiceRef(TraitId),
    /// A function the standard library provides, named by its declaration. It
    /// has no body here: calling it through an erased value compiles an adapter.
    NativeCallable,
}

/// A method a trait declares: what its receiver may do and its other types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Method {
    pub receiver: LoanAccess,
    /// The routine takes a copy of the receiver instead of a loan of it, which
    /// only a receiver that owns nothing allows.
    pub by_value: bool,
    pub parameters: Box<[ValueType]>,
    pub result: Option<ValueType>,
}

/// The methods an object of the trait can be asked to run, in call order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraitDescription {
    pub methods: Box<[Method]>,
}

/// One type's implementation of a trait. The routines take a loan of the
/// receiver first and then the method's own parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Implementation {
    pub trait_id: TraitId,
    pub receiver: ValueType,
    pub methods: Box<[FunctionId]>,
}

/// A set of named properties with a type each. An object literal makes its
/// properties required, a dynamic literal makes them configurable, and a view
/// requires the named properties to hold the given types (`Dynamic` accepts any).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contract {
    pub required: bool,
    pub fields: Box<[(Arc<str>, ValueType)]>,
}

/// Where a source variable lives while its routine runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugPlace {
    /// A register of the frame, readable while the register is initialized.
    Register(Register),
    /// A module-level variable.
    Module(GlobalId),
}

/// A named source variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugLocal {
    pub name: Arc<str>,
    pub place: DebugPlace,
}

/// A constant value known when the routine is lowered: scalars, strings and
/// tuples of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frozen {
    Scalar(Scalar),
    Text(Arc<str>),
    Tuple(Box<[Frozen]>),
}

/// An operator on erased values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DynamicOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    Equal,
    NotEqual,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    And,
    Or,
    Negate,
    Not,
}

fn is_prefix<T: PartialEq>(shorter: &[T], longer: &[T]) -> bool {
    shorter.len() <= longer.len() && shorter == &longer[..shorter.len()]
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
            | Self::Weak(_)
            | Self::Object(_)
            | Self::Path
            | Self::Location(_)
            | Self::Allocation
            | Self::Generator(_)
            | Self::Sender(_)
            | Self::Receiver(_)
            | Self::Task(_)
            | Self::Clock
            | Self::OpenObject
            | Self::Dynamic
            | Self::Failure
            | Self::Any
            | Self::Context
            | Self::Registration
            | Self::ServiceKey
            | Self::ServiceRef(_)
            | Self::NativeCallable => None,
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
    /// A growable sequence of initialized elements, indexed from zero.
    Vec { element: ValueType },
    /// A mutable cell: one value that can be replaced through a shared loan.
    Cell { element: ValueType },
    /// A window onto the elements of an array. It has no storage of its own:
    /// it exists only as the pointee of a loan.
    Slice { element: ValueType },
}

/// A 64-bit signed integer value.
const I64: ValueType = ValueType::Scalar(ScalarType::I64);

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
    /// A shared, erased object that implements a trait.
    Object(TraitId),
    /// A relative storage recipe.
    Path,
    /// A retained reference to a field of a traced record.
    Location(ManagedId),
    /// A block of memory an allocator handed out.
    Allocation,
    /// A suspended routine.
    Generator(SignatureId),
    /// The sending end of a channel.
    Sender(ManagedId),
    /// The receiving end of a channel.
    Receiver(ManagedId),
    /// A spawned task.
    Task(ManagedId),
    /// A capability to sleep.
    Clock,
    /// An object with named properties.
    OpenObject,
    /// An erased value.
    Dynamic,
    /// A failure to load a module or to view an object: a message and what kind.
    Failure,
    /// An owned erased value.
    Any,
    /// A service context.
    Context,
    /// A provider's registration.
    Registration,
    /// A service's identity.
    ServiceKey,
    /// A live reference to a service.
    ServiceRef(TraitId),
    /// A native function value.
    NativeCallable,
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
            ValueType::Object(id) => Self::Object(id),
            ValueType::Path => Self::Path,
            ValueType::Location(managed) => Self::Location(managed),
            ValueType::Allocation => Self::Allocation,
            ValueType::Generator(signature) => Self::Generator(signature),
            ValueType::Sender(managed) => Self::Sender(managed),
            ValueType::Receiver(managed) => Self::Receiver(managed),
            ValueType::Task(managed) => Self::Task(managed),
            ValueType::Clock => Self::Clock,
            ValueType::OpenObject => Self::OpenObject,
            ValueType::Dynamic => Self::Dynamic,
            ValueType::Failure => Self::Failure,
            ValueType::Any => Self::Any,
            ValueType::Context => Self::Context,
            ValueType::Registration => Self::Registration,
            ValueType::ServiceKey => Self::ServiceKey,
            ValueType::ServiceRef(id) => Self::ServiceRef(id),
            ValueType::NativeCallable => Self::NativeCallable,
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
            Self::Object(id) => ValueType::Object(id),
            Self::Path => ValueType::Path,
            Self::Location(managed) => ValueType::Location(managed),
            Self::Allocation => ValueType::Allocation,
            Self::Generator(signature) => ValueType::Generator(signature),
            Self::Sender(managed) => ValueType::Sender(managed),
            Self::Receiver(managed) => ValueType::Receiver(managed),
            Self::Task(managed) => ValueType::Task(managed),
            Self::Clock => ValueType::Clock,
            Self::OpenObject => ValueType::OpenObject,
            Self::Dynamic => ValueType::Dynamic,
            Self::Failure => ValueType::Failure,
            Self::Any => ValueType::Any,
            Self::Context => ValueType::Context,
            Self::Registration => ValueType::Registration,
            Self::ServiceKey => ValueType::ServiceKey,
            Self::ServiceRef(id) => ValueType::ServiceRef(id),
            Self::NativeCallable => ValueType::NativeCallable,
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

/// Where the variants a channel operation builds sit in its result type, packed
/// at lowering from the declared encodings: result ok and error, then for a
/// send error the closed, full and limit variants, and for a receive error the
/// closed and empty variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelVariants(pub u32);

impl ChannelVariants {
    const WIDTH: u32 = 6;

    pub fn pack(fields: [u32; 5]) -> Option<Self> {
        let mut packed = 0;
        for (index, field) in fields.into_iter().enumerate() {
            if field >= 1 << Self::WIDTH {
                return None;
            }
            packed |= field << (index as u32 * Self::WIDTH);
        }
        Some(Self(packed))
    }

    pub fn field(self, index: usize) -> u32 {
        (self.0 >> (index as u32 * Self::WIDTH)) & ((1 << Self::WIDTH) - 1)
    }

    pub fn ok(self) -> u32 {
        self.field(0)
    }

    pub fn err(self) -> u32 {
        self.field(1)
    }

    /// The first, second and third variants of the error type.
    pub fn failure(self, index: usize) -> u32 {
        self.field(2 + index)
    }
}

/// What one line of input becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    /// The line itself, without its terminator.
    Text,
    /// A decimal 64-bit integer; surrounding white space is ignored.
    Integer,
    /// A finite decimal number; surrounding white space is ignored.
    Float,
    /// `TRUE` or `FALSE` in any letter case; surrounding white space is ignored.
    Bool,
}

impl InputKind {
    pub(crate) fn value_type(self) -> ValueType {
        match self {
            Self::Text => ValueType::Text,
            Self::Integer => ValueType::Scalar(ScalarType::I64),
            Self::Float => ValueType::Scalar(ScalarType::F64),
            Self::Bool => ValueType::Scalar(ScalarType::Bool),
        }
    }
}

/// A string transformation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextMapOp {
    /// Remove leading and trailing white space.
    Trim,
    Upper,
    Lower,
}

/// A test of one string against another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextTestOp {
    Contains,
    StartsWith,
    EndsWith,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extremum {
    Min,
    Max,
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
    /// Erase a value into a shared object that implements a trait.
    TraitBox(Register, Register, ImplementationId),
    /// Allocate a heap object holding a copy of the value.
    GcNew(Register, Register),
    /// Borrow the heap object a handle refers to.
    GcBorrow(Register, Register),
    /// A half-open integer range.
    RangeNew(Register, Register, Register),
    /// A closed integer range, stored half-open. The end must not overflow.
    RangeInclusive(Register, Register, Register),
    /// A vector of the integers in a range.
    VecFrom(Register, Register),
    /// A vector of the listed elements.
    VecNew(Register, OperandListId),
    /// Append to the vector held by an owner or behind an exclusive loan.
    VecPush(Register, Register),
    /// Number of elements in a vector.
    VecLength(Register, Register),
    /// A cell holding the value.
    CellNew(Register, Register),
    /// A copy of the value a cell holds.
    CellRead(Register, Register),
    /// Replace the value a cell holds, through any loan to the cell.
    CellWrite(Register, Register),
    /// Replace the value a cell holds and return the old one.
    CellReplace(Register, Register, Register),
    /// A copy of the value an owner holds or a loan points at. Counted owners
    /// inside gain an owner and handles keep their identity.
    Clone(Register, Register),
    TextMap(TextMapOp, Register, Register),
    TextTest(TextTestOp, Register, Register, Register),
    /// Replace every occurrence of the first string of the list by the second.
    TextReplace(Register, Register, OperandListId),
    /// The text spelling of a scalar or string, as `print` writes it.
    ToText(Register, Register),
    /// Checked absolute value.
    Abs(Register, Register),
    /// A count as a signed integer; a count above the maximum fails.
    CountToInteger(Register, Register),
    /// A signed integer as a count; a negative value fails.
    IntegerToCount(Register, Register),
    /// Parse the text as a decimal 64-bit integer, with no surrounding white
    /// space. The result is a two-variant enum: one carries the integer, the
    /// other a payload-free error.
    ParseInteger(Register, Register),
    /// A channel that holds up to the capacity; the result pairs its sender
    /// with its receiver.
    ChannelBounded(Register, Register),
    /// A channel without a capacity limit.
    ChannelUnbounded(Register),
    /// Another end of the same channel.
    ChannelClone(Register, Register),
    /// Close the channel from either end: sends fail from now on, and a
    /// closing receiver discards what is queued.
    ChannelClose(Register),
    ChannelIsClosed(Register, Register),
    /// Send, waiting while the channel is full. Operands: the sender and the
    /// value. The result is a result of unit or a send error carrying the value
    /// back.
    ChannelSend(Register, OperandListId, ChannelVariants),
    /// Send without waiting.
    ChannelTrySend(Register, OperandListId, ChannelVariants),
    /// Receive, waiting while the channel is empty and open.
    ChannelReceive(Register, Register, ChannelVariants),
    /// Receive without waiting.
    ChannelTryReceive(Register, Register, ChannelVariants),
    /// The value a send error carries, whichever failure it reports.
    SendErrorValue(Register, Register),
    /// A new object with the contract's properties, valued by the list.
    ObjectNew(Register, OperandListId, ContractId),
    /// Read a property into a register of its type, or of `Dynamic`.
    ObjectGet(Register, Register, TextId),
    /// Read a property by a computed name.
    ObjectGetKey(Register, Register, Register),
    /// Write a property, adding it to a configurable object.
    ObjectSet(Register, TextId, Register),
    /// Write a property by a computed name; the list holds the name and the value.
    ObjectSetKey(Register, OperandListId),
    /// Remove a configurable property; the result says whether it existed.
    ObjectDelete(Register, Register, TextId),
    ObjectDeleteKey(Register, Register, Register),
    /// Check that the object has the contract's properties with those types
    /// and require them from now on, through every alias.
    ObjectView(Register, Register, ContractId),
    /// Like a view, but a failure is a value: the result is the viewed object or
    /// a failure carrying why the view cannot be made.
    ObjectTryView(Register, Register, ContractId),
    /// Make the object readonly through every alias.
    ObjectFreeze(Register),
    /// Load a module by path at run time. The result is the module's namespace
    /// object, or a failure.
    ModuleLoad(Register, Register),
    /// Whether a failure is of a kind: 0 not found, 1 denied, 2 failed checks.
    FailureFlag(Register, Register, u32),
    FailureMessage(Register, Register),
    /// Call the callable an erased value holds with erased or typed arguments.
    /// The result is erased.
    DynamicInvoke(Register, Register, OperandListId),
    /// Wrap the value with its type.
    Erase(Register, Register),
    /// The native function with this declaration number.
    NativeCallableConst(Register, u32),
    /// A new root service context.
    ServiceContext(Register),
    /// A context that sees what its parent sees.
    ServiceExtend(Register, Register),
    /// A context that gives the key its own, separate providers.
    ServiceIsolate(Register, Register, Register),
    /// The key at this index of the linked service keys.
    ServiceKeyConst(Register, u32),
    /// A live reference to the key's service in the context.
    ServiceReference(Register, Register, Register),
    /// Provide a value for the key in the context. The list holds the context,
    /// the key and the value; the result is the registration.
    ServiceProvide(Register, OperandListId),
    ServiceAvailable(Register, Register, Register),
    /// Run the callback in a context that sees the provider the key resolves to
    /// now and keeps seeing it. The list holds the context, the key and the
    /// callback.
    ServiceInject(OperandListId),
    /// Run the callback when the context is disposed.
    ServiceOnDispose(Register, Register),
    /// Withdraw the registration or dispose the context, running the cleanup
    /// of everything that depended on it.
    ServiceDispose(Register),
    /// A pseudo-random real in `[0, bound)`, from the activation's seed.
    Random(Register, Register),
    /// A constant aggregate.
    FrozenValue(Register, FrozenId),
    /// Arithmetic and comparison on counts, checked like integer arithmetic.
    Count(IntegerOp, Register, Register, Register),
    /// Move the value into an owned erased value.
    AnyErase(Register, Register),
    /// Move the value out of an owned erased value that holds the destination's
    /// type exactly.
    AnyChecked(Register, Register),
    /// Read an erased value as the destination's type, which it must have.
    DynamicCast(Register, Register),
    DynamicBinary(DynamicOp, Register, Register, Register),
    DynamicUnary(DynamicOp, Register, Register),
    /// Start a task that runs the closure, which takes no arguments, and
    /// returns its handle. What the closure captured moves into the task.
    TaskSpawn(Register, Register),
    /// Wait for the task to end and take its outcome: a four-variant enum
    /// holding the result of a completed task, then failed, cancelled and
    /// stopped.
    TaskJoin(Register, Register),
    /// Ask the task to stop; it ends the next time it is scheduled.
    TaskCancel(Register),
    TaskIsFinished(Register, Register),
    ClockNew(Register),
    ClockClone(Register, Register),
    /// Wait for the given number of milliseconds.
    ClockSleep(Register, Register),
    /// Suspend the generator whose body this is, handing the value to whoever
    /// resumed it. The destination receives the value passed to the next
    /// resume.
    Yield(Register, Register),
    /// Run a new generator to its first suspension. The result is a step: a
    /// two-variant enum, the yielded value then the returned value.
    GeneratorStart(Register, Register),
    /// Start a new generator or continue a suspended one that takes the unit
    /// resume value.
    GeneratorNext(Register, Register),
    /// Continue a suspended generator that takes the unit resume value.
    GeneratorResumeUnit(Register, Register),
    /// Continue a suspended generator with a value.
    GeneratorResume(Register, Register, Register),
    /// Release a generator and everything its suspended frame owns. Closing
    /// twice is not an error.
    GeneratorClose(Register),
    /// The fixed size and alignment a counted allocation asks for.
    LayoutForRc(Register),
    /// Ask the global allocator for a block of the layout. The result is a
    /// two-variant enum: the allocation, or a payload-free failure.
    Allocate(Register, Register),
    /// Give a block back.
    Deallocate(Register),
    /// A counted owner whose block came from an allocator. The list holds the
    /// allocator state, the allocation result and the value. When the last
    /// owner is released the routine runs with a loan of the allocator state
    /// and the allocation.
    RcNewIn(Register, OperandListId, FunctionId),
    /// A retained reference to a field of the traced record the handle, or a
    /// loan of the handle, refers to.
    LocationField(Register, Register, u32),
    /// Copy the value the location refers to.
    LocationLoad(Register, Register),
    /// Replace the value the location refers to.
    LocationStore(Register, Register),
    /// The empty storage path that starts at the root value.
    PathNew(Register, Register),
    /// The path that reaches nothing.
    PathMissing(Register),
    /// Append a step into the contents of a cell, handle or counted owner.
    PathTarget(Register, Register),
    /// Append a step into a field of a record.
    PathField(Register, Register, u32),
    /// Append a step to an element of a vector or array.
    PathElement(Register, Register, Register),
    /// Read one line from the input source into the register.
    Input(Register, InputKind),
    /// Read one line from the input source through an exclusive loan.
    InputLoan(Register, InputKind),
    Extremum(Extremum, Register, Register, Register),
    /// View the elements of an array, or of another slice, through a loan.
    SliceOf(Register, Register),
    /// Number of elements in an array or slice.
    SliceLength(Register, Register),
    /// Checked sum of the integer elements of an array or slice.
    Sum(Register, Register),
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
    /// Fail unless the enum holds this variant, for a payload nobody reads.
    EnumExpect(Register, u32),
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
    /// A method of the object held by the register. The routine receives a
    /// loan of the object's receiver before the arguments.
    Method {
        object: Register,
        method: u32,
    },
    /// Write one line of text. The template lives in the owning function.
    Output(OutputId),
    /// Call the mapper closure held by the register with the arguments, then
    /// apply the storage path it returns to the first argument. The
    /// destination receives an option of the value the path reaches.
    Mapped(Register),
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

#[derive(Debug, Clone, PartialEq, Eq)]
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
            Input(dst, _) | PathMissing(dst) => ([None, None], &[], Some(dst)),
            ObjectNew(dst, list, _) => ([None, None], &lists[list.index()], Some(dst)),
            ObjectGet(dst, object, _) | ObjectDelete(dst, object, _) => {
                ([Some(object), None], &[], Some(dst))
            }
            ObjectGetKey(dst, object, key) | ObjectDeleteKey(dst, object, key) => {
                ([Some(object), Some(key)], &[], Some(dst))
            }
            ObjectSet(object, _, value) => ([Some(object), Some(value)], &[], None),
            ObjectSetKey(object, list) => ([Some(object), None], &lists[list.index()], None),
            ModuleLoad(dst, path) => ([Some(path), None], &[], Some(dst)),
            FailureFlag(dst, failure, _) | FailureMessage(dst, failure) => {
                ([Some(failure), None], &[], Some(dst))
            }
            DynamicInvoke(dst, callee, list) => {
                ([Some(callee), None], &lists[list.index()], Some(dst))
            }
            ServiceContext(dst) | ServiceKeyConst(dst, _) | NativeCallableConst(dst, _) => {
                ([None, None], &[], Some(dst))
            }
            ServiceExtend(dst, ctx) => ([Some(ctx), None], &[], Some(dst)),
            ServiceIsolate(dst, ctx, key)
            | ServiceReference(dst, ctx, key)
            | ServiceAvailable(dst, ctx, key) => ([Some(ctx), Some(key)], &[], Some(dst)),
            ServiceProvide(dst, list) => ([None, None], &lists[list.index()], Some(dst)),
            ServiceInject(list) => ([None, None], &lists[list.index()], None),
            ServiceOnDispose(ctx, callback) => ([Some(ctx), Some(callback)], &[], None),
            ServiceDispose(target) => ([Some(target), None], &[], None),
            FrozenValue(dst, _) => ([None, None], &[], Some(dst)),
            Random(dst, bound) => ([Some(bound), None], &[], Some(dst)),
            Count(_, dst, lhs, rhs) => ([Some(lhs), Some(rhs)], &[], Some(dst)),
            AnyErase(dst, source) | AnyChecked(dst, source) => {
                ([Some(source), None], &[], Some(dst))
            }
            ObjectView(dst, source, _)
            | ObjectTryView(dst, source, _)
            | Erase(dst, source)
            | DynamicCast(dst, source) => ([Some(source), None], &[], Some(dst)),
            DynamicUnary(_, dst, source) => ([Some(source), None], &[], Some(dst)),
            DynamicBinary(_, dst, lhs, rhs) => ([Some(lhs), Some(rhs)], &[], Some(dst)),
            ObjectFreeze(object) => ([Some(object), None], &[], None),
            TaskSpawn(dst, closure) | TaskJoin(dst, closure) => {
                ([Some(closure), None], &[], Some(dst))
            }
            TaskCancel(task) => ([Some(task), None], &[], None),
            TaskIsFinished(dst, task) | ClockClone(dst, task) => {
                ([Some(task), None], &[], Some(dst))
            }
            ClockNew(dst) => ([None, None], &[], Some(dst)),
            ClockSleep(clock, duration) => ([Some(clock), Some(duration)], &[], None),
            ChannelBounded(dst, capacity) => ([Some(capacity), None], &[], Some(dst)),
            ChannelUnbounded(dst) => ([None, None], &[], Some(dst)),
            ChannelClone(dst, end) | ChannelIsClosed(dst, end) | SendErrorValue(dst, end) => {
                ([Some(end), None], &[], Some(dst))
            }
            ChannelClose(end) => ([Some(end), None], &[], None),
            ChannelSend(dst, list, _) | ChannelTrySend(dst, list, _) => {
                ([None, None], &lists[list.index()], Some(dst))
            }
            ChannelReceive(dst, end, _) | ChannelTryReceive(dst, end, _) => {
                ([Some(end), None], &[], Some(dst))
            }
            Yield(dst, value) => ([Some(value), None], &[], Some(dst)),
            GeneratorStart(dst, generator)
            | GeneratorNext(dst, generator)
            | GeneratorResumeUnit(dst, generator) => ([Some(generator), None], &[], Some(dst)),
            GeneratorResume(dst, generator, value) => {
                ([Some(generator), Some(value)], &[], Some(dst))
            }
            GeneratorClose(generator) => ([Some(generator), None], &[], None),
            LayoutForRc(dst) => ([None, None], &[], Some(dst)),
            Allocate(dst, layout) => ([Some(layout), None], &[], Some(dst)),
            Deallocate(allocation) => ([Some(allocation), None], &[], None),
            RcNewIn(dst, list, _) => ([None, None], &lists[list.index()], Some(dst)),
            LocationField(dst, src, _) | LocationLoad(dst, src) => {
                ([Some(src), None], &[], Some(dst))
            }
            LocationStore(location, value) => ([Some(location), Some(value)], &[], None),
            PathNew(dst, src) | PathTarget(dst, src) | PathField(dst, src, _) => {
                ([Some(src), None], &[], Some(dst))
            }
            PathElement(dst, src, index) => ([Some(src), Some(index)], &[], Some(dst)),
            InputLoan(loan, _) => ([Some(loan), None], &[], None),
            BorrowGlobal(dst, _) => ([None, None], &[], Some(dst)),
            Clone(dst, src)
            | TextMap(_, dst, src)
            | ToText(dst, src)
            | Abs(dst, src)
            | CountToInteger(dst, src)
            | IntegerToCount(dst, src)
            | ParseInteger(dst, src) => ([Some(src), None], &[], Some(dst)),
            TextTest(_, dst, lhs, rhs) | Extremum(_, dst, lhs, rhs) => {
                ([Some(lhs), Some(rhs)], &[], Some(dst))
            }
            TextReplace(dst, text, list) => ([Some(text), None], &lists[list.index()], Some(dst)),
            RangeNew(dst, start, end) | RangeInclusive(dst, start, end) => {
                ([Some(start), Some(end)], &[], Some(dst))
            }
            VecFrom(dst, src) | VecLength(dst, src) | CellNew(dst, src) | CellRead(dst, src) => {
                ([Some(src), None], &[], Some(dst))
            }
            VecNew(dst, list) => ([None, None], &lists[list.index()], Some(dst)),
            VecPush(target, value) | CellWrite(target, value) => {
                ([Some(target), Some(value)], &[], None)
            }
            CellReplace(dst, cell, value) => ([Some(cell), Some(value)], &[], Some(dst)),
            SliceOf(dst, src) | SliceLength(dst, src) | Sum(dst, src) => {
                ([Some(src), None], &[], Some(dst))
            }
            TraitBox(dst, value, _) => ([Some(value), None], &[], Some(dst)),
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
                    CallTarget::Closure(register)
                    | CallTarget::Mapped(register)
                    | CallTarget::Method {
                        object: register, ..
                    } => Some(register),
                    _ => None,
                };
                ([callee, None], &call.arguments, call.destination)
            }
            Closure(dst, _, list) => ([None, None], &lists[list.index()], Some(dst)),
            Tuple(dst, list) => ([None, None], &lists[list.index()], Some(dst)),
            EnumExpect(src, _) => ([Some(src), None], &[], None),
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
    /// The source variables of the routine and where each lives, for debuggers.
    pub locals: Box<[DebugLocal]>,
    /// Constant aggregates, built whole.
    pub frozen: Box<[Frozen]>,
    /// Property sets that objects are made with and viewed through.
    pub contracts: Box<[Contract]>,
    /// The description of the generator this routine is the body of, if any.
    /// Calling a closure of such a routine makes a generator instead of
    /// running it; the routine's result is what the generator returns.
    pub generator: Option<SignatureId>,
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
    pub traits: Box<[TraitDescription]>,
    pub implementations: Box<[Implementation]>,
    pub functions: Box<[FunctionImage]>,
}

/// Read-only input for execution and future backend preparation.
#[derive(Debug)]
pub struct VerifiedFunction {
    locals: Box<[DebugLocal]>,
    frozen: Box<[Frozen]>,
    contracts: Box<[Contract]>,
    generator: Option<SignatureId>,
    registers: Box<[RegisterType]>,
    parameters: Box<[Register]>,
    shapes: Arc<[Shape]>,
    signatures: Arc<[Signature]>,
    globals: Arc<[ValueType]>,
    managed: Arc<[ValueType]>,
    traits: Arc<[TraitDescription]>,
    implementations: Arc<[Implementation]>,
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
    /// The source variables of the routine.
    pub fn debug_locals(&self) -> &[DebugLocal] {
        &self.locals
    }

    /// The registers that hold a value just before this instruction runs.
    pub fn initialized_at(&self, instruction: InstructionId) -> Vec<Register> {
        let pc = instruction.index();
        let Some(entry) = self.storage_entry(pc) else {
            return Vec::new();
        };
        let mut state = vec![StorageState::Dead; self.registers.len()];
        for &(index, value) in &entry.live {
            state[index] = value;
        }
        for index in entry.start..pc {
            self.update_storage(self.instructions[index], &mut state);
        }
        state
            .iter()
            .enumerate()
            .filter(|(_, state)| **state == StorageState::Initialized)
            .filter_map(|(index, _)| u32::try_from(index).ok().map(Register))
            .collect()
    }

    pub fn registers(&self) -> &[RegisterType] {
        &self.registers
    }

    pub fn parameters(&self) -> &[Register] {
        &self.parameters
    }

    pub fn result(&self) -> Option<ValueType> {
        self.result
    }

    /// The module's shape table, indexed by `ShapeId`.
    pub fn shape_table(&self) -> &[Shape] {
        &self.shapes
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
            RegisterType::Object(id) => Ok(ValueType::Object(id)),
            RegisterType::Path => Ok(ValueType::Path),
            RegisterType::Location(managed) => Ok(ValueType::Location(managed)),
            RegisterType::Allocation => Ok(ValueType::Allocation),
            RegisterType::Generator(signature) => Ok(ValueType::Generator(signature)),
            RegisterType::Sender(managed) => Ok(ValueType::Sender(managed)),
            RegisterType::Receiver(managed) => Ok(ValueType::Receiver(managed)),
            RegisterType::Task(managed) => Ok(ValueType::Task(managed)),
            RegisterType::Clock => Ok(ValueType::Clock),
            RegisterType::OpenObject => Ok(ValueType::OpenObject),
            RegisterType::Dynamic => Ok(ValueType::Dynamic),
            RegisterType::Failure => Ok(ValueType::Failure),
            RegisterType::Any => Ok(ValueType::Any),
            RegisterType::Context => Ok(ValueType::Context),
            RegisterType::Registration => Ok(ValueType::Registration),
            RegisterType::ServiceKey => Ok(ValueType::ServiceKey),
            RegisterType::ServiceRef(id) => Ok(ValueType::ServiceRef(id)),
            RegisterType::NativeCallable => Ok(ValueType::NativeCallable),
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

    /// The variant that carries the parsed integer and the one that carries
    /// the error, of a result-shaped enum. The error has no payload data.
    fn parse_variants(&self, register: Register) -> Result<(u32, u32), InvalidBytecode> {
        let variants = self.enum_variants(register)?;
        let [first, second] = variants else {
            return Err(InvalidBytecode::ExpectedEnum(register));
        };
        let unit_enum = |ty: Option<ValueType>| {
            matches!(ty, Some(ValueType::Composite(shape))
                if matches!(self.shapes.get(shape.index()), Some(Shape::Enum(v)) if v.len() == 1 && v[0].is_none()))
        };
        if *first == Some(I64) && unit_enum(*second) {
            Ok((0, 1))
        } else if *second == Some(I64) && unit_enum(*first) {
            Ok((1, 0))
        } else {
            Err(InvalidBytecode::ExpectedEnum(register))
        }
    }

    fn contract(&self, id: ContractId) -> Result<&Contract, InvalidBytecode> {
        self.contracts
            .get(id.index())
            .ok_or(InvalidBytecode::UnknownContract(id))
    }

    fn text(&self, id: TextId) -> Result<&Arc<str>, InvalidBytecode> {
        self.texts
            .get(id.index())
            .ok_or(InvalidBytecode::UnknownText(id))
    }

    /// The variants of a result whose error is a failure: the one that carries
    /// the value, then the failure.
    fn failure_result(&self, register: Register) -> Result<(u32, u32), InvalidBytecode> {
        match self.enum_variants(register)? {
            [Some(value), Some(ValueType::Failure)] if *value != ValueType::Failure => Ok((0, 1)),
            [Some(ValueType::Failure), Some(value)] if *value != ValueType::Failure => Ok((1, 0)),
            _ => Err(InvalidBytecode::ExpectedEnum(register)),
        }
    }

    /// [`Self::failure_result`] for a verified register.
    fn failure_result_runtime(&self, register: Register) -> (u32, u32) {
        match self.failure_result(register) {
            Ok(variants) => variants,
            Err(_) => unreachable!("verified failure result"),
        }
    }

    /// The register holds a service context, or a loan of one.
    fn require_context(&self, register: Register) -> Result<(), InvalidBytecode> {
        if self.through(register)? == ValueType::Context {
            Ok(())
        } else {
            Err(InvalidBytecode::ExpectedHandle(register))
        }
    }

    fn require_key(&self, register: Register) -> Result<(), InvalidBytecode> {
        if self.through(register)? == ValueType::ServiceKey {
            Ok(())
        } else {
            Err(InvalidBytecode::ExpectedHandle(register))
        }
    }

    /// The register holds an object, or an erased value that may hold one.
    fn require_object(&self, register: Register) -> Result<(), InvalidBytecode> {
        match self.through(register)? {
            ValueType::OpenObject | ValueType::Dynamic => Ok(()),
            _ => Err(InvalidBytecode::ExpectedObject(register)),
        }
    }

    /// Whether a value of this type may be erased or stored in an object:
    /// nothing in it is a counted or channel owner, which an object could
    /// neither release nor move.
    fn erasable_type(&self, ty: ValueType) -> bool {
        match ty {
            ValueType::Rc(_)
            | ValueType::Weak(_)
            | ValueType::Generator(_)
            | ValueType::Sender(_)
            | ValueType::Receiver(_)
            | ValueType::Task(_) => false,
            ValueType::Composite(shape) => match self.shapes.get(shape.index()) {
                Some(Shape::Tuple(fields)) => fields.iter().all(|&field| self.erasable_type(field)),
                Some(Shape::Enum(variants)) => variants
                    .iter()
                    .flatten()
                    .all(|&payload| self.erasable_type(payload)),
                Some(
                    Shape::Array { element, .. } | Shape::Vec { element } | Shape::Cell { element },
                ) => self.erasable_type(*element),
                Some(Shape::Slice { .. }) | None => false,
            },
            _ => true,
        }
    }

    fn require_erasable(&self, register: Register) -> Result<(), InvalidBytecode> {
        let ty = self.value_type(register)?;
        if self.erasable_type(ty) {
            Ok(())
        } else {
            Err(InvalidBytecode::NotErasable(register))
        }
    }

    /// The type a task's handle (or a loan of one) says its result has.
    fn task_element(&self, register: Register) -> Result<ValueType, InvalidBytecode> {
        let ValueType::Task(managed) = self.through(register)? else {
            return Err(InvalidBytecode::ExpectedTask(register));
        };
        self.managed
            .get(managed.index())
            .copied()
            .ok_or(InvalidBytecode::UnknownManaged(managed))
    }

    /// The register holds a channel's sender and receiver of one element type.
    fn channel_pair(&self, register: Register) -> Result<(), InvalidBytecode> {
        let ValueType::Composite(shape) = self.value_type(register)? else {
            return Err(InvalidBytecode::ExpectedTuple(register));
        };
        match self.shape(shape)? {
            Shape::Tuple(ends)
                if matches!(
                    **ends,
                    [ValueType::Sender(sender), ValueType::Receiver(receiver)] if sender == receiver
                ) =>
            {
                Ok(())
            }
            _ => Err(InvalidBytecode::ExpectedChannelEnd(register)),
        }
    }

    /// Whether the register holds (or loans) a sender, and the element type.
    fn channel_end(&self, register: Register) -> Result<(bool, ValueType), InvalidBytecode> {
        let (is_sender, managed) = match self.through(register)? {
            ValueType::Sender(managed) => (true, managed),
            ValueType::Receiver(managed) => (false, managed),
            _ => return Err(InvalidBytecode::ExpectedChannelEnd(register)),
        };
        let element = self
            .managed
            .get(managed.index())
            .copied()
            .ok_or(InvalidBytecode::UnknownManaged(managed))?;
        Ok((is_sender, element))
    }

    fn is_unit(&self, ty: Option<ValueType>) -> bool {
        matches!(ty, Some(ValueType::Composite(shape))
            if matches!(self.shapes.get(shape.index()), Some(Shape::Tuple(f)) if f.is_empty()))
    }

    /// The result of a send: unit, or a send error that carries the element.
    fn send_result(
        &self,
        register: Register,
        element: ValueType,
        v: ChannelVariants,
    ) -> Result<(), InvalidBytecode> {
        let bad = || InvalidBytecode::ExpectedEnum(register);
        let variants = self.enum_variants(register)?;
        let (ok, err) = (v.ok() as usize, v.err() as usize);
        if variants.len() != 2 || ok > 1 || err > 1 || ok == err || !self.is_unit(variants[ok]) {
            return Err(bad());
        }
        let Some(ValueType::Composite(failure)) = variants[err] else {
            return Err(bad());
        };
        let Shape::Enum(kinds) = self.shape(failure)? else {
            return Err(bad());
        };
        let distinct = |a: u32, b: u32| a != b;
        let in_range = (0..3).all(|i| (v.failure(i) as usize) < kinds.len());
        if kinds.len() != 3
            || !in_range
            || !(distinct(v.failure(0), v.failure(1))
                && distinct(v.failure(0), v.failure(2))
                && distinct(v.failure(1), v.failure(2)))
            || !kinds.iter().all(|payload| *payload == Some(element))
        {
            return Err(bad());
        }
        Ok(())
    }

    /// The result of a receive: the element, or a receive error with two unit
    /// variants.
    fn receive_result(
        &self,
        register: Register,
        element: ValueType,
        v: ChannelVariants,
    ) -> Result<(), InvalidBytecode> {
        let bad = || InvalidBytecode::ExpectedEnum(register);
        let variants = self.enum_variants(register)?;
        let (ok, err) = (v.ok() as usize, v.err() as usize);
        if variants.len() != 2 || ok > 1 || err > 1 || ok == err || variants[ok] != Some(element) {
            return Err(bad());
        }
        let Some(ValueType::Composite(failure)) = variants[err] else {
            return Err(bad());
        };
        match self.shape(failure)? {
            Shape::Enum(kinds)
                if kinds.len() == 2
                    && kinds.iter().all(Option::is_none)
                    && (v.failure(0) as usize) < 2
                    && (v.failure(1) as usize) < 2
                    && v.failure(0) != v.failure(1) =>
            {
                Ok(())
            }
            _ => Err(bad()),
        }
    }

    /// The description of the generator a register holds, or an exclusive loan
    /// of one holds.
    fn generator_description(&self, register: Register) -> Result<&Signature, InvalidBytecode> {
        if let RegisterType::Loan { access, .. } = self.register_kind(register)?
            && access != LoanAccess::Exclusive
        {
            return Err(InvalidBytecode::ExpectedExclusiveLoan(register));
        }
        let ValueType::Generator(id) = self.through(register)? else {
            return Err(InvalidBytecode::ExpectedGenerator(register));
        };
        let description = self.signature(id)?;
        if description.parameters.len() != 2 || description.result.is_none() {
            return Err(InvalidBytecode::UnknownSignature(id));
        }
        Ok(description)
    }

    /// The type is the empty tuple, the resume value of `next` and `resume()`.
    fn require_unit(&self, ty: ValueType, register: Register) -> Result<(), InvalidBytecode> {
        match ty {
            ValueType::Composite(shape) if matches!(self.shapes.get(shape.index()), Some(Shape::Tuple(f)) if f.is_empty()) => {
                Ok(())
            }
            _ => Err(InvalidBytecode::ExpectedGenerator(register)),
        }
    }

    /// The step a generator produces: its yielded value in the first variant,
    /// its returned value in the second.
    fn step_variants(
        &self,
        register: Register,
        description: &Signature,
    ) -> Result<(), InvalidBytecode> {
        match self.enum_variants(register)? {
            [Some(yielded), Some(returned)]
                if *yielded == description.parameters[0]
                    && Some(*returned) == description.result =>
            {
                Ok(())
            }
            _ => Err(InvalidBytecode::ExpectedEnum(register)),
        }
    }

    /// Whether a constant has the shape of a type.
    fn frozen_matches(&self, ty: ValueType, frozen: &Frozen) -> bool {
        match (ty, frozen) {
            (ValueType::Scalar(expected), Frozen::Scalar(scalar)) => scalar.ty() == expected,
            (ValueType::Text, Frozen::Text(_)) => true,
            (ValueType::Composite(shape), Frozen::Tuple(items)) => {
                matches!(self.shapes.get(shape.index()), Some(Shape::Tuple(fields))
                    if fields.len() == items.len()
                        && fields.iter().zip(items.iter()).all(|(&field, item)| self.frozen_matches(field, item)))
            }
            _ => false,
        }
    }

    /// The register holds a size and an alignment.
    fn require_layout(&self, register: Register) -> Result<(), InvalidBytecode> {
        let count = ValueType::Scalar(ScalarType::U64);
        let ValueType::Composite(shape) = self.value_type(register)? else {
            return Err(InvalidBytecode::ExpectedTuple(register));
        };
        match self.shape(shape)? {
            Shape::Tuple(fields) if **fields == [count, count] => Ok(()),
            _ => Err(InvalidBytecode::ExpectedTuple(register)),
        }
    }

    /// The variants of an allocation result: one carries the allocation and the
    /// other is a payload-free failure.
    fn allocation_result(&self, register: Register) -> Result<(u32, u32), InvalidBytecode> {
        let unit_enum = |ty: Option<ValueType>| {
            matches!(ty, Some(ValueType::Composite(shape))
                if matches!(self.shapes.get(shape.index()), Some(Shape::Enum(v)) if v.len() == 1 && v[0].is_none()))
        };
        match self.enum_variants(register)? {
            [Some(ValueType::Allocation), failure] if unit_enum(*failure) => Ok((0, 1)),
            [failure, Some(ValueType::Allocation)] if unit_enum(*failure) => Ok((1, 0)),
            _ => Err(InvalidBytecode::ExpectedEnum(register)),
        }
    }

    /// [`Self::allocation_result`] for a verified register.
    fn allocation_result_runtime(&self, register: Register) -> (u32, u32) {
        match self.allocation_result(register) {
            Ok(variants) => variants,
            Err(_) => unreachable!("verified allocation result"),
        }
    }

    /// The type of the value a register holds, or the pointee of a loan.
    fn through(&self, register: Register) -> Result<ValueType, InvalidBytecode> {
        match self.register_kind(register)? {
            RegisterType::Loan { pointee, .. } => Ok(pointee),
            _ => self.value_type(register),
        }
    }

    /// The variant that carries a value and the empty one, of an option-shaped
    /// enum.
    fn option_enum(&self, register: Register) -> Result<(u32, u32), InvalidBytecode> {
        match self.enum_variants(register)? {
            [Some(_), None] => Ok((0, 1)),
            [None, Some(_)] => Ok((1, 0)),
            _ => Err(InvalidBytecode::ExpectedEnum(register)),
        }
    }

    /// The type of the value an option-shaped enum carries.
    fn option_payload_runtime(&self, register: Register) -> ValueType {
        match self.enum_variants(register) {
            Ok([Some(payload), None] | [None, Some(payload)]) => *payload,
            _ => unreachable!("verified option register"),
        }
    }

    /// [`Self::parse_variants`] for a verified register.
    fn parse_variants_runtime(&self, register: Register) -> (u32, u32) {
        match self.parse_variants(register) {
            Ok(variants) => variants,
            Err(_) => unreachable!("verified result register"),
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

    /// Whether a verified register holds a vector.
    fn is_vec(&self, register: Register) -> bool {
        matches!(
            self.register_kind(register),
            Ok(RegisterType::Composite(shape))
                if matches!(self.shapes.get(shape.index()), Some(Shape::Vec { .. }))
        )
    }

    /// Whether a verified projection starts from a vector.
    fn projects_from_vec(&self, source: Register) -> bool {
        matches!(
            self.projection_base(source),
            Ok((ValueType::Composite(shape), ..))
                if matches!(self.shapes.get(shape.index()), Some(Shape::Vec { .. }))
        )
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

    /// The element type of an array or slice held by the register or behind
    /// its loan, and whether it is a slice. An array viewed as a slice must
    /// be one-dimensional and start at zero.
    fn sequence(&self, register: Register) -> Result<(ValueType, bool), InvalidBytecode> {
        let (container, ..) = self.projection_base(register)?;
        let ValueType::Composite(shape) = container else {
            return Err(InvalidBytecode::ExpectedArray(register));
        };
        match self.shape(shape)? {
            Shape::Array { element, bounds } if matches!(bounds.as_ref(), [(0, _)]) => {
                Ok((*element, false))
            }
            Shape::Vec { element } => Ok((*element, false)),
            Shape::Slice { element } => Ok((*element, true)),
            _ => Err(InvalidBytecode::SliceSource(register)),
        }
    }

    /// Whether a verified projection starts from a slice.
    fn projects_from_slice(&self, source: Register) -> bool {
        matches!(self.sequence(source), Ok((_, true)))
    }

    /// The element type of an array or vector, and the bounds an array's
    /// indices must lie in. A vector takes one zero-based index.
    fn indexed(&self, register: Register) -> Result<(ValueType, Vec<(i64, i64)>), InvalidBytecode> {
        let RegisterType::Composite(id) = self.register_kind(register)? else {
            return Err(InvalidBytecode::ExpectedArray(register));
        };
        match self.shape(id)? {
            Shape::Array { element, bounds } => Ok((*element, bounds.to_vec())),
            Shape::Vec { element } => Ok((*element, vec![(0, 0)])),
            _ => Err(InvalidBytecode::ExpectedArray(register)),
        }
    }

    /// The element type of the vector an owner holds or a loan points at.
    fn vec_element(&self, register: Register) -> Result<ValueType, InvalidBytecode> {
        let (container, ..) = self.projection_base(register)?;
        let ValueType::Composite(shape) = container else {
            return Err(InvalidBytecode::ExpectedVec(register));
        };
        match self.shape(shape)? {
            Shape::Vec { element } => Ok(*element),
            _ => Err(InvalidBytecode::ExpectedVec(register)),
        }
    }

    /// The element type of the cell an owner holds or a loan points at.
    fn cell_element(&self, register: Register) -> Result<ValueType, InvalidBytecode> {
        let (container, ..) = self.projection_base(register)?;
        let ValueType::Composite(shape) = container else {
            return Err(InvalidBytecode::ExpectedCell(register));
        };
        match self.shape(shape)? {
            Shape::Cell { element } => Ok(*element),
            _ => Err(InvalidBytecode::ExpectedCell(register)),
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
    /// An index is an integer or a count.
    fn require_index(&self, register: Register) -> Result<(), InvalidBytecode> {
        match self.register_type(register)? {
            ScalarType::I64 | ScalarType::U64 => Ok(()),
            actual => Err(InvalidBytecode::RegisterType {
                register,
                expected: ScalarType::I64,
                actual,
            }),
        }
    }

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
            .try_for_each(|&index| self.require_index(index))
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
    /// The checked program this was lowered from, which a run-time load
    /// extends.
    source: Option<Arc<Program>>,
    /// The checked routine each verified routine was lowered from.
    routine_ids: Arc<HashMap<usize, FunctionId>>,
    /// Checked routines lowered although nothing reachable calls them: the
    /// builders of loaded modules, in load order.
    roots: Arc<[usize]>,
    entry: FunctionId,
    functions: Arc<[VerifiedFunction]>,
    shapes: Arc<[Shape]>,
    globals: Arc<[ValueType]>,
    implementations: Arc<[Implementation]>,
    host_bindings: Arc<[ScalarHostBinding]>,
}

impl VerifiedModule {
    /// Whether code lowered from an extended program can replace this one under
    /// frames that are already running: every routine this module has is
    /// identical in it, and every table keeps its entries and their numbers.
    pub(crate) fn is_extended_by(&self, next: &Self) -> Result<(), String> {
        self.is_extended_by_with(next, false)
    }

    /// [`Self::is_extended_by`], where the entry routine may differ when no
    /// frame of it is running: a session appends statements to it.
    pub(crate) fn is_extended_by_with(
        &self,
        next: &Self,
        entry_may_change: bool,
    ) -> Result<(), String> {
        let (Some(old), Some(new)) = (self.functions.first(), next.functions.first()) else {
            return Err("there is no routine to compare".into());
        };
        if self.entry != next.entry {
            return Err("the entry routine moved".into());
        }
        if self.functions.len() > next.functions.len() {
            return Err("routines were removed".into());
        }
        for (name, same) in [
            ("shapes", is_prefix(&self.shapes, &next.shapes)),
            ("module variables", is_prefix(&self.globals, &next.globals)),
            (
                "implementations",
                is_prefix(&self.implementations, &next.implementations),
            ),
            ("signatures", is_prefix(&old.signatures, &new.signatures)),
            ("managed types", is_prefix(&old.managed, &new.managed)),
            ("traits", is_prefix(&old.traits, &new.traits)),
        ] {
            if !same {
                return Err(format!("the table of {name} changed"));
            }
        }
        for (index, (a, b)) in self.functions.iter().zip(next.functions.iter()).enumerate() {
            if entry_may_change && index == self.entry.index() {
                continue;
            }
            for (name, same) in [
                ("instructions", a.instructions == b.instructions),
                // New module variables of the entry routine add unused registers.
                (
                    "registers",
                    is_prefix(&a.registers, &b.registers)
                        && b.registers[a.registers.len()..]
                            .iter()
                            .all(|kind| *kind == RegisterType::Unused),
                ),
                ("parameters", a.parameters == b.parameters),
                ("result", a.result == b.result),
                ("generator", a.generator == b.generator),
                ("calls", a.calls == b.calls),
                ("lists", a.lists == b.lists),
                ("texts", a.texts == b.texts),
                ("contracts", a.contracts == b.contracts),
                ("outputs", a.outputs == b.outputs),
            ] {
                if !same {
                    return Err(format!("the {name} of routine {index} changed"));
                }
            }
        }
        Ok(())
    }

    /// [`Self::lower`] for a program that run-time loads may extend.
    pub fn lower_shared(program: Arc<Program>, routine: usize) -> Result<Self, LowerError> {
        Self::lower_extended(program, routine, &[])
    }

    /// Lower the program with these checked routines as extra roots, after
    /// everything the entry reaches so that its routines keep their numbers.
    pub(crate) fn lower_extended(
        program: Arc<Program>,
        routine: usize,
        roots: &[usize],
    ) -> Result<Self, LowerError> {
        let mut module = Self::lower_checked_roots(
            program.functions(),
            routine,
            Some(program.services.clone()),
            roots,
        )?;
        module.source = Some(program);
        Ok(module)
    }

    /// The last instruction of the entry routine, its return.
    pub(crate) fn entry_end(&self) -> InstructionId {
        let count = self
            .functions
            .get(self.entry.index())
            .map_or(0, |entry| entry.instructions.len());
        InstructionId(u32::try_from(count.saturating_sub(1)).unwrap_or(0))
    }

    /// The first instruction of a statement of the entry routine that starts at
    /// or after this source offset, when there is one.
    pub(crate) fn entry_statement_from(&self, offset: u32) -> Option<InstructionId> {
        let entry = self.functions.get(self.entry.index())?;
        entry
            .statements
            .iter()
            .filter(|statement| statement.span.start >= offset)
            .map(|statement| statement.instruction)
            .min()
    }

    /// The checked routine a verified routine was lowered from.
    pub fn routine_of(&self, function: FunctionId) -> Option<usize> {
        self.routine_ids
            .iter()
            .find(|&(_, &lowered)| lowered == function)
            .map(|(&routine, _)| routine)
    }

    /// The verified routine a checked routine was lowered to.
    pub(crate) fn lowered(&self, routine: usize) -> Option<FunctionId> {
        self.routine_ids.get(&routine).copied()
    }

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
    ExpectedGenerator(Register),
    ExpectedChannelEnd(Register),
    UnknownFrozen(FrozenId),
    ExpectedObject(Register),
    ExpectedDynamic(Register),
    NotErasable(Register),
    UnknownContract(ContractId),
    ExpectedTask(Register),
    YieldOutsideGenerator,
    InvalidInputKind {
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
    /// A slice exists only behind a loan.
    UnsizedValue(Register),
    ExpectedVec(Register),
    ExpectedCell(Register),
    SliceSource(Register),
    ExpectedText(Register),
    UnknownText(TextId),
    UnknownGlobal(GlobalId),
    UnknownTrait(TraitId),
    UnknownImplementation(ImplementationId),
    UnknownMethod {
        object: Register,
        method: u32,
    },
    /// A routine does not take the receiver and parameters its trait declares.
    ImplementationMismatch(ImplementationId),
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
                    ValueType::Function(signature) | ValueType::Generator(signature)
                        if signature.index() >= self.signatures.len() =>
                    {
                        return Err(image_error(InvalidBytecode::UnknownSignature(signature)));
                    }
                    ValueType::Gc(managed)
                    | ValueType::Rc(managed)
                    | ValueType::Weak(managed)
                    | ValueType::Location(managed)
                    | ValueType::Sender(managed)
                    | ValueType::Receiver(managed)
                    | ValueType::Task(managed)
                        if managed.index() >= self.managed.len() =>
                    {
                        return Err(image_error(InvalidBytecode::UnknownManaged(managed)));
                    }
                    ValueType::Composite(id) if id.index() >= self.shapes.len() => {
                        return Err(image_error(InvalidBytecode::UnknownShape(id)));
                    }
                    ValueType::Object(id) if id.index() >= self.traits.len() => {
                        return Err(image_error(InvalidBytecode::UnknownTrait(id)));
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
        let traits: Arc<[TraitDescription]> = self.traits.into();
        let implementations: Arc<[Implementation]> = self.implementations.into();
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
                    ValueType::Function(id) | ValueType::Generator(id)
                        if id.index() >= signatures.len() =>
                    {
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
                traits: traits.clone(),
                implementations: implementations.clone(),
                locals: image.locals,
                frozen: image.frozen,
                contracts: image.contracts,
                generator: image.generator,
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
        for (index, implementation) in implementations.iter().enumerate() {
            let id = ImplementationId(
                u32::try_from(index)
                    .map_err(|_| image_error(InvalidBytecode::FunctionIdOverflow))?,
            );
            let mismatch = || image_error(InvalidBytecode::ImplementationMismatch(id));
            let description = traits.get(implementation.trait_id.index()).ok_or_else(|| {
                image_error(InvalidBytecode::UnknownTrait(implementation.trait_id))
            })?;
            if implementation.methods.len() != description.methods.len() {
                return Err(mismatch());
            }
            for (&target, method) in implementation
                .methods
                .iter()
                .zip(description.methods.iter())
            {
                let callee = functions
                    .get(target.index())
                    .ok_or_else(|| image_error(InvalidBytecode::UnknownFunction(target)))?;
                let Some((&receiver, rest)) = callee.parameters.split_first() else {
                    return Err(mismatch());
                };
                let receiver_ok = if method.by_value {
                    callee.value_type(receiver) == Ok(implementation.receiver)
                        && callee.erasable_type(implementation.receiver)
                } else {
                    matches!(
                        callee.registers.get(receiver.index()),
                        Some(RegisterType::Loan { access, origin: LoanOrigin::Parameter { .. }, pointee })
                            if *access == method.receiver && *pointee == implementation.receiver
                    )
                };
                let parameters_ok =
                    rest.len() == method.parameters.len()
                        && rest.iter().zip(method.parameters.iter()).all(
                            |(&parameter, &expected)| callee.value_type(parameter) == Ok(expected),
                        );
                if !receiver_ok || !parameters_ok || callee.result != method.result {
                    return Err(mismatch());
                }
            }
        }
        for index in 0..functions.len() {
            let id = function_id(index).map_err(image_error)?;
            let (frame, storage_entries) =
                functions[index].verify(id, &functions, &host_bindings)?;
            functions[index].frame = frame;
            functions[index].storage_entries = storage_entries;
        }
        Ok(VerifiedModule {
            source: None,
            routine_ids: Arc::new(HashMap::new()),
            roots: Arc::from([]),
            entry: self.entry,
            shapes,
            globals,
            implementations,
            functions: functions.into(),
            host_bindings: host_bindings.into(),
        })
    }
}

fn shape_components(shape: &Shape) -> Vec<ValueType> {
    match shape {
        Shape::Tuple(fields) => fields.to_vec(),
        Shape::Enum(variants) => variants.iter().copied().flatten().collect(),
        Shape::Array { element, .. }
        | Shape::Slice { element }
        | Shape::Vec { element }
        | Shape::Cell { element } => vec![*element],
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
        for (index, &kind) in self.registers.iter().enumerate() {
            if let RegisterType::Composite(shape) = kind {
                let shape_data = self.shape(shape).map_err(|kind| error(None, kind))?;
                if matches!(shape_data, Shape::Slice { .. }) {
                    let register = Register(u32::try_from(index).unwrap_or(u32::MAX));
                    return Err(error(None, InvalidBytecode::UnsizedValue(register)));
                }
            }
            if let RegisterType::Function(signature) | RegisterType::Generator(signature) = kind {
                self.signature(signature)
                    .map_err(|kind| error(None, kind))?;
            }
            if let RegisterType::Object(id) = kind
                && self.traits.get(id.index()).is_none()
            {
                return Err(error(None, InvalidBytecode::UnknownTrait(id)));
            }
            if let RegisterType::Gc(managed)
            | RegisterType::Rc(managed)
            | RegisterType::Weak(managed)
            | RegisterType::Location(managed)
            | RegisterType::Sender(managed)
            | RegisterType::Receiver(managed)
            | RegisterType::Task(managed) = kind
                && self.managed.get(managed.index()).is_none()
            {
                return Err(error(None, InvalidBytecode::UnknownManaged(managed)));
            }
        }
        if let Some(id) = self.generator {
            let description = self.signature(id).map_err(|kind| error(None, kind))?;
            if description.parameters.len() != 2 || description.result != self.result {
                return Err(error(None, InvalidBytecode::ResultPresence));
            }
            // A suspended frame outlives the call that made it, so it cannot
            // hold a loan its caller lent.
            if let Some(&register) = self
                .parameters
                .iter()
                .find(|register| self.registers[register.index()].is_loan())
            {
                return Err(error(None, InvalidBytecode::InvalidLoanParameter(register)));
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
                | RegisterType::Weak(_)
                | RegisterType::Path
                | RegisterType::Location(_)
                | RegisterType::Allocation
                | RegisterType::Sender(_)
                | RegisterType::Receiver(_)
                | RegisterType::Task(_)
                | RegisterType::Clock
                | RegisterType::OpenObject
                | RegisterType::Dynamic
                | RegisterType::Failure
                | RegisterType::Any
                | RegisterType::Object(_)
                | RegisterType::Generator(_)
                | RegisterType::Context
                | RegisterType::Registration
                | RegisterType::ServiceKey
                | RegisterType::ServiceRef(_)
                | RegisterType::NativeCallable => {}
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
                if callee.generator.is_some() {
                    return Err(InvalidBytecode::ClosureTarget(id));
                }
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
            CallTarget::Method { object, method } => {
                let (RegisterType::Object(trait_id) | RegisterType::ServiceRef(trait_id)) =
                    self.register_kind(object)?
                else {
                    return Err(InvalidBytecode::ExpectedHandle(object));
                };
                let description = self
                    .traits
                    .get(trait_id.index())
                    .ok_or(InvalidBytecode::UnknownTrait(trait_id))?;
                let declared = description
                    .methods
                    .get(method as usize)
                    .ok_or(InvalidBytecode::UnknownMethod { object, method })?;
                self.verify_arguments(
                    &call.arguments,
                    declared.parameters.iter().map(|&ty| Ok(ty)),
                )?;
                declared.result
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
            CallTarget::Mapped(callee) => {
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
                if signature.result != Some(ValueType::Path) || call.arguments.is_empty() {
                    return Err(InvalidBytecode::ResultPresence);
                }
                let Some(destination) = call.destination else {
                    return Err(InvalidBytecode::ResultPresence);
                };
                self.option_enum(destination)?;
                return Ok(());
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
                    if !matches!(
                        self.value_type(argument)?,
                        ValueType::Text | ValueType::Dynamic
                    ) {
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
                RegisterType::Object(id) => self.require_value(arg, ValueType::Object(id))?,
                RegisterType::Path => self.require_value(arg, ValueType::Path)?,
                RegisterType::Location(managed) => {
                    self.require_value(arg, ValueType::Location(managed))?
                }
                RegisterType::Allocation => self.require_value(arg, ValueType::Allocation)?,
                RegisterType::Generator(signature) => {
                    self.require_value(arg, ValueType::Generator(signature))?
                }
                RegisterType::Sender(managed) => {
                    self.require_value(arg, ValueType::Sender(managed))?
                }
                RegisterType::Receiver(managed) => {
                    self.require_value(arg, ValueType::Receiver(managed))?
                }
                RegisterType::Task(managed) => self.require_value(arg, ValueType::Task(managed))?,
                RegisterType::Clock => self.require_value(arg, ValueType::Clock)?,
                RegisterType::OpenObject => self.require_value(arg, ValueType::OpenObject)?,
                RegisterType::Dynamic => self.require_value(arg, ValueType::Dynamic)?,
                RegisterType::Failure => self.require_value(arg, ValueType::Failure)?,
                RegisterType::Any => self.require_value(arg, ValueType::Any)?,
                RegisterType::Context => self.require_value(arg, ValueType::Context)?,
                RegisterType::Registration => self.require_value(arg, ValueType::Registration)?,
                RegisterType::ServiceKey => self.require_value(arg, ValueType::ServiceKey)?,
                RegisterType::ServiceRef(id) => {
                    self.require_value(arg, ValueType::ServiceRef(id))?
                }
                RegisterType::NativeCallable => {
                    self.require_value(arg, ValueType::NativeCallable)?
                }
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
                match self.shape(shape)? {
                    Shape::Array { element, bounds } => {
                        self.require_indices(src, bounds, list)?;
                        self.require_projection(dst, src, *element)
                    }
                    // A slice or vector is indexed from zero by one integer.
                    Shape::Slice { element } | Shape::Vec { element } => {
                        self.require_indices(src, &[(0, 0)], list)?;
                        self.require_projection(dst, src, *element)
                    }
                    _ => Err(InvalidBytecode::ExpectedArray(src)),
                }
            }
            Instruction::Clone(dst, src) => {
                let (held, ..) = self.projection_base(src)?;
                self.require_value(dst, held)
            }
            Instruction::TextMap(_, dst, src) => {
                self.require_text(src)?;
                self.require_text(dst)
            }
            Instruction::TextTest(_, dst, text, other) => {
                self.require_text(text)?;
                self.require_text(other)?;
                self.require_type(dst, ScalarType::Bool)
            }
            Instruction::TextReplace(dst, text, list) => {
                self.require_text(text)?;
                let [from, to] = self.operand_list(list)? else {
                    return Err(InvalidBytecode::ArgumentCount {
                        expected: 2,
                        actual: self.operand_list(list)?.len(),
                    });
                };
                self.require_text(*from)?;
                self.require_text(*to)?;
                self.require_text(dst)
            }
            Instruction::ToText(dst, src) => {
                if self.register_kind(src)? != RegisterType::Text {
                    self.register_type(src)?;
                }
                self.require_text(dst)
            }
            Instruction::ObjectNew(dst, list, contract) => {
                if self.value_type(dst)? != ValueType::Dynamic {
                    self.require_value(dst, ValueType::OpenObject)?;
                }
                let fields = &self.contract(contract)?.fields;
                let values = self.operand_list(list)?;
                if fields.len() != values.len() {
                    return Err(InvalidBytecode::ArgumentCount {
                        expected: fields.len(),
                        actual: values.len(),
                    });
                }
                for (&value, (_, ty)) in values.iter().zip(fields.iter()) {
                    self.require_erasable(value)?;
                    self.require_value(value, *ty)?;
                }
                Ok(())
            }
            Instruction::ObjectGet(dst, object, name) => {
                self.require_object(object)?;
                self.text(name)?;
                self.require_erasable(dst)
            }
            Instruction::ObjectGetKey(dst, object, key) => {
                self.require_object(object)?;
                self.require_text(key)?;
                self.require_erasable(dst)
            }
            Instruction::ObjectSet(object, name, value) => {
                self.require_object(object)?;
                self.text(name)?;
                self.require_erasable(value)
            }
            Instruction::ObjectSetKey(object, list) => {
                self.require_object(object)?;
                let &[key, value] = self.operand_list(list)? else {
                    return Err(InvalidBytecode::ArgumentCount {
                        expected: 2,
                        actual: self.operand_list(list)?.len(),
                    });
                };
                self.require_text(key)?;
                self.require_erasable(value)
            }
            Instruction::ObjectDelete(dst, object, name) => {
                self.require_object(object)?;
                self.text(name)?;
                self.require_type(dst, ScalarType::Bool)
            }
            Instruction::ObjectDeleteKey(dst, object, key) => {
                self.require_object(object)?;
                self.require_text(key)?;
                self.require_type(dst, ScalarType::Bool)
            }
            Instruction::ObjectView(dst, source, contract) => {
                self.require_value(dst, ValueType::OpenObject)?;
                self.require_object(source)?;
                for (_, ty) in self.contract(contract)?.fields.iter() {
                    if !self.erasable_type(*ty) {
                        return Err(InvalidBytecode::NotErasable(source));
                    }
                }
                Ok(())
            }
            Instruction::ObjectFreeze(object) => self.require_object(object),
            Instruction::ObjectTryView(dst, source, contract) => {
                self.require_object(source)?;
                for (_, ty) in self.contract(contract)?.fields.iter() {
                    if !self.erasable_type(*ty) {
                        return Err(InvalidBytecode::NotErasable(source));
                    }
                }
                self.failure_result(dst).map(|_| ())
            }
            Instruction::ModuleLoad(dst, path) => {
                self.require_text(path)?;
                self.failure_result(dst).map(|_| ())
            }
            Instruction::FailureFlag(dst, failure, which) => {
                self.require_value(failure, ValueType::Failure)?;
                if which > 2 {
                    return Err(InvalidBytecode::ExpectedDynamic(failure));
                }
                self.require_type(dst, ScalarType::Bool)
            }
            Instruction::FailureMessage(dst, failure) => {
                self.require_value(failure, ValueType::Failure)?;
                self.require_text(dst)
            }
            Instruction::DynamicInvoke(dst, callee, list) => {
                self.require_value(callee, ValueType::Dynamic)?;
                self.require_value(dst, ValueType::Dynamic)?;
                for &argument in self.operand_list(list)? {
                    self.require_erasable(argument)?;
                }
                Ok(())
            }
            Instruction::NativeCallableConst(dst, _) => {
                self.require_value(dst, ValueType::NativeCallable)
            }
            Instruction::ServiceContext(dst) => self.require_value(dst, ValueType::Context),
            Instruction::ServiceExtend(dst, ctx) => {
                self.require_context(ctx)?;
                self.require_value(dst, ValueType::Context)
            }
            Instruction::ServiceIsolate(dst, ctx, key) => {
                self.require_context(ctx)?;
                self.require_key(key)?;
                self.require_value(dst, ValueType::Context)
            }
            Instruction::ServiceKeyConst(dst, _) => self.require_value(dst, ValueType::ServiceKey),
            Instruction::ServiceReference(dst, ctx, key) => {
                self.require_context(ctx)?;
                self.require_key(key)?;
                match self.value_type(dst)? {
                    ValueType::ServiceRef(_) => Ok(()),
                    _ => Err(InvalidBytecode::ExpectedHandle(dst)),
                }
            }
            Instruction::ServiceProvide(dst, list) => {
                let &[ctx, key, value] = self.operand_list(list)? else {
                    return Err(InvalidBytecode::ArgumentCount {
                        expected: 3,
                        actual: self.operand_list(list)?.len(),
                    });
                };
                self.require_context(ctx)?;
                self.require_key(key)?;
                if !matches!(self.value_type(value)?, ValueType::Object(_)) {
                    return Err(InvalidBytecode::ExpectedHandle(value));
                }
                self.require_value(dst, ValueType::Registration)
            }
            Instruction::ServiceAvailable(dst, ctx, key) => {
                self.require_context(ctx)?;
                self.require_key(key)?;
                self.require_type(dst, ScalarType::Bool)
            }
            Instruction::ServiceInject(list) => {
                let &[ctx, key, callback] = self.operand_list(list)? else {
                    return Err(InvalidBytecode::ArgumentCount {
                        expected: 3,
                        actual: self.operand_list(list)?.len(),
                    });
                };
                self.require_context(ctx)?;
                self.require_key(key)?;
                match self.value_type(callback)? {
                    ValueType::Function(_) => Ok(()),
                    _ => Err(InvalidBytecode::ExpectedFunction(callback)),
                }
            }
            Instruction::ServiceOnDispose(ctx, callback) => {
                self.require_context(ctx)?;
                match self.value_type(callback)? {
                    ValueType::Function(_) => Ok(()),
                    _ => Err(InvalidBytecode::ExpectedFunction(callback)),
                }
            }
            Instruction::ServiceDispose(target) => match self.through(target)? {
                ValueType::Context | ValueType::Registration => Ok(()),
                _ => Err(InvalidBytecode::ExpectedHandle(target)),
            },
            Instruction::Random(dst, bound) => {
                if !matches!(
                    self.register_type(bound)?,
                    ScalarType::I64 | ScalarType::F64
                ) {
                    return Err(InvalidBytecode::ExpectedScalar(bound));
                }
                self.require_type(dst, ScalarType::F64)
            }
            Instruction::Count(op, dst, lhs, rhs) => {
                self.require_type(lhs, ScalarType::U64)?;
                self.require_type(rhs, ScalarType::U64)?;
                let result = match op.result() {
                    ScalarType::I64 => ScalarType::U64,
                    other => other,
                };
                self.require_type(dst, result)
            }
            Instruction::FrozenValue(dst, id) => {
                let frozen = self
                    .frozen
                    .get(id.index())
                    .ok_or(InvalidBytecode::UnknownFrozen(id))?;
                if self.frozen_matches(self.value_type(dst)?, frozen) {
                    Ok(())
                } else {
                    Err(InvalidBytecode::ExpectedTuple(dst))
                }
            }
            Instruction::AnyErase(dst, source) => {
                self.require_value(dst, ValueType::Any)?;
                self.value_type(source).map(|_| ())
            }
            Instruction::AnyChecked(dst, source) => {
                self.require_value(source, ValueType::Any)?;
                self.value_type(dst).map(|_| ())
            }
            Instruction::Erase(dst, source) => {
                self.require_value(dst, ValueType::Dynamic)?;
                self.require_erasable(source)
            }
            Instruction::DynamicCast(dst, source) => {
                self.require_value(source, ValueType::Dynamic)?;
                self.require_erasable(dst)
            }
            Instruction::DynamicBinary(op, dst, lhs, rhs) => {
                if matches!(op, DynamicOp::Negate | DynamicOp::Not) {
                    return Err(InvalidBytecode::ExpectedDynamic(dst));
                }
                self.require_value(lhs, ValueType::Dynamic)?;
                self.require_value(rhs, ValueType::Dynamic)?;
                let comparison = matches!(
                    op,
                    DynamicOp::Equal
                        | DynamicOp::NotEqual
                        | DynamicOp::Less
                        | DynamicOp::Greater
                        | DynamicOp::LessEqual
                        | DynamicOp::GreaterEqual
                );
                if comparison && self.value_type(dst)? == ValueType::Scalar(ScalarType::Bool) {
                    return Ok(());
                }
                self.require_value(dst, ValueType::Dynamic)
            }
            Instruction::DynamicUnary(op, dst, source) => {
                if !matches!(op, DynamicOp::Negate | DynamicOp::Not) {
                    return Err(InvalidBytecode::ExpectedDynamic(dst));
                }
                self.require_value(source, ValueType::Dynamic)?;
                self.require_value(dst, ValueType::Dynamic)
            }
            Instruction::TaskSpawn(dst, closure) => {
                let RegisterType::Task(managed) = self.register_kind(dst)? else {
                    return Err(InvalidBytecode::ExpectedTask(dst));
                };
                let RegisterType::Function(id) = self.register_kind(closure)? else {
                    return Err(InvalidBytecode::ExpectedFunction(closure));
                };
                let signature = self.signature(id)?;
                let element = self
                    .managed
                    .get(managed.index())
                    .copied()
                    .ok_or(InvalidBytecode::UnknownManaged(managed))?;
                let returns_unit = signature.result.is_none() && self.is_unit(Some(element));
                if !signature.parameters.is_empty()
                    || !(returns_unit || signature.result == Some(element))
                {
                    return Err(InvalidBytecode::ClosureTarget(FunctionId(0)));
                }
                Ok(())
            }
            Instruction::TaskJoin(dst, task) => {
                let element = self.task_element(task)?;
                match self.value_type(dst)? {
                    ValueType::Composite(shape) => match self.shape(shape)? {
                        Shape::Enum(variants)
                            if variants.len() == 4
                                && variants[0] == Some(element)
                                && variants[1..].iter().all(Option::is_none) =>
                        {
                            Ok(())
                        }
                        _ => Err(InvalidBytecode::ExpectedEnum(dst)),
                    },
                    _ => Err(InvalidBytecode::ExpectedEnum(dst)),
                }
            }
            Instruction::TaskCancel(task) => self.task_element(task).map(|_| ()),
            Instruction::TaskIsFinished(dst, task) => {
                self.task_element(task)?;
                self.require_type(dst, ScalarType::Bool)
            }
            Instruction::ClockNew(dst) => self.require_value(dst, ValueType::Clock),
            Instruction::ClockClone(dst, clock) => {
                if self.through(clock)? != ValueType::Clock {
                    return Err(InvalidBytecode::ExpectedTask(clock));
                }
                self.require_value(dst, ValueType::Clock)
            }
            Instruction::ClockSleep(clock, duration) => {
                if self.through(clock)? != ValueType::Clock {
                    return Err(InvalidBytecode::ExpectedTask(clock));
                }
                self.require_type(duration, ScalarType::U64)
            }
            Instruction::ChannelBounded(dst, capacity) => {
                self.require_type(capacity, ScalarType::U64)?;
                self.channel_pair(dst)
            }
            Instruction::ChannelUnbounded(dst) => self.channel_pair(dst),
            Instruction::ChannelClone(dst, end) => {
                self.channel_end(end)?;
                self.require_value(dst, self.through(end)?)
            }
            Instruction::ChannelClose(end) => self.channel_end(end).map(|_| ()),
            Instruction::ChannelIsClosed(dst, end) => {
                self.channel_end(end)?;
                self.require_type(dst, ScalarType::Bool)
            }
            Instruction::ChannelSend(dst, list, variants)
            | Instruction::ChannelTrySend(dst, list, variants) => {
                let &[sender, value] = self.operand_list(list)? else {
                    return Err(InvalidBytecode::ArgumentCount {
                        expected: 2,
                        actual: self.operand_list(list)?.len(),
                    });
                };
                let (is_sender, element) = self.channel_end(sender)?;
                if !is_sender {
                    return Err(InvalidBytecode::ExpectedChannelEnd(sender));
                }
                self.require_value(value, element)?;
                self.send_result(dst, element, variants)
            }
            Instruction::ChannelReceive(dst, end, variants)
            | Instruction::ChannelTryReceive(dst, end, variants) => {
                let (is_sender, element) = self.channel_end(end)?;
                if is_sender {
                    return Err(InvalidBytecode::ExpectedChannelEnd(end));
                }
                self.receive_result(dst, element, variants)
            }
            Instruction::SendErrorValue(dst, error) => {
                let ValueType::Composite(shape) = self.through(error)? else {
                    return Err(InvalidBytecode::ExpectedEnum(error));
                };
                match self.shape(shape)? {
                    Shape::Enum(variants)
                        if !variants.is_empty()
                            && variants.iter().all(|payload| payload.is_some())
                            && variants.windows(2).all(|pair| pair[0] == pair[1]) =>
                    {
                        let payload = variants[0].unwrap_or(ValueType::Path);
                        self.require_value(dst, payload)
                    }
                    _ => Err(InvalidBytecode::ExpectedEnum(error)),
                }
            }
            Instruction::Yield(dst, value) => {
                let Some(id) = self.generator else {
                    return Err(InvalidBytecode::YieldOutsideGenerator);
                };
                let description = self.signature(id)?;
                let [yielded, resume] = *description.parameters else {
                    return Err(InvalidBytecode::UnknownSignature(id));
                };
                self.require_value(value, yielded)?;
                self.require_value(dst, resume)
            }
            Instruction::GeneratorStart(dst, generator)
            | Instruction::GeneratorNext(dst, generator)
            | Instruction::GeneratorResumeUnit(dst, generator) => {
                let description = self.generator_description(generator)?;
                self.step_variants(dst, description)?;
                if !matches!(inst, Instruction::GeneratorStart(..)) {
                    self.require_unit(description.parameters[1], generator)?;
                }
                Ok(())
            }
            Instruction::GeneratorResume(dst, generator, value) => {
                let description = self.generator_description(generator)?;
                self.step_variants(dst, description)?;
                self.require_value(value, description.parameters[1])
            }
            Instruction::GeneratorClose(generator) => {
                self.generator_description(generator).map(|_| ())
            }
            Instruction::LayoutForRc(dst) => self.require_layout(dst),
            Instruction::Allocate(dst, layout) => {
                self.allocation_result(dst)?;
                self.require_layout(layout)
            }
            Instruction::Deallocate(allocation) => {
                self.require_value(allocation, ValueType::Allocation)
            }
            Instruction::RcNewIn(dst, list, deallocate) => {
                let RegisterType::Rc(managed) = self.register_kind(dst)? else {
                    return Err(InvalidBytecode::ExpectedHandle(dst));
                };
                let &[allocator, allocated, value] = self.operand_list(list)? else {
                    return Err(InvalidBytecode::ArgumentCount {
                        expected: 3,
                        actual: self.operand_list(list)?.len(),
                    });
                };
                let declared = self
                    .managed
                    .get(managed.index())
                    .copied()
                    .ok_or(InvalidBytecode::UnknownManaged(managed))?;
                self.require_value(value, declared)?;
                self.allocation_result(allocated)?;
                let state = self.value_type(allocator)?;
                let callee = functions
                    .get(deallocate.index())
                    .ok_or(InvalidBytecode::UnknownFunction(deallocate))?;
                let &[receiver, block] = &*callee.parameters else {
                    return Err(InvalidBytecode::ArgumentCount {
                        expected: 2,
                        actual: callee.parameters.len(),
                    });
                };
                let receives_state = matches!(
                    callee.register_kind(receiver)?,
                    RegisterType::Loan { access: LoanAccess::Exclusive, pointee, .. } if pointee == state
                );
                if !receives_state
                    || callee.register_kind(block)? != RegisterType::Allocation
                    || callee.result.is_some()
                {
                    return Err(InvalidBytecode::ResultPresence);
                }
                Ok(())
            }
            Instruction::LocationField(dst, owner, field) => {
                let RegisterType::Location(location) = self.register_kind(dst)? else {
                    return Err(InvalidBytecode::ExpectedHandle(dst));
                };
                let held = self.through(owner)?;
                let ValueType::Gc(handle) = held else {
                    return Err(InvalidBytecode::ExpectedHandle(owner));
                };
                let record = self
                    .managed
                    .get(handle.index())
                    .copied()
                    .ok_or(InvalidBytecode::UnknownManaged(handle))?;
                let declared = self
                    .managed
                    .get(location.index())
                    .copied()
                    .ok_or(InvalidBytecode::UnknownManaged(location))?;
                let ValueType::Composite(shape) = record else {
                    return Err(InvalidBytecode::ExpectedTuple(owner));
                };
                match self.shape(shape)? {
                    Shape::Tuple(fields) if fields.get(field as usize) == Some(&declared) => Ok(()),
                    _ => Err(InvalidBytecode::ExpectedTuple(owner)),
                }
            }
            Instruction::LocationLoad(dst, location) => {
                let ValueType::Location(managed) = self.through(location)? else {
                    return Err(InvalidBytecode::ExpectedHandle(location));
                };
                let declared = self
                    .managed
                    .get(managed.index())
                    .copied()
                    .ok_or(InvalidBytecode::UnknownManaged(managed))?;
                self.require_value(dst, declared)
            }
            Instruction::LocationStore(location, value) => {
                let ValueType::Location(managed) = self.through(location)? else {
                    return Err(InvalidBytecode::ExpectedHandle(location));
                };
                let declared = self
                    .managed
                    .get(managed.index())
                    .copied()
                    .ok_or(InvalidBytecode::UnknownManaged(managed))?;
                self.require_value(value, declared)
            }
            Instruction::PathNew(dst, root) => {
                self.require_value(dst, ValueType::Path)?;
                self.value_type(root).map(|_| ())
            }
            Instruction::PathMissing(dst) => self.require_value(dst, ValueType::Path),
            Instruction::PathTarget(dst, src) | Instruction::PathField(dst, src, _) => {
                self.require_value(dst, ValueType::Path)?;
                self.require_value(src, ValueType::Path)
            }
            Instruction::PathElement(dst, src, index) => {
                self.require_value(dst, ValueType::Path)?;
                self.require_value(src, ValueType::Path)?;
                self.require_index(index)
            }
            Instruction::Input(dst, kind) => self.require_value(dst, kind.value_type()),
            Instruction::InputLoan(loan, kind) => {
                let (access, _, ty) = self.loan_type(loan)?;
                if access != LoanAccess::Exclusive {
                    return Err(InvalidBytecode::ExpectedExclusiveLoan(loan));
                }
                if ty != kind.value_type() {
                    return Err(InvalidBytecode::ValueType {
                        register: loan,
                        expected: kind.value_type(),
                        actual: ty,
                    });
                }
                Ok(())
            }
            Instruction::ParseInteger(dst, text) => {
                self.require_text(text)?;
                self.parse_variants(dst).map(|_| ())
            }
            Instruction::CountToInteger(dst, src) => {
                self.require_type(src, ScalarType::U64)?;
                self.require_type(dst, ScalarType::I64)
            }
            Instruction::IntegerToCount(dst, src) => {
                self.require_type(src, ScalarType::I64)?;
                self.require_type(dst, ScalarType::U64)
            }
            Instruction::Abs(dst, src) => {
                // Integers (checked) and floats.
                let ty = self.register_type(src)?;
                if !matches!(ty, ScalarType::I64 | ScalarType::F64) {
                    return Err(InvalidBytecode::ExpectedScalar(src));
                }
                self.require_type(dst, ty)
            }
            Instruction::Extremum(_, dst, lhs, rhs) => {
                self.require_type(lhs, ScalarType::I64)?;
                self.require_type(rhs, ScalarType::I64)?;
                self.require_type(dst, ScalarType::I64)
            }
            Instruction::SliceOf(dst, src) => {
                let (element, _) = self.sequence(src)?;
                let (_, _, pointee) = self.loan_type(dst)?;
                let ValueType::Composite(shape) = pointee else {
                    return Err(InvalidBytecode::SliceSource(dst));
                };
                match self.shape(shape)? {
                    Shape::Slice { element: viewed } if *viewed == element => {}
                    _ => return Err(InvalidBytecode::SliceSource(dst)),
                }
                self.require_projection(dst, src, pointee)
            }
            Instruction::SliceLength(dst, src) => {
                self.sequence(src)?;
                self.require_type(dst, ScalarType::I64)
            }
            Instruction::Sum(dst, src) => {
                let (element, _) = self.sequence(src)?;
                if element != ValueType::Scalar(ScalarType::I64) {
                    return Err(InvalidBytecode::SliceSource(src));
                }
                self.require_type(dst, ScalarType::I64)
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
            Instruction::TraitBox(dst, value, implementation) => {
                let RegisterType::Object(trait_id) = self.register_kind(dst)? else {
                    return Err(InvalidBytecode::ExpectedHandle(dst));
                };
                let declared = self
                    .implementations
                    .get(implementation.index())
                    .ok_or(InvalidBytecode::UnknownImplementation(implementation))?;
                if declared.trait_id != trait_id {
                    return Err(InvalidBytecode::ImplementationMismatch(implementation));
                }
                self.require_value(value, declared.receiver)
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
            Instruction::EnumExpect(src, variant) => {
                if self.variant_payload(src, variant).is_err() {
                    return Err(InvalidBytecode::UnknownVariant {
                        register: src,
                        variant,
                    });
                }
                Ok(())
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
                // Calling a closure of a generator body makes a generator.
                let made = match callee.generator {
                    Some(description) => {
                        if self.signature(description)?.result != callee.result {
                            return Err(InvalidBytecode::ClosureTarget(target));
                        }
                        Some(ValueType::Generator(description))
                    }
                    None => callee.result,
                };
                if callee.parameters.len() != captures.len() + declared || made != signature.result
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
                let (element, bounds) = self.indexed(array)?;
                self.require_indices(array, &bounds, list)?;
                self.require_value(dst, element)
            }
            Instruction::ArrayStore(array, list, value) => {
                let (element, bounds) = self.indexed(array)?;
                self.require_indices(array, &bounds, list)?;
                self.require_value(value, element)
            }
            Instruction::RangeNew(dst, start, end)
            | Instruction::RangeInclusive(dst, start, end) => {
                self.require_type(start, ScalarType::I64)?;
                self.require_type(end, ScalarType::I64)?;
                if self.tuple_fields(dst)? != [I64, I64] {
                    return Err(InvalidBytecode::ExpectedTuple(dst));
                }
                Ok(())
            }
            Instruction::VecFrom(dst, range) => {
                if self.vec_element(dst)? != I64 || self.tuple_fields(range)? != [I64, I64] {
                    return Err(InvalidBytecode::ExpectedVec(dst));
                }
                Ok(())
            }
            Instruction::VecNew(dst, list) => {
                let element = self.vec_element(dst)?;
                self.operand_list(list)?
                    .iter()
                    .try_for_each(|&item| self.require_value(item, element))
            }
            Instruction::VecPush(target, value) => {
                let element = self.vec_element(target)?;
                let (_, access, _) = self.projection_base(target)?;
                if self.register_kind(target)?.is_loan() && access != LoanAccess::Exclusive {
                    return Err(InvalidBytecode::ExpectedExclusiveLoan(target));
                }
                self.require_value(value, element)
            }
            Instruction::VecLength(dst, vector) => {
                self.vec_element(vector)?;
                self.require_type(dst, ScalarType::U64)
            }
            Instruction::CellNew(dst, value) => {
                let element = self.cell_element(dst)?;
                self.require_value(value, element)
            }
            Instruction::CellRead(dst, cell) => {
                let element = self.cell_element(cell)?;
                self.require_value(dst, element)
            }
            Instruction::CellWrite(cell, value) => {
                let element = self.cell_element(cell)?;
                self.require_value(value, element)
            }
            Instruction::CellReplace(dst, cell, value) => {
                let element = self.cell_element(cell)?;
                self.require_value(value, element)?;
                self.require_value(dst, element)
            }
            Instruction::ArrayLength(dst, array) => {
                let (_, bounds) = self.indexed(array)?;
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
