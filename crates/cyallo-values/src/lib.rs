//! Tagged values and relocatable Session cell indices. No NaN boxing.
use cyallo_ast::Literal;
use std::collections::BTreeMap;
#[derive(Debug, Clone, PartialEq)]
pub enum GeneratorStep<T> { Yielded(T), Complete(T) }
impl<T> GeneratorStep<T> {
    pub fn value(&self) -> &T { match self { Self::Yielded(value) | Self::Complete(value) => value } }
    pub fn value_mut(&mut self) -> &mut T { match self { Self::Yielded(value) | Self::Complete(value) => value } }
    pub fn is_complete(&self) -> bool { matches!(self, Self::Complete(_)) }
}
impl<T: std::fmt::Display> std::fmt::Display for GeneratorStep<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { match self {
        Self::Yielded(value) => write!(f, "Yielded({value})"), Self::Complete(value) => write!(f, "Complete({value})"),
    } }
}
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Concurrent(cyallo_concurrent_values::Handle),
    TaskOutcome(cyallo_concurrent_values::TaskOutcome<Box<Value>>),
    SendError(cyallo_concurrent_values::SendError<Box<Value>>),
    Generator(usize),
    GeneratorStep(GeneratorStep<Box<Value>>),
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
    Range(cyallo_ast::Number, cyallo_ast::Number, bool),
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
    Optional(Option<Box<Value>>),
    Result(Result<Box<Value>, Box<Value>>),
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
impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Concurrent(handle) => write!(f, "{handle:?}"),
            Self::TaskOutcome(outcome) => match outcome {
                cyallo_concurrent_values::TaskOutcome::Completed(value) => write!(f, "Completed({value})"),
                cyallo_concurrent_values::TaskOutcome::Failed(error) => write!(f, "Failed({error})"),
                cyallo_concurrent_values::TaskOutcome::Cancelled => f.write_str("Cancelled"),
                cyallo_concurrent_values::TaskOutcome::Stopped => f.write_str("Stopped"),
            },
            Self::SendError(error) => write!(f, "SendError({}, {})", error.kind, error.value),
            Self::Generator(id) => write!(f, "<generator {id}>"),
            Self::GeneratorStep(step) => write!(f, "{step}"),
            Self::Layout { size, align } => write!(f, "Layout({size}, {align})"),
            Self::Allocation(id) => write!(f, "<allocation {id}>"),
            Self::AllocError(message) => write!(f, "{message}"),
            Self::Number(n) => write!(f, "{n}"),
            Self::Integer(v) => write!(f, "{v}"),
            Self::Real(v) => write!(f, "{v}"),
            Self::Boolean(v) => write!(f, "{}", if *v { "TRUE" } else { "FALSE" }),
            Self::Char(v) => write!(f, "{v}"),
            Self::String(v) => write!(f, "{v}"),
            Self::Array(v) => write!(f, "<array: {} elements>", v.len()),
            Self::Deque(v) => write!(f, "<deque: {} elements>", v.len()),
            Self::Range(a, b, inclusive) => {
                write!(f, "{a}..{}{b}", if *inclusive { "=" } else { "" })
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
            Self::Optional(Some(v)) => write!(f, "{v}"),
            Self::Optional(None) => write!(f, "<none>"),
            Self::Result(Ok(value)) => write!(f, "Ok({value})"),
            Self::Result(Err(error)) => write!(f, "Err({error})"),
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
