//! Immutable layouts and property contracts in one explicitly owned domain.
use cyallo_ast::{NumberKind, Type};
use std::{collections::HashMap, fmt, sync::Arc};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PropertyId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShapeId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Mutable,
    Readonly,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Contract {
    Required(Type),
    Configurable,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Representation {
    Number(NumberKind),
    Boolean,
    Char,
    String,
    Object,
    Callable,
    Other,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    pub id: PropertyId,
    pub contract: Contract,
    pub representation: Representation,
}
#[derive(Debug)]
pub struct Shape {
    pub access: Access,
    pub fields: Box<[Property]>,
    lookup: Lookup,
}
#[derive(Debug)]
enum Lookup {
    Linear,
    Sorted(Box<[(PropertyId, usize)]>),
}
impl Shape {
    pub fn slot(&self, key: PropertyId) -> Option<usize> {
        match &self.lookup {
            Lookup::Linear => self.fields.iter().position(|field| field.id == key),
            Lookup::Sorted(index) => index
                .binary_search_by_key(&key.0, |(id, _)| id.0)
                .ok()
                .map(|i| index[i].1),
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub enum DynamicError {
    MissingProperty(Box<str>),
    Readonly,
    RequiredProperty(Box<str>),
    ExpectedObject(Type),
    ExpectedCallable(Type),
    ExpectedKey(Type),
    Type {
        expected: Type,
        actual: Type,
    },
    ArgumentCount {
        expected: usize,
        actual: usize,
    },
    BorrowedValue,
    BorrowedCall,
    ShapeLimit,
    PropertyLimit,
    Operator {
        operator: cyallo_ast::Operator,
        left: Type,
        right: Option<Type>,
    },
    CallCheck(cyallo_ast::Diagnostic),
    Call(Box<cyallo_modules::LoadError>),
}
impl fmt::Display for DynamicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingProperty(name) => write!(f, "object has no property `{name}`"),
            Self::Readonly => f.write_str("object is readonly"),
            Self::RequiredProperty(name) => write!(f, "cannot delete required property `{name}`"),
            Self::ExpectedObject(ty) => write!(f, "expected an object, got {ty}"),
            Self::ExpectedCallable(ty) => write!(f, "value of type {ty} is not callable"),
            Self::ExpectedKey(ty) => write!(f, "property key must be a string, got {ty}"),
            Self::Type { expected, actual } => write!(
                f,
                "dynamic type mismatch: expected {expected}, got {actual}"
            ),
            Self::ArgumentCount { expected, actual } => write!(
                f,
                "dynamic call expected {expected} arguments, got {actual}"
            ),
            Self::BorrowedValue => {
                f.write_str("borrowed values cannot escape into owned dynamic storage")
            }
            Self::BorrowedCall => f.write_str("dynamic calls require owned argument contracts"),
            Self::ShapeLimit => f.write_str("object shape limit exceeded"),
            Self::PropertyLimit => f.write_str("property identity limit exceeded"),
            Self::Operator {
                operator,
                left,
                right: Some(right),
            } => write!(
                f,
                "dynamic operator {operator} does not accept {left} and {right}"
            ),
            Self::Operator {
                operator,
                left,
                right: None,
            } => write!(f, "dynamic operator {operator} does not accept {left}"),
            Self::CallCheck(error) => write!(f, "dynamic call failed checking: {error}"),
            Self::Call(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for DynamicError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CallCheck(error) => Some(error),
            Self::Call(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}
#[derive(Debug, Default)]
pub struct Domain {
    names: HashMap<Box<str>, PropertyId>,
    labels: Vec<Box<str>>,
    shapes: Vec<Arc<Shape>>,
}
impl Domain {
    pub fn name(&self, id: PropertyId) -> &str {
        &self.labels[id.0 as usize]
    }
    pub fn property(&mut self, name: &str) -> Result<PropertyId, DynamicError> {
        if let Some(id) = self.names.get(name) {
            return Ok(*id);
        }
        let id =
            PropertyId(u32::try_from(self.names.len()).map_err(|_| DynamicError::PropertyLimit)?);
        self.names.insert(name.into(), id);
        self.labels.push(name.into());
        Ok(id)
    }
    pub fn shape(
        &mut self,
        fields: Vec<Property>,
        access: Access,
    ) -> Result<ShapeId, DynamicError> {
        if let Some(index) = self
            .shapes
            .iter()
            .position(|shape| shape.access == access && shape.fields.as_ref() == fields)
        {
            return Ok(ShapeId(index as u32));
        }
        let id = ShapeId(u32::try_from(self.shapes.len()).map_err(|_| DynamicError::ShapeLimit)?);
        let lookup = if fields.len() < 8 {
            Lookup::Linear
        } else {
            let mut index: Vec<_> = fields.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
            index.sort_by_key(|(id, _)| id.0);
            Lookup::Sorted(index.into_boxed_slice())
        };
        self.shapes.push(Arc::new(Shape {
            access,
            fields: fields.into_boxed_slice(),
            lookup,
        }));
        Ok(id)
    }
    pub fn get(&self, id: ShapeId) -> Arc<Shape> {
        self.shapes[id.0 as usize].clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sorted_lookup_rejects_unknown_keys_and_retains_exact_spelling() {
        let mut domain = Domain::default();
        let names = ["x", "X", "small", "large", "a", "b", "c", "d", "e"];
        let fields = names
            .iter()
            .map(|name| Property {
                id: domain.property(name).unwrap(),
                contract: Contract::Configurable,
                representation: Representation::Boolean,
            })
            .collect();
        let id = domain.shape(fields, Access::Mutable).unwrap();
        let shape = domain.get(id);
        for (index, name) in names.iter().enumerate() {
            assert_eq!(shape.slot(domain.property(name).unwrap()), Some(index));
        }
        assert_eq!(shape.slot(domain.property("unknown").unwrap()), None);
        assert_eq!(
            domain
                .shape(shape.fields.to_vec(), Access::Mutable)
                .unwrap(),
            id
        );
    }
    #[test]
    fn transitions_never_change_an_existing_shape() {
        let mut domain = Domain::default();
        let key = domain.property("field").unwrap();
        let first = domain
            .shape(
                vec![Property {
                    id: key,
                    contract: Contract::Configurable,
                    representation: Representation::Boolean,
                }],
                Access::Mutable,
            )
            .unwrap();
        let second = domain
            .shape(
                vec![Property {
                    id: key,
                    contract: Contract::Required(Type::String),
                    representation: Representation::String,
                }],
                Access::Readonly,
            )
            .unwrap();
        assert_ne!(first, second);
        assert_eq!(domain.get(first).access, Access::Mutable);
        assert_eq!(domain.get(first).fields[0].contract, Contract::Configurable);
    }
}
