//! Backend-independent checked IR. Construction is private to the compiler.
pub use crate::effects::EffectSummary;
pub use cyallo_ast::RoutineKind;
use cyallo_ast::{Literal, Span, Type};
pub type Reg = usize;
#[derive(Debug, Clone)]
pub struct Local {
    pub display_name: Option<Box<str>>,
    pub name: Box<str>,
    pub ty: Type,
    pub constant: bool,
    pub span: Span,
}
#[derive(Debug, Clone, Copy)]
pub struct Slot {
    pub global: bool,
    pub index: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalStorage {
    Cell(usize),
    Register(Reg),
}
/// How an output operation spells values. Modern `print` uses Rust-like text;
/// CAIE `OUTPUT` keeps the exam spelling of booleans (`TRUE`, `FALSE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextStyle {
    Plain,
    Caie,
}
#[derive(Debug, Clone)]
pub enum Op {
    Unreachable,
    EnumConstruct(Reg, cyallo_ast::EnumVariant, Option<Reg>),
    EnumTest(Reg, Reg, cyallo_ast::EnumVariant),
    EnumPayload(
        Reg,
        Reg,
        cyallo_ast::EnumVariant,
        crate::enums::Projection,
        // Boxed: an inline `Type` made this the largest `Op` and grew every instruction.
        Box<Type>,
    ),
    Dynamic(Reg, crate::dynamic::Operation, Vec<Reg>),
    Yield(Reg, Reg),
    Frozen(Reg, std::sync::Arc<cyallo_ast::ConstantValue>),
    Statement,
    Nop,
    Lookup(Reg, crate::lookup::Operation, Vec<Reg>),
    ArrayLiteral(Reg, Vec<Reg>),
    Aggregate(Reg, Vec<Reg>),
    Field(Reg, Reg, usize),
    Extract(Reg, Reg, usize),
    FunctionValue(Reg, usize, Vec<Reg>),
    Invoke(Option<Reg>, Reg, Vec<Reg>),
    ServiceKey(Reg, usize),
    Service(Reg, crate::services::Operation, Vec<Reg>),
    TraitBox(Reg, Reg, usize),
    TraitCall(Option<Reg>, Reg, usize, Vec<Reg>),
    Length(Reg, Reg),
    Literal(Reg, Literal),
    LiteralString(Reg, std::sync::Arc<str>),
    Address(Reg, Slot),
    ManagedAddress(Reg, Reg),
    Index(Reg, Reg, Vec<Reg>, bool),
    Load(Reg, Reg),
    Take(Reg, Reg),
    Drop(Reg),
    Store(Reg, Reg),
    FrameLoad(Reg, Reg),
    FrameStore(Reg, Reg),
    Unary(Reg, cyallo_ast::Operator, Reg),
    Binary(Reg, cyallo_ast::Operator, Reg, Reg),
    Move(Reg, Reg),
    Jump(usize),
    Branch(Reg, usize, usize),
    Call(Option<Reg>, usize, Vec<Reg>),
    Builtin(Reg, crate::intrinsic::Intrinsic, Vec<Reg>),
    Output(Vec<Reg>, TextStyle),
    Input(Reg, Type),
    FrameInput(Reg, Type),
    Return(Option<Reg>),
    CheckStep(Reg),
}
#[derive(Debug, Clone)]
pub struct Instruction {
    pub op: Op,
    pub span: Span,
}
#[derive(Debug, Clone)]
pub struct Function {
    pub generator: Option<Type>,
    pub symbol: Box<str>,
    pub origin: cyallo_ast::RoutineOrigin,
    pub attributes: cyallo_ast::RoutineAttributes,
    pub analysis_error: Option<crate::Diagnostic>,
    pub resume: crate::osr::ResumeMetadata,
    pub gc: crate::gc_metadata::GcMetadata,
    pub kind: RoutineKind,
    pub span: Span,
    pub pure: bool,
    pub effects: EffectSummary,
    pub name: Box<str>,
    pub locals: Vec<Local>,
    pub local_storage: Box<[LocalStorage]>,
    pub params: Vec<bool>,
    pub result: Type,
    pub registers: Vec<Type>,
    pub code: Vec<Instruction>,
}
impl Function {
    pub fn cell_slot(&self, local: usize) -> usize {
        match self.local_storage[local] {
            LocalStorage::Cell(index) => index,
            LocalStorage::Register(_) => unreachable!("frame registers have no cell address"),
        }
    }
}

/// Cheap, analysis-free facts used to choose a compilation tier.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RoutineShape {
    pub loops: bool,
    pub calls: bool,
}
/// A declaration is cheap to inspect. Body lowering is initialized once, shared
/// by sessions and compiler workers, and never mutates after publication.
#[derive(Debug, Clone)]
pub struct Routine {
    pub generator: Option<Type>,
    pub symbol: Box<str>,
    pub attributes: cyallo_ast::RoutineAttributes,
    pub estimated_words: usize,
    pub direct_calls: Box<[usize]>,
    pub name: Box<str>,
    pub kind: RoutineKind,
    pub span: Span,
    pub params: Vec<bool>,
    pub result: Type,
    pub eager_candidate: bool,
    pub shape: RoutineShape,
    body: std::sync::Arc<RoutineBody>,
}
#[derive(Debug)]
struct RoutineBody {
    checked: std::sync::OnceLock<Function>,
    deferred: Option<crate::analysis::DeferredBody>,
    relocated: Option<crate::linking::RelocatedBody>,
}
impl Routine {
    pub fn relocated(
        source: Routine,
        relocation: std::sync::Arc<crate::linking::Relocation>,
        entry: bool,
    ) -> Self {
        let mut routine = source.clone();
        routine.span = relocation.span(source.span);
        for span in [
            &mut routine.attributes.heat_span,
            &mut routine.attributes.compilation_span,
            &mut routine.attributes.inline_span,
        ]
        .into_iter()
        .flatten()
        {
            *span = relocation.span(*span);
        }
        routine.direct_calls = relocation.header_calls(&source.direct_calls);
        routine.body = std::sync::Arc::new(RoutineBody {
            checked: Default::default(),
            deferred: None,
            relocated: Some(crate::linking::RelocatedBody {
                source,
                relocation,
                entry,
            }),
        });
        routine
    }
    pub fn ready(function: Function) -> Self {
        let shape = RoutineShape {
            loops: !function.resume.loop_headers.is_empty(),
            calls: function.code.iter().any(|i| {
                matches!(
                    i.op,
                    Op::Call(..)
                        | Op::Invoke(..)
                        | Op::Dynamic(_, crate::dynamic::Operation::Invoke, _)
                        | Op::TraitCall(..)
                        | Op::Service(..)
                        | Op::Builtin(..)
                )
            }),
        };
        Self {
            generator: function.generator.clone(),
            symbol: function.symbol.clone(),
            attributes: function.attributes,
            estimated_words: function.code.len(),
            direct_calls: function
                .code
                .iter()
                .filter_map(|i| {
                    if let Op::Call(_, to, _) = i.op {
                        Some(to)
                    } else {
                        None
                    }
                })
                .collect(),
            name: function.name.clone(),
            kind: function.kind,
            span: function.span,
            params: function.params.clone(),
            result: function.result.clone(),
            eager_candidate: function.code.len() <= 256 && (shape.loops || shape.calls),
            shape,
            body: std::sync::Arc::new(RoutineBody {
                checked: std::sync::OnceLock::from(function),
                deferred: None,
                relocated: None,
            }),
        }
    }
    pub fn deferred(
        body: crate::analysis::DeferredBody,
        eager_candidate: bool,
        shape: RoutineShape,
        estimated_words: usize,
        direct_calls: Box<[usize]>,
    ) -> Self {
        let f = &body.function;
        Self {
            generator: f.generator.then(|| crate::generator::factory_type(f)),
            symbol: f.name.clone(),
            attributes: f.attributes,
            estimated_words,
            direct_calls,
            name: f.source_name.clone(),
            kind: f.kind,
            span: f.span,
            params: f.params.iter().map(|p| p.by_ref).collect(),
            result: f.result.clone(),
            eager_candidate,
            shape,
            body: std::sync::Arc::new(RoutineBody {
                checked: Default::default(),
                deferred: Some(body),
                relocated: None,
            }),
        }
    }
    pub fn is_analyzed(&self) -> bool {
        self.body.checked.get().is_some()
    }
    pub fn checked(&self) -> Result<&Function, &crate::Diagnostic> {
        let body: &Function = self;
        match &body.analysis_error {
            Some(error) => Err(error),
            None => Ok(body),
        }
    }
}
impl std::ops::Deref for Routine {
    type Target = Function;
    fn deref(&self) -> &Function {
        self.body.checked.get_or_init(|| {
            if let Some(body) = &self.body.relocated {
                return body.analyze();
            }
            self.body
                .deferred
                .as_ref()
                .expect("deferred body")
                .analyze()
        })
    }
}

#[derive(Debug, Clone)]
pub struct Module {
    pub records: std::sync::Arc<std::collections::HashMap<String, Type>>,
    pub requires_tracing: bool,
    pub functions: Vec<Routine>,
    pub services: std::sync::Arc<crate::services::Metadata>,
}
impl cyallo_ast::GetSpan for Instruction {
    fn span(&self) -> Span {
        self.span
    }
}
