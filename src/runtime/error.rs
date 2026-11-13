//! Typed runtime failures. Every failure a Session can report is a variant here;
//! text exists only in `Display` and in the diagnostic produced by
//! [`RuntimeFailure::diagnostic`], which owns the code mapping.
use cyallo_ast::{Diagnostic, NumberError, Span};
use std::fmt;

/// A failed Session: the typed error and the source span of the failing instruction.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeFailure {
    pub error: RuntimeError,
    pub span: Span,
}
impl RuntimeFailure {
    pub fn new(error: RuntimeError, span: Span) -> Self {
        Self { error, span }
    }
    /// Presentation form. Deferred compile checks keep their own code and span.
    pub fn diagnostic(&self) -> Diagnostic {
        match &self.error {
            RuntimeError::DeferredCheck(diagnostic) => diagnostic.clone(),
            error => Diagnostic::new(error.code(), error.to_string(), self.span),
        }
    }
}
impl fmt::Display for RuntimeFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.diagnostic().fmt(f)
    }
}
impl std::error::Error for RuntimeFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeError {
    /// Checked arithmetic and numeric conversion failures.
    Number(NumberError),
    Arithmetic(ArithmeticError),
    Bounds(BoundsError),
    Uninitialized(UninitializedRead),
    Text(TextError),
    Input(InputError),
    Ownership(OwnershipError),
    Generator(GeneratorError),
    Unwrap(UnwrapError),
    Service(ServiceError),
    Lookup(LookupError),
    Allocator(AllocatorError),
    /// A dynamic `checked` conversion out of `Any` met a different runtime type.
    CheckedTypeMismatch,
    Limit(ResourceLimit),
    Exhausted(IdentitySpace),
    Gc(GcError),
    /// A deferred body failed its semantic checks when first called.
    DeferredCheck(Diagnostic),
    Engine(EngineError),
    /// The VM met a state the type checker should have excluded.
    Invariant(Invariant),
}
impl RuntimeError {
    /// Stable diagnostic code for each failure family.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Number(_) | Self::Arithmetic(_) => "R100",
            Self::Bounds(_) => "R110",
            Self::Uninitialized(_) => "R120",
            Self::Text(_) => "R130",
            Self::Input(InputError::EndOfInput) => "R002",
            Self::Input(_) => "R140",
            Self::Ownership(_) => "R150",
            Self::Generator(_) => "R160",
            Self::Unwrap(_) => "R170",
            Self::Service(_) | Self::Lookup(_) => "R180",
            Self::CheckedTypeMismatch => "R190",
            Self::Allocator(_) => "R200",
            Self::Limit(_) | Self::Exhausted(_) => "R210",
            Self::Gc(_) => "R220",
            Self::DeferredCheck(diagnostic) => diagnostic.code,
            Self::Engine(_) => "R900",
            Self::Invariant(_) => "R901",
        }
    }
}
impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(error) => error.fmt(f),
            Self::Arithmetic(error) => error.fmt(f),
            Self::Bounds(error) => error.fmt(f),
            Self::Uninitialized(error) => error.fmt(f),
            Self::Text(error) => error.fmt(f),
            Self::Input(error) => error.fmt(f),
            Self::Ownership(error) => error.fmt(f),
            Self::Generator(error) => error.fmt(f),
            Self::Unwrap(error) => error.fmt(f),
            Self::Service(error) => error.fmt(f),
            Self::Lookup(error) => error.fmt(f),
            Self::Allocator(error) => error.fmt(f),
            Self::CheckedTypeMismatch => f.write_str("checked conversion found a different runtime type"),
            Self::Limit(error) => error.fmt(f),
            Self::Exhausted(error) => error.fmt(f),
            Self::Gc(error) => error.fmt(f),
            Self::DeferredCheck(diagnostic) => f.write_str(&diagnostic.message),
            Self::Engine(error) => error.fmt(f),
            Self::Invariant(error) => write!(f, "internal error: {error}"),
        }
    }
}
impl std::error::Error for RuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Number(error) => Some(error),
            Self::DeferredCheck(diagnostic) => Some(diagnostic),
            _ => None,
        }
    }
}
macro_rules! from_variant {
    ($($source:ty => $variant:ident),* $(,)?) => {$(
        impl From<$source> for RuntimeError {
            fn from(error: $source) -> Self {
                Self::$variant(error)
            }
        }
    )*};
}
from_variant! {
    NumberError => Number,
    ArithmeticError => Arithmetic,
    BoundsError => Bounds,
    UninitializedRead => Uninitialized,
    TextError => Text,
    InputError => Input,
    OwnershipError => Ownership,
    GeneratorError => Generator,
    UnwrapError => Unwrap,
    ServiceError => Service,
    LookupError => Lookup,
    AllocatorError => Allocator,
    ResourceLimit => Limit,
    IdentitySpace => Exhausted,
    GcError => Gc,
    EngineError => Engine,
    Invariant => Invariant,
}

/// Writes one `Display` arm per unit variant.
macro_rules! messages {
    ($ty:ty { $($variant:ident => $text:literal),* $(,)? }) => {
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(match self { $(Self::$variant => $text),* })
            }
        }
        impl std::error::Error for $ty {}
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithmeticError {
    IntegerOverflow,
    DivisionByZero,
    ZeroStep,
    /// A math function's result is not a finite number.
    NonFiniteResult(cyallo_ir::intrinsic::Intrinsic),
    /// Truncation of a float does not fit the integer type.
    TruncationOutOfRange,
    ClampBoundsReversed,
    RandomBound,
}
impl fmt::Display for ArithmeticError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IntegerOverflow => f.write_str("integer overflow"),
            Self::DivisionByZero => f.write_str("division by zero"),
            Self::ZeroStep => f.write_str("loop step must not be zero"),
            Self::NonFiniteResult(function) => write!(f, "{function} result is not finite"),
            Self::TruncationOutOfRange => f.write_str("truncated value is outside the integer range"),
            Self::ClampBoundsReversed => f.write_str("clamp lower bound exceeds upper bound"),
            Self::RandomBound => f.write_str("random bound must be positive and finite"),
        }
    }
}
impl std::error::Error for ArithmeticError {}

/// Index failures keep the rejected index and the valid extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundsError {
    Array { index: i64, lower: i64, upper: i64 },
    Collection { index: i64, length: usize },
    Slice { index: i64, length: usize },
    SparseIndex(i64),
    Field(usize),
    StoragePath,
    SliceRange,
}
impl fmt::Display for BoundsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Array { index, lower, upper } => {
                write!(f, "array index {index} outside {lower}..={upper}")
            }
            Self::Collection { index, length } => {
                write!(f, "index {index} out of bounds for length {length}")
            }
            Self::Slice { index, length } => {
                write!(f, "slice index {index} out of bounds for length {length}")
            }
            Self::SparseIndex(index) => {
                write!(f, "sparse array index {index} outside 0..4294967295")
            }
            Self::Field(index) => write!(f, "field {index} out of bounds"),
            Self::StoragePath => f.write_str("storage path projection out of bounds"),
            Self::SliceRange => f.write_str("slice range out of bounds"),
        }
    }
}
impl std::error::Error for BoundsError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UninitializedRead {
    Storage,
    ArrayElement,
    /// A sparse array has no element at this index.
    Hole(u32),
    Collection,
}
impl fmt::Display for UninitializedRead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage => f.write_str("read of uninitialized storage"),
            Self::ArrayElement => f.write_str("read of uninitialized array element"),
            Self::Hole(index) => write!(f, "read of array hole at index {index}"),
            Self::Collection => f.write_str("read of uninitialized collection"),
        }
    }
}
impl std::error::Error for UninitializedRead {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextError {
    IndexOutOfBounds,
    NegativeLength,
    RangeOutOfBounds,
    InvalidScalar,
    CaseConversionNotSingleChar,
    InvalidNumber,
}
messages!(TextError {
    IndexOutOfBounds => "string index out of bounds",
    NegativeLength => "negative string length",
    RangeOutOfBounds => "string range out of bounds",
    InvalidScalar => "invalid Unicode scalar value",
    CaseConversionNotSingleChar => "case conversion does not produce a single char",
    InvalidNumber => "invalid numeric string",
});

/// Failures reading `input` into a typed destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputError {
    EndOfInput,
    Number(NumberError),
    Integer,
    Real,
    Boolean,
    EmptyChar,
    MultipleChars,
    UnsupportedType,
}
impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EndOfInput => f.write_str("INPUT reached end of input"),
            Self::Number(error) => write!(f, "invalid numeric input: {error}"),
            Self::Integer => f.write_str("invalid integer input"),
            Self::Real => f.write_str("invalid float input"),
            Self::Boolean => f.write_str("invalid boolean input"),
            Self::EmptyChar => f.write_str("empty char input"),
            Self::MultipleChars => f.write_str("char input requires one Unicode scalar"),
            Self::UnsupportedType => f.write_str("input does not support this type"),
        }
    }
}
impl std::error::Error for InputError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnershipError {
    ReleasedOwner,
    ReleasedAllocation,
    WeakNotUpgraded,
    StrongCountOverflow,
    WeakCountOverflow,
    MoveOnlyGenerator,
    MoveOnlyAllocation,
}
messages!(OwnershipError {
    ReleasedOwner => "use of a released owner",
    ReleasedAllocation => "use of a released allocation",
    WeakNotUpgraded => "weak owner must be upgraded before access",
    StrongCountOverflow => "strong count overflow",
    WeakCountOverflow => "weak count overflow",
    MoveOnlyGenerator => "generators are move-only",
    MoveOnlyAllocation => "allocations are move-only",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratorError {
    /// Resume after completion or close.
    Finished,
    AlreadyRunning,
    CloseWhileRunning,
    StartWithArgument,
    MissingResumeValue,
    Invalid,
    LimitExceeded,
}
messages!(GeneratorError {
    Finished => "cannot resume a completed or closed generator",
    AlreadyRunning => "generator is already running",
    CloseWhileRunning => "cannot close a running generator",
    StartWithArgument => "start a generator with resume(); arguments belong to its factory",
    MissingResumeValue => "resume requires the value expected by yield",
    Invalid => "invalid generator",
    LimitExceeded => "generator limit exceeded",
});

#[derive(Debug, Clone, PartialEq)]
pub enum UnwrapError {
    /// `unwrap` met `Err`; keeps the rendered error value.
    Err(Box<str>),
    /// `expect` met `None`; keeps the program's message.
    Expect(Box<str>),
    UnwrapErrOnOk,
    NoneValue,
    /// A generator step was read as the other variant.
    StepVariant,
}
impl fmt::Display for UnwrapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Err(error) => write!(f, "unwrap failed: {error}"),
            Self::Expect(message) => f.write_str(message),
            Self::UnwrapErrOnOk => f.write_str("unwrap_err called on Ok"),
            Self::NoneValue => f.write_str("unwrap called on None"),
            Self::StepVariant => f.write_str("generator step has the other variant"),
        }
    }
}
impl std::error::Error for UnwrapError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceError {
    Unavailable,
    NotInjected,
    Released,
    InactiveContext,
    ProviderConflict,
    TraitRequired,
    InjectRequiresCallback,
}
messages!(ServiceError {
    Unavailable => "service is unavailable",
    NotInjected => "service is unavailable or was not injected",
    Released => "service was released",
    InactiveContext => "service context is inactive",
    ProviderConflict => "service already has an active provider in this isolation",
    TraitRequired => "service must implement a trait",
    InjectRequiresCallback => "inject requires a callback",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupError {
    MapperReturnedNothing,
    MapperResultNotPlan,
    MapperNotReadonly,
    MapperInvokesService,
    MapperResolvesServiceBinding,
    MapperInvokesProcedure,
    ForeignPlan,
    UninitializedGuard,
    MissingPlan,
    NegativePlanIndex,
}
messages!(LookupError {
    MapperReturnedNothing => "mapper returned no plan",
    MapperResultNotPlan => "mapper must return a lookup plan",
    MapperNotReadonly => "lookup mapper must be readonly: no mutable globals, writes, I/O or nondeterminism",
    MapperInvokesService => "lookup mapper cannot invoke a service",
    MapperResolvesServiceBinding => "lookup mapper cannot resolve a mutable service binding",
    MapperInvokesProcedure => "lookup mapper cannot invoke an unchecked procedure value",
    ForeignPlan => "lookup plan belongs to a different receiver",
    UninitializedGuard => "lookup guards require initialized owned values",
    MissingPlan => "cannot project a missing lookup plan",
    NegativePlanIndex => "negative lookup plan index",
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllocatorError {
    /// A program-defined allocator returned `Err`; keeps its message.
    Rejected(Box<str>),
    InvalidLayout,
    InvalidSize,
    InvalidAlignment,
    SizeOverflow,
    AllocationFailed,
    InsufficientBlock,
    ResultNotAllocation,
    InvalidResultType,
    MissingState,
    InvalidCleanup,
}
impl fmt::Display for AllocatorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected(message) => write!(f, "allocator failed: {message}"),
            other => f.write_str(other.message()),
        }
    }
}
impl std::error::Error for AllocatorError {}
impl AllocatorError {
    fn message(&self) -> &'static str {
        match self {
            Self::Rejected(_) => "allocator failed",
            Self::InvalidLayout => "invalid allocation layout",
            Self::InvalidSize => "invalid allocation size",
            Self::InvalidAlignment => "invalid allocation alignment",
            Self::SizeOverflow => "allocation size overflow",
            Self::AllocationFailed => "allocation failed",
            Self::InsufficientBlock => "allocator returned insufficient size or alignment",
            Self::ResultNotAllocation => "allocator returned a non-allocation",
            Self::InvalidResultType => "allocator must return Result<Allocation, AllocError>",
            Self::MissingState => "missing allocator state",
            Self::InvalidCleanup => "invalid cleanup callback",
        }
    }
}

/// Configured Session limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceLimit {
    CallDepth,
    Storage,
    Allocation,
    Context,
    Registration,
    ConstantAllocation,
    Instructions,
}
messages!(ResourceLimit {
    CallDepth => "call depth limit exceeded",
    Storage => "execution storage limit exceeded",
    Allocation => "allocation storage limit exceeded",
    Context => "context storage limit exceeded",
    Registration => "registration storage limit exceeded",
    ConstantAllocation => "constant evaluation exceeded its allocation limit",
    Instructions => "instruction limit reached",
});

/// A monotonically allocated identity or counter ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentitySpace {
    StorageIdentity,
    StorageVersion,
    Generator,
    Symbol,
}
messages!(IdentitySpace {
    StorageIdentity => "storage identity space exhausted",
    StorageVersion => "storage version space exhausted",
    Generator => "generator identity space exhausted",
    Symbol => "symbol space exhausted",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcError {
    Disabled,
    ForeignRoot,
    /// A root or heap edge points past the heap. `edge` distinguishes heap edges.
    DanglingReference { edge: bool },
}
impl fmt::Display for GcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => f.write_str("garbage collection is not enabled for this Session"),
            Self::ForeignRoot => f.write_str("root belongs to a different Session"),
            Self::DanglingReference { edge: false } => f.write_str("invalid managed reference"),
            Self::DanglingReference { edge: true } => f.write_str("invalid managed heap edge"),
        }
    }
}
impl std::error::Error for GcError {}

/// Execution-engine failures, independent of the guest program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineError {
    ForeignProgram,
    NoProgress,
    /// A Rust panic was caught at an engine boundary.
    Panic,
    /// Generated code trapped outside the runtime's helpers.
    Trap,
}
messages!(EngineError {
    ForeignProgram => "artifact belongs to a different program",
    NoProgress => "compiled routine made no progress",
    Panic => "internal runtime failure",
    Trap => "native code trapped",
});

/// The runtime representation a VM operation required.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    Integer,
    Number,
    Boolean,
    Char,
    String,
    Generator,
    GeneratorStep,
    Cell,
    Location,
    Option,
    Result,
    Layout,
    Allocation,
    AllocError,
    ReferenceCounted,
    Context,
    Registration,
    ServiceKey,
    TraitObject,
    SparseArray,
    Array,
    Range,
    Callable,
    Address,
    Composite,
    Record,
    Any,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invariant {
    Expected(ValueKind),
    UnknownRecordLayout,
    UnknownRecordField,
    UnknownFunction,
    UnsupportedIntrinsic(cyallo_ir::intrinsic::Intrinsic),
    MissingReturnValue,
    MissingContinuation,
    MissingResumer,
    YieldOutsideGenerator,
    InvalidStorage,
    InvalidSliceStorage,
    InvalidCollectionType,
    InvalidManagedType,
    InvalidTraitMethod,
    ArrayShapeMismatch,
    /// An operator reached the VM with operands it does not define.
    UnsupportedOperation,
    /// Generated code reported a fault code Rust does not define.
    UnknownNativeFault(u64),
}
impl fmt::Display for Invariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Expected(kind) => write!(f, "expected a {kind:?} value"),
            Self::UnknownRecordLayout => f.write_str("unknown record layout"),
            Self::UnknownRecordField => f.write_str("unknown record field"),
            Self::UnknownFunction => f.write_str("unknown function"),
            Self::UnsupportedIntrinsic(intrinsic) => write!(f, "unsupported builtin {intrinsic}"),
            Self::MissingReturnValue => f.write_str("function returned without a value"),
            Self::MissingContinuation => f.write_str("generator has no continuation"),
            Self::MissingResumer => f.write_str("generator has no resumer"),
            Self::YieldOutsideGenerator => f.write_str("yield outside its lexical generator"),
            Self::InvalidStorage => f.write_str("invalid storage"),
            Self::InvalidSliceStorage => f.write_str("invalid slice storage"),
            Self::InvalidCollectionType => f.write_str("invalid collection type"),
            Self::InvalidManagedType => f.write_str("invalid managed allocation type"),
            Self::InvalidTraitMethod => f.write_str("invalid trait method"),
            Self::ArrayShapeMismatch => f.write_str("array shape mismatch"),
            Self::UnsupportedOperation => f.write_str("unsupported operation for these operands"),
            Self::UnknownNativeFault(bits) => write!(f, "unknown native fault code {bits}"),
        }
    }
}
impl std::error::Error for Invariant {}

/// Shorthand for a VM invariant that a value has the expected representation.
pub(crate) fn expected(kind: ValueKind) -> RuntimeError {
    RuntimeError::Invariant(Invariant::Expected(kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_render_through_their_family_code() {
        let span = Span::new(3, 7);
        let failure = RuntimeFailure::new(ArithmeticError::IntegerOverflow.into(), span);
        let diagnostic = failure.diagnostic();
        assert_eq!(diagnostic.code, "R100");
        assert_eq!(diagnostic.span, span);
        assert_eq!(diagnostic.message, "integer overflow");
        assert_eq!(RuntimeError::from(InputError::EndOfInput).code(), "R002");
        assert_eq!(RuntimeError::from(Invariant::UnknownFunction).code(), "R901");
    }

    #[test]
    fn deferred_checks_keep_their_original_diagnostic() {
        let original = Diagnostic::new("E106", "unresolved name", Span::new(10, 12));
        let failure = RuntimeFailure::new(
            RuntimeError::DeferredCheck(original.clone()),
            Span::new(0, 1),
        );
        assert_eq!(failure.diagnostic(), original);
        assert_eq!(failure.error.code(), "E106");
    }

    #[test]
    fn bounds_failures_keep_index_and_extent() {
        let error = RuntimeError::from(BoundsError::Array {
            index: 3,
            lower: 1,
            upper: 2,
        });
        assert_eq!(error.to_string(), "array index 3 outside 1..=2");
        assert!(matches!(
            error,
            RuntimeError::Bounds(BoundsError::Array { index: 3, .. })
        ));
    }
}
