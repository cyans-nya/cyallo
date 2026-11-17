//! Tagged values and relocatable Session cell indices. No NaN boxing.
use cyallo_ast::Literal;
pub use cyallo_concurrent_values::GeneratorStep;
use std::collections::BTreeMap;
/// A range of integers. `reversed` iterates from the end towards the start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RangeValue {
    pub start: cyallo_ast::Number,
    pub end: cyallo_ast::Number,
    pub inclusive: bool,
    pub reversed: bool,
}
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    NativeEnum(cyallo_ast::EnumVariant,Option<Box<Value>>),
    NativeEnumStorage(cyallo_ast::EnumVariant, Option<usize>),
    Enum(cyallo_ast::VariantId, Option<Box<Value>>),
    EnumStorage(cyallo_ast::VariantId, Option<usize>),
    TypeDescriptor(cyallo_ast::ExportedType),
    DeferredCallable(Box<str>),
    Dynamic(cyallo_ast::Type, Box<Value>),
    Object(usize),
    ObjectStorage {
        shape: cyallo_shapes::ShapeId,
        slots: Vec<Value>,
    },
    ViewError(std::sync::Arc<cyallo_shapes::DynamicError>),
    NativeCallable(cyallo_ast::DeclarationId),
    Concurrent(cyallo_concurrent_values::Handle),
    TaskOutcome(cyallo_concurrent_values::TaskOutcome<Box<Value>>),
    LoadError(std::sync::Arc<cyallo_modules::LoadError>),
    Generator(usize),
    Layout {
        size: usize,
        align: usize,
    },
    Allocation(usize),
    AllocError(std::sync::Arc<str>),
    Number(cyallo_ast::Number),
    Integer(i64),
    Real(f64),
    Boolean(bool),
    Char(char),
    String(std::sync::Arc<str>),
    Array(Vec<usize>),
    Deque(std::collections::VecDeque<usize>),
    Range(RangeValue),
    ArraySnapshot(Vec<Value>),
    SparseArray(BTreeMap<u32, usize>, u64),
    SparseSnapshot(BTreeMap<u32, Value>, u64),
    Function(usize, Vec<Value>),
    Context(usize),
    Registration(usize),
    ServiceKey(usize),
    ServiceRef(usize, usize),
    TraitObject(usize, usize),
    Address(usize),
    /// Borrowed data plus runtime extent. The owner is relocatable storage.
    Slice {
        owner: usize,
        offset: usize,
        length: usize,
    },
    Managed(usize),
    Cell(Box<Value>),
    Rc(usize),
    Weak(usize),
    Symbol(u64),
    ParseError(std::sync::Arc<str>),
    Erased(cyallo_ast::Type, Box<Value>),
    StoragePath(Box<Value>, Vec<usize>),
    MissingStoragePath,
    Uninitialized,
}
impl Value {
    pub fn from_number(n: cyallo_ast::Number) -> Self {
        match n.kind {
            cyallo_ast::NumberKind::I64 => Self::Integer(n.int() as i64),
            cyallo_ast::NumberKind::F64 => Self::Real(n.real()),
            _ => Self::Number(n),
        }
    }
    pub fn as_number(&self) -> Option<cyallo_ast::Number> {
        use cyallo_ast::{Number, NumberKind};
        match self {
            Self::Number(n) => Some(*n),
            Self::Integer(n) => Some(Number {
                kind: NumberKind::I64,
                bits: *n as u64,
            }),
            Self::Real(n) => Some(Number {
                kind: NumberKind::F64,
                bits: n.to_bits(),
            }),
            _ => None,
        }
    }
}
/// Constructors and views for the `core` `Option` and `Result` enums, whose variants
/// have reserved ids.
impl Value {
    pub fn some(value: Value) -> Self {
        Self::Enum(cyallo_ast::VariantId::OPTION_SOME, Some(Box::new(value)))
    }
    pub fn none() -> Self {
        Self::Enum(cyallo_ast::VariantId::OPTION_NONE, None)
    }
    pub fn option(value: Option<Box<Value>>) -> Self {
        match value {
            Some(value) => Self::Enum(cyallo_ast::VariantId::OPTION_SOME, Some(value)),
            None => Self::none(),
        }
    }
    pub fn result(value: Result<Box<Value>, Box<Value>>) -> Self {
        match value {
            Ok(value) => Self::Enum(cyallo_ast::VariantId::RESULT_OK, Some(value)),
            Err(error) => Self::Enum(cyallo_ast::VariantId::RESULT_ERR, Some(error)),
        }
    }
    pub fn ok(value: Value) -> Self {
        Self::result(Ok(Box::new(value)))
    }
    pub fn err(error: Value) -> Self {
        Self::result(Err(Box::new(error)))
    }
    /// `Some(payload)` for `Option::Some`, `None` for `Option::None`; the outer
    /// `None` means the value is not an option.
    pub fn as_option(&self) -> Option<Option<&Value>> {
        match self {
            Self::Enum(variant, Some(value)) if *variant == cyallo_ast::VariantId::OPTION_SOME => {
                Some(Some(value))
            }
            Self::Enum(variant, None) if *variant == cyallo_ast::VariantId::OPTION_NONE => {
                Some(None)
            }
            _ => None,
        }
    }
    pub fn as_result(&self) -> Option<Result<&Value, &Value>> {
        match self {
            Self::Enum(variant, Some(value)) if *variant == cyallo_ast::VariantId::RESULT_OK => {
                Some(Ok(value))
            }
            Self::Enum(variant, Some(value)) if *variant == cyallo_ast::VariantId::RESULT_ERR => {
                Some(Err(value))
            }
            _ => None,
        }
    }
    pub fn step(step: GeneratorStep<Box<Value>>) -> Self {
        match step {
            GeneratorStep::Yielded(value) => {
                Self::Enum(cyallo_ast::VariantId::STEP_YIELDED, Some(value))
            }
            GeneratorStep::Complete(value) => {
                Self::Enum(cyallo_ast::VariantId::STEP_COMPLETE, Some(value))
            }
        }
    }
    pub fn as_step(&self) -> Option<GeneratorStep<&Value>> {
        match self {
            Self::Enum(variant, Some(value)) if *variant == cyallo_ast::VariantId::STEP_YIELDED => {
                Some(GeneratorStep::Yielded(value))
            }
            Self::Enum(variant, Some(value)) if *variant == cyallo_ast::VariantId::STEP_COMPLETE => {
                Some(GeneratorStep::Complete(value))
            }
            _ => None,
        }
    }
    pub fn send_error(kind: cyallo_concurrent_values::SendFailure, value: Box<Value>) -> Self {
        use cyallo_concurrent_values::SendFailure::*;
        Self::Enum(
            match kind {
                Closed => cyallo_ast::VariantId::SEND_CLOSED,
                Full => cyallo_ast::VariantId::SEND_FULL,
                Limit => cyallo_ast::VariantId::SEND_LIMIT,
            },
            Some(value),
        )
    }
    pub fn as_send_error(&self) -> Option<(cyallo_concurrent_values::SendFailure, &Value)> {
        use cyallo_concurrent_values::SendFailure::*;
        let Self::Enum(variant, Some(value)) = self else {
            return None;
        };
        let kind = match *variant {
            v if v == cyallo_ast::VariantId::SEND_CLOSED => Closed,
            v if v == cyallo_ast::VariantId::SEND_FULL => Full,
            v if v == cyallo_ast::VariantId::SEND_LIMIT => Limit,
            _ => return None,
        };
        Some((kind, value))
    }
    pub fn into_option(self) -> Result<Option<Box<Value>>, Value> {
        match self {
            Self::Enum(variant, payload) if variant == cyallo_ast::VariantId::OPTION_SOME && payload.is_some() => Ok(payload),
            Self::Enum(variant, None) if variant == cyallo_ast::VariantId::OPTION_NONE => Ok(None),
            other => Err(other),
        }
    }
    pub fn into_result(self) -> Result<Result<Box<Value>, Box<Value>>, Value> {
        match self {
            Self::Enum(variant, Some(value)) if variant == cyallo_ast::VariantId::RESULT_OK => Ok(Ok(value)),
            Self::Enum(variant, Some(value)) if variant == cyallo_ast::VariantId::RESULT_ERR => Ok(Err(value)),
            other => Err(other),
        }
    }
}
impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Enum(variant, Some(v)) if *variant == cyallo_ast::VariantId::OPTION_SOME => {
                write!(f, "{v}")
            }
            Self::Enum(variant, None) if *variant == cyallo_ast::VariantId::OPTION_NONE => {
                write!(f, "<none>")
            }
            Self::Enum(variant, Some(v)) if *variant == cyallo_ast::VariantId::RESULT_OK => {
                write!(f, "Ok({v})")
            }
            Self::Enum(variant, Some(v)) if *variant == cyallo_ast::VariantId::RESULT_ERR => {
                write!(f, "Err({v})")
            }
            Self::Enum(variant, Some(v)) if *variant == cyallo_ast::VariantId::STEP_YIELDED => {
                write!(f, "Yielded({v})")
            }
            Self::Enum(variant, Some(v)) if *variant == cyallo_ast::VariantId::STEP_COMPLETE => {
                write!(f, "Complete({v})")
            }
            Self::Enum(_, Some(v)) if self.as_send_error().is_some() => {
                write!(f, "SendError({}, {v})", self.as_send_error().unwrap().0)
            }
            Self::Enum(variant, payload) => {
                write!(f, "<variant {}", variant.0.0)?;
                if let Some(payload) = payload {
                    write!(f, ": {payload}")?;
                }
                f.write_str(">")
            }
            Self::NativeEnum(variant,payload)=>{write!(f,"<variant {}",variant.declaration.0.0)?;if let Some(payload)=payload{write!(f,": {payload}")?;}f.write_str(">")},
            Self::NativeEnumStorage(variant, _) => {
                write!(f, "<stored variant {}>", variant.declaration.0.0)
            }
            Self::EnumStorage(variant, _) => write!(f, "<stored variant {}>", variant.0.0),
            Self::NativeCallable(_) => f.write_str("<native callable>"),
            Self::Concurrent(handle) => write!(f, "{handle:?}"),
            Self::TypeDescriptor(descriptor) => write!(f, "type {}", descriptor.name),
            Self::DeferredCallable(_) => f.write_str("generic callable"),
            Self::Dynamic(_, value) => value.fmt(f),
            Self::Object(_) => f.write_str("object"),
            Self::ObjectStorage { .. } => f.write_str("object storage"),
            Self::ViewError(error) => error.fmt(f),
            Self::TaskOutcome(outcome) => match outcome {
                cyallo_concurrent_values::TaskOutcome::Completed(value) => {
                    write!(f, "Completed({value})")
                }
                cyallo_concurrent_values::TaskOutcome::Failed(error) => {
                    write!(f, "Failed({error})")
                }
                cyallo_concurrent_values::TaskOutcome::Cancelled => f.write_str("Cancelled"),
                cyallo_concurrent_values::TaskOutcome::Stopped => f.write_str("Stopped"),
            },
            Self::LoadError(error) => error.fmt(f),
            Self::Generator(id) => write!(f, "<generator {id}>"),
            Self::Layout { size, align } => write!(f, "Layout({size}, {align})"),
            Self::Allocation(id) => write!(f, "<allocation {id}>"),
            Self::AllocError(message) => write!(f, "{message}"),
            Self::Number(n) => write!(f, "{n}"),
            Self::Integer(v) => write!(f, "{v}"),
            Self::Real(v) => write!(f, "{v}"),
            Self::Boolean(v) => write!(f, "{v}"),
            Self::Char(v) => write!(f, "{v}"),
            Self::String(v) => write!(f, "{v}"),
            Self::Array(v) => write!(f, "<array: {} elements>", v.len()),
            Self::Deque(v) => write!(f, "<deque: {} elements>", v.len()),
            Self::Range(range) => {
                let operator = if range.inclusive { "..=" } else { ".." };
                if range.reversed {
                    write!(f, "({}{operator}{}).rev()", range.start, range.end)
                } else {
                    write!(f, "{}{operator}{}", range.start, range.end)
                }
            }
            Self::ArraySnapshot(v) => write!(f, "<array: {} elements>", v.len()),
            Self::SparseArray(_, length) | Self::SparseSnapshot(_, length) => {
                write!(f, "<sparse array: length {length}>")
            }
            Self::Address(v) => write!(f, "<address {v}>"),
            Self::Slice { length, .. } => write!(f, "<slice: {length} elements>"),
            Self::Cell(_) => write!(f, "<cell>"),
            Self::Managed(v) => write!(f, "<location {v}>"),
            Self::Rc(v) => write!(f, "<rc owner {v}>"),
            Self::Weak(v) => write!(f, "<weak owner {v}>"),
            Self::Symbol(v) => write!(f, "<symbol {v}>"),
            Self::ParseError(message) => write!(f, "{message}"),
            Self::Erased(_, v) => write!(f, "{v}"),
            Self::StoragePath(..) | Self::MissingStoragePath => write!(f, "<storage path>"),
            Self::Function(id, _) => write!(f, "<function {id}>"),
            Self::Context(id) => write!(f, "<context {id}>"),
            Self::Registration(id) => write!(f, "<registration {id}>"),
            Self::ServiceKey(id) => write!(f, "<service key {id}>"),
            Self::ServiceRef(view, key) => write!(f, "<service {view}:{key}>"),
            Self::TraitObject(_, implementation) => {
                write!(f, "<trait implementation {implementation}>")
            }
            Self::Uninitialized => write!(f, "<uninitialized>"),
        }
    }
}
impl From<Literal> for Value {
    fn from(l: Literal) -> Self {
        match l {
            Literal::Number(n) => Self::Number(n),
            Literal::Integer(v) => Self::Integer(v),
            Literal::Real(v) => Self::Real(v),
            Literal::Boolean(v) => Self::Boolean(v),
            Literal::Char(v) => Self::Char(v),
            Literal::String(v) => Self::String(v.into()),
        }
    }
}
impl From<&cyallo_ast::ConstantValue> for Value {
    fn from(value: &cyallo_ast::ConstantValue) -> Self {
        use cyallo_ast::ConstantValue as C;
        match value {
            C::Number(n) => Self::Number(*n),
            C::Integer(v) => Self::Integer(*v),
            C::Real(v) => Self::Real(*v),
            C::Boolean(v) => Self::Boolean(*v),
            C::Char(v) => Self::Char(*v),
            C::String(v) => Self::String(v.clone()),
            C::Array(values) => Self::ArraySnapshot(values.iter().map(Self::from).collect()),
            C::Sparse(values, length) => Self::SparseSnapshot(
                values.iter().map(|(i, v)| (*i, Self::from(v))).collect(),
                *length,
            ),
        }
    }
}
