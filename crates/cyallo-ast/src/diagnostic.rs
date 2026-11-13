//! Descriptive diagnostic constructors own the mapping from typed failures to
//! presentation codes. Call sites carry semantic errors, not code literals.
use crate::{Diagnostic, NumberError, Span};

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
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(match self {
        Self::FactorySignatureRequired => "a generator factory requires -> Generator<Yield, Resume, Return>",
        Self::CaptureInPureFactory => "gen fn captures nothing; use gen proc to capture values",
        Self::YieldOutsideGenerator => "yield is only valid in its lexical generator body",
        Self::DelegateExpectedGenerator => "yield from requires an owned generator",
        Self::BorrowedYield => "a generator cannot yield borrowed values",
    }) }
}
impl std::error::Error for GeneratorDiagnostic {}
fn generator_failure(error: GeneratorDiagnostic, span: Span) -> Diagnostic {
    Diagnostic::new("E160", error.to_string(), span)
}
pub fn generator_factory_signature_required(span: Span) -> Diagnostic { generator_failure(GeneratorDiagnostic::FactorySignatureRequired, span) }
pub fn generator_capture_requires_proc(span: Span) -> Diagnostic { generator_failure(GeneratorDiagnostic::CaptureInPureFactory, span) }
pub fn yield_outside_generator(span: Span) -> Diagnostic { generator_failure(GeneratorDiagnostic::YieldOutsideGenerator, span) }
pub fn generator_delegate_required(span: Span) -> Diagnostic { generator_failure(GeneratorDiagnostic::DelegateExpectedGenerator, span) }
pub fn borrowed_generator_yield(span: Span) -> Diagnostic { generator_failure(GeneratorDiagnostic::BorrowedYield, span) }
