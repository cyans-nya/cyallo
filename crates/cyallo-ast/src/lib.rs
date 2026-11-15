//! Source-preserving syntax and diagnostics for the 9618 executable dialect.
pub use oxc_span::{GetSpan, GetSpanMut, SPAN, Span};
pub mod diagnostic;
mod display;
pub use display::source_name;
mod number;
pub use number::{Number, NumberError, NumberKind};
mod operator;
pub use operator::Operator;
/// Opaque declaration identity attached by semantic resolution, never the parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IntrinsicId(pub u16, pub Option<DeclarationId>);
impl IntrinsicId {
    pub fn declared(self, declaration: DeclarationId) -> Self {
        Self(self.0, Some(declaration))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeclarationId(pub u32);
/// Canonical builtin type constructors, assigned by type resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StorageKind {
    Cell,
    Location,
    StoragePath,
    Option,
    Result,
    Vec,
    VecDeque,
    Range,
    ParseError,
    Weak,
    ArenaCell,
    Layout,
    Allocation,
    AllocError,
    Generator,
    PureGenerator,
    GeneratorStep,
    Sender,
    Receiver,
    SendError,
    ReceiveError,
    Clock,
    Task,
    TaskOutcome,
    LoadError,
    ViewError,
    TypeDescriptor,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryKind {
    Gc,
    Rc,
}
/// Coroutine confinement is independent of the allocation's lifetime strategy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Locality {
    #[default]
    Local,
    Shared,
}
/// Semantic identifiers are exact. Compatibility syntax folds names while parsing.
pub trait IdentifierEq {
    fn eq_identifier(&self, other: &str) -> bool;
}
impl IdentifierEq for str {
    fn eq_identifier(&self, other: &str) -> bool {
        self == other
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub message: String,
    pub span: Span,
    pub related: Vec<(Span, String)>,
}
impl Diagnostic {
    pub fn new(code: &'static str, message: impl Into<String>, span: Span) -> Self {
        Self {
            code,
            message: message.into(),
            span,
            related: vec![],
        }
    }
    pub fn with_related(mut self, span: Span, message: impl Into<String>) -> Self {
        self.related.push((span, message.into()));
        self
    }
}
impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} at {}..{}: {}",
            self.code, self.span.start, self.span.end, self.message
        )
    }
}
impl std::error::Error for Diagnostic {}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    /// An unresolved generic native function value, retaining declaration identity.
    NativeCallable(DeclarationId),
    /// A value operand named by a native declaration's result contract.
    ValueParameter(usize),
    /// Application of an annotated native type-family declaration.
    Dependent(DeclarationId, Vec<Type>),
    Lifetime(Box<str>),
    SelfType,
    Numeric(NumberKind),
    Integer,
    Real,
    Boolean,
    Char,
    String,
    Array(Vec<(i64, i64)>, Box<Type>),
    SparseArray(Box<Type>),
    Named(Box<str>),
    Applied(Box<str>, Vec<Type>),
    Record(Box<str>, Vec<(Box<str>, Type)>),
    /// A resolved nominal backedge through an indirection in a recursive layout.
    RecordRef(Box<str>),
    Tuple(Vec<Type>),
    Function(RoutineKind, Vec<(Type, bool)>, Box<Type>),
    Borrowed(Box<Type>),
    Reference {
        mutable: bool,
        lifetime: Option<Box<str>>,
        pointee: Box<Type>,
    },
    Managed {
        kind: MemoryKind,
        locality: Locality,
        lifetime: Option<Box<str>>,
        pointee: Box<Type>,
    },
    Slice(Box<Type>),
    Context,
    Registration,
    ServiceKey(Box<Type>),
    ServiceRef(Box<Type>),
    TraitObject(Box<str>),
    /// General managed storage/lookup wrappers, independent of services.
    Storage(StorageKind, Box<Type>),
    Symbol,
    Any,
    Dynamic,
    Object(Vec<(Box<str>, Type)>),
    Void,
}
impl Type {
    pub fn pointee_or_self(&self) -> &Self {
        match self {
            Self::Reference { pointee, .. } | Self::Managed { pointee, .. } => pointee,
            _ => self,
        }
    }
    pub fn numeric(kind: NumberKind) -> Self {
        match kind {
            NumberKind::I64 => Self::Integer,
            NumberKind::F64 => Self::Real,
            _ => Self::Numeric(kind),
        }
    }
    pub fn number_kind(&self) -> Option<NumberKind> {
        match self {
            Self::Integer => Some(NumberKind::I64),
            Self::Real => Some(NumberKind::F64),
            Self::Numeric(k) => Some(*k),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Number(Number),
    Integer(i64),
    Real(f64),
    Boolean(bool),
    Char(char),
    String(Box<str>),
}
impl Literal {
    pub fn number(n: Number) -> Self {
        match n.kind {
            NumberKind::I64 => Self::Integer(n.int() as i64),
            NumberKind::F64 => Self::Real(n.real()),
            _ => Self::Number(n),
        }
    }
}
#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}
/// A deeply immutable, address-free constant pool payload. Sharing its Arc never
/// shares mutable Session storage, and GC relocation cannot invalidate it.
#[derive(Debug, Clone)]
pub enum ConstantValue {
    Number(Number),
    Integer(i64),
    Real(f64),
    Boolean(bool),
    Char(char),
    String(std::sync::Arc<str>),
    Array(Vec<ConstantValue>),
    Sparse(std::collections::BTreeMap<u32, ConstantValue>, u64),
}
impl ConstantValue {
    pub fn allocation_size(&self) -> usize {
        let extra = match self {
            Self::String(s) => s.len(),
            Self::Array(values) => values
                .iter()
                .map(Self::allocation_size)
                .fold(0usize, usize::saturating_add),
            Self::Sparse(values, _) => values
                .values()
                .map(Self::allocation_size)
                .fold(0usize, usize::saturating_add),
            _ => 0,
        };
        128usize.saturating_add(extra)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    Typed,
    Dynamic,
}
#[derive(Debug, Clone)]
pub enum ExprKind {
    /// Evaluate a native function value, then materialize its checked ordinary
    /// callable adapter. The first expression's effects must be preserved.
    NativeCoerce(Box<Expr>, Box<Expr>),
    Yield(Box<Expr>, bool),
    Invalid(Diagnostic),
    ManagedNew(MemoryKind, Locality, Option<Box<Expr>>, Box<Expr>),
    IntrinsicCall(IntrinsicId, Vec<Type>, Vec<Expr>),
    Frozen(std::sync::Arc<ConstantValue>, Type),
    Literal(Literal),
    Name(Box<str>),
    TypeDescriptor(ExportedType),
    DeferredCallable(Box<str>),
    ObjectLiteral(ObjectKind, Vec<(Box<str>, Expr)>),
    DeleteProperty(Box<Expr>),
    ArrayLiteral(Vec<Expr>),
    Tuple(Vec<Expr>),
    Record(Box<str>, Vec<(Box<str>, Expr)>),
    Field(Box<Expr>, Box<str>),
    TupleField(Box<Expr>, usize),
    Invoke(Box<Expr>, Vec<Expr>),
    Closure(Box<Function>, Vec<ReferenceCapture>),
    ClosureValue(Box<str>, Vec<Expr>, Type),
    TypedCollection(Box<str>, Vec<Type>, Vec<Expr>),
    GenericCall(Box<str>, Vec<Type>, Vec<Expr>),
    GenericRecord(Box<str>, Vec<Type>, Vec<(Box<str>, Expr)>),
    Cast(Box<Expr>, Type),
    ExtensionField(Box<Expr>, Box<str>, Box<str>),
    Parenthesized(Box<Expr>),
    Index(Box<Expr>, Vec<Expr>),
    Call(Box<str>, Vec<Expr>),
    Unary(Operator, Box<Expr>),
    Binary(Operator, Box<Expr>, Box<Expr>),
}
#[derive(Debug, Clone)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}
#[derive(Debug, Clone)]
pub enum StmtKind {
    /// Name, annotation, compile-time constant, mutable, initializer.
    Binding(Box<str>, Option<Type>, bool, bool, Expr),
    Compound(Expr, Operator, Expr),
    Evaluate(Expr),
    Break,
    Continue,
    ReturnVoid,
    Declare(Box<str>, Type),
    Constant(Box<str>, Expr),
    Assign(Expr, Expr),
    Output(Vec<Expr>),
    Input(Expr),
    If(Expr, Vec<Stmt>, Vec<Stmt>),
    While(Expr, Vec<Stmt>),
    Repeat(Vec<Stmt>, Expr),
    /// CAIE `FOR name <- first TO last [STEP step]`: inclusive, signed step.
    For(Box<str>, Expr, Expr, Option<Expr>, Vec<Stmt>),
    /// `for name in iterable { body }`; the iterable is a range expression.
    ForIn(Box<str>, Expr, Vec<Stmt>),
    Case(Expr, Vec<(CaseLabel, Vec<Stmt>)>, Vec<Stmt>),
    Call(Box<str>, Vec<Expr>),
    Return(Expr),
}
#[derive(Debug, Clone)]
pub struct Param {
    pub arity: ParameterArity,
    pub passing: ParameterPassing,
    pub receiver: bool,
    pub name: Box<str>,
    pub ty: Type,
    pub by_ref: bool,
    pub span: Span,
}
#[derive(Debug, Clone)]
pub struct ReferenceCapture {
    pub name: Box<str>,
    pub mutable: bool,
}
#[derive(Debug, Clone)]
pub struct Function {
    pub annotations: Vec<DeclarationAnnotation>,
    pub body_kind: FunctionBodyKind,
    pub constraints: Vec<GenericConstraint>,
    pub type_packs: Vec<Box<str>>,
    pub generator: bool,
    pub source_name: Box<str>,
    pub origin: RoutineOrigin,
    pub attributes: RoutineAttributes,
    pub type_params: Vec<Box<str>>,
    pub kind: RoutineKind,
    pub name: Box<str>,
    pub params: Vec<Param>,
    pub result: Type,
    pub body: Vec<Stmt>,
    pub span: Span,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionBodyKind {
    Source,
    Declaration,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarationAnnotation {
    pub name: Box<str>,
    pub value: Box<str>,
    pub span: Span,
}
#[derive(Debug, Clone)]
pub struct GenericConstraint {
    pub parameter: Box<str>,
    pub bounds: Vec<Type>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParameterArity {
    #[default]
    One,
    Variadic,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParameterPassing {
    #[default]
    Value,
    /// A native call reads a VM descriptor without taking its language owner.
    Read,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RoutineOrigin {
    #[default]
    Source,
    Closure,
    Implementation,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutineKind {
    Function,
    Procedure,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HeatHint {
    #[default]
    Auto,
    Hot,
    Cold,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CompileHint {
    #[default]
    Auto,
    Eager,
    Lazy,
    AlwaysEager,
    AlwaysLazy,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InlineHint {
    #[default]
    Auto,
    Preferred,
    Never,
    Always,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RoutineAttributes {
    pub heat: HeatHint,
    pub compilation: CompileHint,
    pub inlining: InlineHint,
    pub heat_span: Option<Span>,
    pub compilation_span: Option<Span>,
    pub inline_span: Option<Span>,
}
impl RoutineAttributes {
    pub fn is_empty(self) -> bool {
        self.heat_span.is_none() && self.compilation_span.is_none() && self.inline_span.is_none()
    }
}
#[derive(Debug, Clone, Default)]
pub struct Ast {
    pub type_aliases: Vec<TypeAlias>,
    pub resolved_modules: Vec<ResolvedModuleExports>,
    pub native: NativeDeclarations,
    pub source_names: std::collections::HashMap<Box<str>, Box<str>>,
    pub body: Vec<Stmt>,
    pub functions: Vec<Function>,
    pub records: Vec<Record>,
    pub imports: Vec<Import>,
    pub exports: Vec<Box<str>>,
    pub traits: Vec<Trait>,
    pub implementations: Vec<Implementation>,
    pub properties: Vec<Property>,
    pub modules: Vec<ModuleDeclaration>,
    pub inherent_implementations: Vec<InherentImplementation>,
}
#[derive(Debug, Clone)]
pub struct TypeAlias {
    pub name: Box<str>,
    pub ty: Type,
    pub span: Span,
}
#[derive(Debug, Clone)]
pub struct ResolvedModuleExports {
    pub path: Box<str>,
    pub values: Vec<ResolvedExport>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportKind {
    Value,
    Module,
    Type,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedType {
    pub name: Box<str>,
    pub identity: Box<str>,
}
#[derive(Debug, Clone)]
pub struct ResolvedExport {
    pub kind: ExportKind,
    pub name: Box<str>,
    pub target: Box<str>,
}
/// A loader may install an independently resolved package under a private node.
/// This changes lexical ancestry only; it never grants core provenance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ModuleBoundary {
    #[default]
    Lexical,
    Package,
}
#[derive(Debug, Clone)]
pub struct ModuleDeclaration {
    pub boundary: ModuleBoundary,
    pub annotations: Vec<DeclarationAnnotation>,
    pub name: Box<str>,
    pub public: bool,
    pub body: Option<Box<Ast>>,
    pub span: Span,
}
#[derive(Debug, Clone)]
pub struct Import {
    pub path: Box<str>,
    pub alias: Option<Box<str>>,
    pub span: Span,
    pub public: bool,
    pub glob: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraitOrigin {
    Source,
    Allocator,
}
#[derive(Debug, Clone)]
pub struct Trait {
    pub type_params: Vec<Box<str>>,
    pub annotations: Vec<DeclarationAnnotation>,
    pub origin: TraitOrigin,
    pub name: Box<str>,
    pub methods: Vec<Function>,
    pub span: Span,
}
#[derive(Debug, Clone)]
pub struct Implementation {
    pub trait_name: Box<str>,
    pub receiver: Type,
    pub methods: Vec<Function>,
    pub span: Span,
}
#[derive(Debug, Clone)]
pub struct InherentImplementation {
    pub type_params: Vec<Box<str>>,
    pub receiver: Type,
    pub methods: Vec<InherentMethod>,
    pub span: Span,
}
#[derive(Debug, Clone)]
pub struct InherentMethod {
    pub public: bool,
    pub function: Function,
}
/// Trusted native headers retained by the compilation after namespace linking.
/// Source trust is checked by the loader/linker before populating this table.
#[derive(Debug, Clone, Default)]
pub struct NativeDeclarations {
    pub import_policy: Option<NativeImportPolicy>,
    pub types: Vec<NativeTypeDeclaration>,
    pub functions: Vec<NativeFunctionDeclaration>,
    pub methods: Vec<NativeMethodDeclaration>,
    pub traits: Vec<(DeclarationId, Trait)>,
    pub families: Vec<(DeclarationId, Record)>,
}
#[derive(Debug, Clone)]
pub struct NativeTypeDeclaration {
    pub id: DeclarationId,
    pub name: Box<str>,
    pub parameters: Vec<Box<str>>,
    pub annotation: DeclarationAnnotation,
}
#[derive(Debug, Clone)]
pub struct NativeFunctionDeclaration {
    pub id: DeclarationId,
    pub module: Option<DeclarationId>,
    pub function: Function,
}
#[derive(Debug, Clone)]
pub struct NativeMethodDeclaration {
    pub id: DeclarationId,
    pub module: Option<DeclarationId>,
    pub owner: DeclarationId,
    pub owner_parameters: Vec<Box<str>>,
    pub receiver: Type,
    pub function: Function,
    pub public: bool,
}
#[derive(Debug, Clone, Default)]
pub struct NativeImportPolicy {
    pub modules: Vec<NativeModuleIdentity>,
    pub access: Vec<NativeModuleAccess>,
    pub requesters: std::collections::HashMap<u32, DeclarationId>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceTrust {
    User,
    Core,
}
#[derive(Debug, Clone)]
pub struct NativeModuleIdentity {
    pub trust: SourceTrust,
    pub id: DeclarationId,
    pub path: Box<str>,
}
#[derive(Debug, Clone)]
pub struct NativeModuleAccess {
    pub referrer: DeclarationId,
    pub allowed: Vec<DeclarationId>,
}
#[derive(Debug, Clone)]
pub struct Property {
    pub receiver: Type,
    pub name: Box<str>,
    pub getter: Function,
    pub span: Span,
}
#[derive(Debug, Clone)]
pub struct Record {
    pub annotations: Vec<DeclarationAnnotation>,
    pub type_params: Vec<Box<str>>,
    pub name: Box<str>,
    pub fields: Vec<(Box<str>, Type)>,
    pub span: Span,
}

impl GetSpan for Expr {
    fn span(&self) -> Span {
        self.span
    }
}
impl GetSpan for Stmt {
    fn span(&self) -> Span {
        self.span
    }
}
impl GetSpan for Function {
    fn span(&self) -> Span {
        self.span
    }
}
impl GetSpan for Param {
    fn span(&self) -> Span {
        self.span
    }
}
/// One source owns its line index. Spans remain half-open UTF-8 byte ranges.
/// Spans are u32 byte offsets, so one source is limited to 4 GiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceTooLarge;
impl std::fmt::Display for SourceTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("source exceeds the 4 GiB span limit")
    }
}
impl std::error::Error for SourceTooLarge {}
#[derive(Debug, Clone)]
pub struct SourceFile {
    text: Box<str>,
    line_starts: Box<[u32]>,
}
impl SourceFile {
    pub fn new(text: String) -> Result<Self, SourceTooLarge> {
        if text.len() > u32::MAX as usize {
            return Err(SourceTooLarge);
        }
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push((i + 1) as u32);
            }
        }
        Ok(Self {
            text: text.into_boxed_str(),
            line_starts: line_starts.into_boxed_slice(),
        })
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn slice(&self, span: Span) -> Option<&str> {
        self.text.get(span.start as usize..span.end as usize)
    }
    /// One-based line and Unicode scalar column, independent of byte offsets.
    pub fn location(&self, offset: u32) -> Option<(usize, usize)> {
        let offset = offset as usize;
        if !self.text.is_char_boundary(offset) {
            return None;
        }
        let line = self
            .line_starts
            .partition_point(|s| *s as usize <= offset)
            .checked_sub(1)?;
        Some((
            line + 1,
            self.text[self.line_starts[line] as usize..offset]
                .chars()
                .count()
                + 1,
        ))
    }
}

#[derive(Debug, Clone)]
pub struct CaseLabel {
    pub lower: Expr,
    pub upper: Option<Expr>,
}
impl GetSpan for CaseLabel {
    fn span(&self) -> Span {
        self.upper
            .as_ref()
            .map_or(self.lower.span, |e| self.lower.span.merge(e.span))
    }
}
impl GetSpanMut for Expr {
    fn span_mut(&mut self) -> &mut Span {
        &mut self.span
    }
}
impl GetSpanMut for Stmt {
    fn span_mut(&mut self) -> &mut Span {
        &mut self.span
    }
}
impl GetSpanMut for Function {
    fn span_mut(&mut self) -> &mut Span {
        &mut self.span
    }
}
impl GetSpanMut for Param {
    fn span_mut(&mut self) -> &mut Span {
        &mut self.span
    }
}

pub mod spans;
pub mod visit;
/// Runs one level of a recursive syntax pass, growing the stack first if it
/// is nearly exhausted. Every pass that recurses over the AST calls this at
/// its expression and block descent, so syntax nesting is limited only by the
/// parser's explicit depth limit, never by the caller's thread stack.
pub fn with_stack<R>(f: impl FnOnce() -> R) -> R {
    const RED_ZONE: usize = 256 * 1024;
    const SEGMENT: usize = 4 * 1024 * 1024;
    stacker::maybe_grow(RED_ZONE, SEGMENT, f)
}
pub type Callable<'a> = (RoutineKind, &'a [(Type, bool)], &'a Type);
pub fn callable(ty: &Type) -> Option<Callable<'_>> {
    match ty {
        Type::Function(k, p, r) => Some((*k, p, r)),
        Type::Borrowed(t) | Type::Reference { pointee: t, .. } => callable(t),
        _ => None,
    }
}
pub fn constant_text(expr: &Expr) -> Option<&str> {
    match &expr.kind {
        ExprKind::Literal(Literal::String(s)) => Some(s),
        ExprKind::Frozen(v, _) => match v.as_ref() {
            ConstantValue::String(s) => Some(s),
            _ => None,
        },
        _ => None,
    }
}
