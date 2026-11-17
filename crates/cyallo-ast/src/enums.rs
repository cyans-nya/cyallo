//! Data enum declarations and source patterns. Identities are assigned by resolution.
use crate::*;
#[derive(Debug, Clone, Hash)]
pub struct EnumDeclaration {
    pub resolved: Option<ResolvedEnum>,
    pub name: Box<str>,
    pub type_params: Vec<Box<str>>,
    pub open: bool,
    pub annotations: Vec<DeclarationAnnotation>,
    pub variants: Vec<VariantDeclaration>,
    pub span: Span,
}
#[derive(Debug, Clone, Hash)]
pub struct VariantDeclaration {
    pub resolved: Option<ResolvedVariant>,
    pub name: Box<str>,
    pub payload: Option<Type>,
    pub annotations: Vec<DeclarationAnnotation>,
    pub span: Span,
}
#[derive(Debug, Clone, Hash)]
pub struct Pattern {
    pub kind: PatternKind,
    pub span: Span,
}
#[derive(Debug, Clone, Hash)]
pub enum PatternKind {
    Wildcard,
    Binding(Box<str>),
    Variant(Box<str>, Option<Box<Pattern>>),
    ResolvedVariant(VariantId, Option<Box<Pattern>>),
    Tuple(Vec<Pattern>),
    Record(Vec<(Box<str>, Pattern)>, bool),
    Literal(Literal),
    Or(Vec<Pattern>),
}
#[derive(Debug, Clone, Hash)]
pub struct MatchArm {
    pub package: Option<DeclarationId>,
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: MatchBody,
    pub span: Span,
}
#[derive(Debug, Clone, Hash)]
pub enum MatchBody {
    Expression(Expr),
    Block(Vec<Stmt>, Option<Box<Expr>>),
}

/// Declaration ids below this are reserved for the variants of the always-present
/// `core` enums, so every program, module and runtime names them identically.
pub const RESERVED_DECLARATIONS: u32 = 16;
impl VariantId {
    pub const OPTION_SOME: Self = Self(DeclarationId(0));
    pub const OPTION_NONE: Self = Self(DeclarationId(1));
    pub const RESULT_OK: Self = Self(DeclarationId(2));
    pub const RESULT_ERR: Self = Self(DeclarationId(3));
    pub const STEP_YIELDED: Self = Self(DeclarationId(4));
    pub const STEP_COMPLETE: Self = Self(DeclarationId(5));
    pub const SEND_CLOSED: Self = Self(DeclarationId(6));
    pub const SEND_FULL: Self = Self(DeclarationId(7));
    pub const SEND_LIMIT: Self = Self(DeclarationId(8));
    pub const RECEIVE_CLOSED: Self = Self(DeclarationId(9));
    pub const RECEIVE_EMPTY: Self = Self(DeclarationId(10));
}
impl EnumEncoding {
    /// The reserved variant id of a `core` role, if it has one.
    pub fn reserved(self) -> Option<VariantId> {
        Some(match self {
            Self::OptionSome => VariantId::OPTION_SOME,
            Self::OptionNone => VariantId::OPTION_NONE,
            Self::ResultOk => VariantId::RESULT_OK,
            Self::ResultErr => VariantId::RESULT_ERR,
            Self::StepYielded => VariantId::STEP_YIELDED,
            Self::StepComplete => VariantId::STEP_COMPLETE,
            Self::SendClosed => VariantId::SEND_CLOSED,
            Self::SendFull => VariantId::SEND_FULL,
            Self::SendLimit => VariantId::SEND_LIMIT,
            Self::ReceiveClosed => VariantId::RECEIVE_CLOSED,
            Self::ReceiveEmpty => VariantId::RECEIVE_EMPTY,
            Self::General => return None,
        })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EnumId(pub DeclarationId);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VariantId(pub DeclarationId);
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EnumIdentity {
    pub declaration: EnumId,
    pub arguments: Vec<Type>,
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EnumType {
    pub identity: EnumIdentity,
    pub name: Box<str>,
    pub package: DeclarationId,
    pub open: bool,
    pub variants: Vec<VariantType>,
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VariantType {
    pub encoding: EnumEncoding,
    pub declaration: VariantId,
    pub name: Box<str>,
    pub symbol: Box<str>,
    pub payload: Option<Type>,
}
#[derive(Debug, Clone, Hash)]
pub struct ResolvedEnum {
    pub declaration: EnumId,
    pub package: DeclarationId,
    pub arguments: Vec<Type>,
}
#[derive(Debug, Clone, Hash)]
pub struct ResolvedVariant {
    pub encoding: EnumEncoding,
    pub declaration: VariantId,
    pub symbol: Box<str>,
}

/// An explicitly annotated native encoding, independent of public names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnumEncoding {
    General,
    OptionSome,
    OptionNone,
    ResultOk,
    ResultErr,
    StepYielded,
    StepComplete,
    SendClosed,
    SendFull,
    SendLimit,
    ReceiveClosed,
    ReceiveEmpty,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EnumVariant {
    pub declaration: VariantId,
    pub encoding: EnumEncoding,
}
#[derive(Debug, Clone, Hash)]
pub struct NativeEnumDeclaration {
    pub storage: StorageKind,
    pub declaration: EnumDeclaration,
}
impl VariantType {
    pub fn operation(&self) -> EnumVariant {
        EnumVariant {
            declaration: self.declaration,
            encoding: self.encoding,
        }
    }
}
