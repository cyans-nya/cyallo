//! Descriptive diagnostic constructors own the mapping from typed failures to
//! presentation codes. Call sites carry semantic errors, not code literals.
use crate::{Diagnostic, NumberError, Span};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedSyntaxError {
    LegacySigil(crate::MemoryKind),
    UnknownLocality(Box<str>),
    RepeatedLocality,
    ExpectedStorage,
}
impl std::fmt::Display for ManagedSyntaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LegacySigil(kind) => write!(
                f,
                "use `{kind}` without `@`; locality modifiers are `@local` and `@shared`"
            ),
            Self::UnknownLocality(name) => write!(
                f,
                "unknown locality modifier `@{name}`; expected `@local` or `@shared`"
            ),
            Self::RepeatedLocality => {
                f.write_str("a managed value may have only one locality modifier")
            }
            Self::ExpectedStorage => f.write_str("a locality modifier must precede `gc` or `rc`"),
        }
    }
}
impl std::error::Error for ManagedSyntaxError {}
pub fn invalid_managed_syntax(error: ManagedSyntaxError, span: Span) -> Diagnostic {
    Diagnostic::new("E178", error.to_string(), span)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedLocalityError {
    SharedRuntimeUnavailable,
}
impl std::fmt::Display for ManagedLocalityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("shared managed graphs are not implemented; `gc` and `rc` values are local to their coroutine")
    }
}
impl std::error::Error for ManagedLocalityError {}
pub fn shared_managed_unavailable(span: Span) -> Diagnostic {
    Diagnostic::new(
        "E179",
        ManagedLocalityError::SharedRuntimeUnavailable.to_string(),
        span,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DereferenceSyntaxError {
    PrefixRemoved,
}
impl std::fmt::Display for DereferenceSyntaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("use postfix dereference `value.*` instead of `*value`")
    }
}
impl std::error::Error for DereferenceSyntaxError {}
pub fn prefix_dereference_removed(span: Span) -> Diagnostic {
    Diagnostic::new(
        "E180",
        DereferenceSyntaxError::PrefixRemoved.to_string(),
        span,
    )
}

pub fn invalid_numeric_literal(error: NumberError, span: Span) -> Diagnostic {
    Diagnostic::new("E001", error.to_string(), span)
}
pub fn numeric_conversion_failed(error: NumberError, span: Span) -> Diagnostic {
    Diagnostic::new("E109", error.to_string(), span)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratorDiagnostic {
    FactorySignatureRequired,
    CaptureInPureFactory,
    YieldOutsideGenerator,
    DelegateExpectedGenerator,
    BorrowedYield,
}
impl std::fmt::Display for GeneratorDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::FactorySignatureRequired => {
                "a generator factory requires -> Generator<Yield, Resume, Return>"
            }
            Self::CaptureInPureFactory => "gen fn captures nothing; use gen proc to capture values",
            Self::YieldOutsideGenerator => "yield is only valid in its lexical generator body",
            Self::DelegateExpectedGenerator => "yield from requires an owned generator",
            Self::BorrowedYield => "a generator cannot yield borrowed values",
        })
    }
}
impl std::error::Error for GeneratorDiagnostic {}
fn generator_failure(error: GeneratorDiagnostic, span: Span) -> Diagnostic {
    Diagnostic::new("E160", error.to_string(), span)
}
pub fn generator_factory_signature_required(span: Span) -> Diagnostic {
    generator_failure(GeneratorDiagnostic::FactorySignatureRequired, span)
}
pub fn generator_capture_requires_proc(span: Span) -> Diagnostic {
    generator_failure(GeneratorDiagnostic::CaptureInPureFactory, span)
}
pub fn yield_outside_generator(span: Span) -> Diagnostic {
    generator_failure(GeneratorDiagnostic::YieldOutsideGenerator, span)
}
pub fn generator_delegate_required(span: Span) -> Diagnostic {
    generator_failure(GeneratorDiagnostic::DelegateExpectedGenerator, span)
}
pub fn borrowed_generator_yield(span: Span) -> Diagnostic {
    generator_failure(GeneratorDiagnostic::BorrowedYield, span)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConcurrentCallError {
    pub declaration: &'static str,
    pub types: Vec<crate::Type>,
    pub arguments: Vec<crate::Type>,
}
impl std::fmt::Display for ConcurrentCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}` does not accept type arguments (", self.declaration)?;
        for (i, t) in self.types.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{t}")?;
        }
        f.write_str(") and operands (")?;
        for (i, t) in self.arguments.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{t}")?;
        }
        f.write_str("); concurrency requires transferable owned values and explicitly sized numeric operands")
    }
}
impl std::error::Error for ConcurrentCallError {}
pub fn invalid_concurrent_call(
    declaration: &'static str,
    types: &[crate::Type],
    arguments: &[crate::Type],
    span: Span,
) -> Diagnostic {
    let error = ConcurrentCallError {
        declaration,
        types: types.to_vec(),
        arguments: arguments.to_vec(),
    };
    Diagnostic::new("E170", error.to_string(), span)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnCaptureError {
    UnknownCallable,
    LocalCapture(crate::Type),
}
impl std::fmt::Display for SpawnCaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownCallable => f.write_str("a local callable has no proven transport contract; spawn a locally constructed closure with transferable captures"),
            Self::LocalCapture(ty) => write!(f, "cannot spawn a closure capturing local `{ty}`; managed objects and borrows cannot cross coroutines"),
        }
    }
}
impl std::error::Error for SpawnCaptureError {}
pub fn local_callable_cannot_spawn(span: Span) -> Diagnostic {
    Diagnostic::new("E171", SpawnCaptureError::UnknownCallable.to_string(), span)
}
pub fn local_capture_cannot_spawn(ty: crate::Type, span: Span) -> Diagnostic {
    Diagnostic::new(
        "E171",
        SpawnCaptureError::LocalCapture(ty).to_string(),
        span,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectSyntaxError {
    CyclicAlias,
    AliasDepth,
    DuplicateProperty,
    PropertyBorrow,
    ReadonlyBorrow,
}
impl std::fmt::Display for ObjectSyntaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::CyclicAlias => "cyclic type alias",
            Self::AliasDepth => "type alias expansion exceeds 256 levels",
            Self::DuplicateProperty => "duplicate object property",
            Self::PropertyBorrow => "object properties do not expose stable borrowed storage",
            Self::ReadonlyBorrow => "cannot write object properties through a readonly borrow",
        })
    }
}
impl std::error::Error for ObjectSyntaxError {}
pub fn cyclic_type_alias(span: crate::Span) -> crate::Diagnostic {
    crate::Diagnostic::new("E181", ObjectSyntaxError::CyclicAlias.to_string(), span)
}
pub fn duplicate_object_property(span: crate::Span) -> crate::Diagnostic {
    crate::Diagnostic::new(
        "E181",
        ObjectSyntaxError::DuplicateProperty.to_string(),
        span,
    )
}
pub fn object_property_borrow(span: crate::Span) -> crate::Diagnostic {
    crate::Diagnostic::new("E181", ObjectSyntaxError::PropertyBorrow.to_string(), span)
}
pub fn readonly_object_borrow(span: crate::Span) -> crate::Diagnostic {
    crate::Diagnostic::new("E181", ObjectSyntaxError::ReadonlyBorrow.to_string(), span)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DynamicBorrowEscape;
impl std::fmt::Display for DynamicBorrowEscape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("borrowed values cannot escape into owned dynamic storage")
    }
}
impl std::error::Error for DynamicBorrowEscape {}
pub fn dynamic_borrow_escape(span: crate::Span) -> crate::Diagnostic {
    crate::Diagnostic::new("E182", DynamicBorrowEscape.to_string(), span)
}

pub fn type_alias_depth(span: crate::Span) -> crate::Diagnostic {
    crate::Diagnostic::new("E181", ObjectSyntaxError::AliasDepth.to_string(), span)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NumericCastError {
    NonNumeric(crate::Type),
}
impl std::fmt::Display for NumericCastError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonNumeric(actual) => {
                write!(f, "numeric cast requires a numeric value, got {actual}")
            }
        }
    }
}
impl std::error::Error for NumericCastError {}
pub fn numeric_cast_requires_number(actual: crate::Type, span: crate::Span) -> crate::Diagnostic {
    crate::Diagnostic::new(
        "E109",
        NumericCastError::NonNumeric(actual).to_string(),
        span,
    )
}
