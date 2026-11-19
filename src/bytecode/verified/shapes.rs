//! Interned composite shapes for checked IR types. A shape is keyed by the
//! complete checked type, so two enums with the same structure but different
//! declarations stay distinct and the verifier compares them by identity.

use super::{ManagedId, ScalarType, Shape, ShapeId, Signature, SignatureId, ValueType, array_len};
use crate::Type;
use cyallo_ast::{EnumIdentity, Locality, MemoryKind, VariantId};
use std::collections::HashMap;

/// The module-level tables an interner produced.
pub(super) struct Tables {
    pub shapes: Box<[Shape]>,
    pub signatures: Box<[Signature]>,
    pub managed: Box<[ValueType]>,
}

#[derive(Default)]
pub(super) struct Shapes {
    by_type: HashMap<Type, ShapeId>,
    shapes: Vec<Shape>,
    /// Variant declarations of each enum shape, in variant-index order.
    variants: HashMap<ShapeId, Vec<VariantId>>,
    signatures: Vec<Signature>,
    managed: Vec<ValueType>,
    /// Enums being interned or interned, for references back to them.
    enum_ids: HashMap<EnumIdentity, ShapeId>,
    /// Tuples and arrays are structural: equal components, equal shape.
    structural: HashMap<Shape, ShapeId>,
    /// Records are nominal: the same name with equal field types.
    records: HashMap<(Box<str>, Box<[ValueType]>), ShapeId>,
}

impl Shapes {
    /// The verified type of a checked type, or `None` while it has no verified
    /// representation. Components are interned first, which numbers them below
    /// the composite that contains them.
    pub fn value(&mut self, ty: &Type) -> Option<ValueType> {
        Some(match ty {
            Type::Integer => ScalarType::I64.into(),
            Type::Real => ScalarType::F64.into(),
            Type::String => ValueType::Text,
            Type::Boolean => ScalarType::Bool.into(),
            Type::Function(_, parameters, result) => {
                if parameters.iter().any(|&(_, by_reference)| by_reference) {
                    return None;
                }
                let parameters = parameters
                    .iter()
                    .map(|(ty, _)| self.value(ty))
                    .collect::<Option<Box<[_]>>>()?;
                let result = match &**result {
                    Type::Void => None,
                    other => Some(self.value(other)?),
                };
                let signature = Signature { parameters, result };
                let index = match self.signatures.iter().position(|known| *known == signature) {
                    Some(index) => index,
                    None => {
                        self.signatures.push(signature);
                        self.signatures.len() - 1
                    }
                };
                ValueType::Function(SignatureId(u32::try_from(index).ok()?))
            }
            Type::Record(_, fields) | Type::AnonymousRecord(fields) if !fields.is_empty() => {
                if let Some(&id) = self.by_type.get(ty) {
                    return Some(ValueType::Composite(id));
                }
                let name: Box<str> = match ty {
                    Type::Record(name, _) => name.clone(),
                    _ => "".into(),
                };
                let fields = fields
                    .iter()
                    .map(|(_, field)| self.value(field))
                    .collect::<Option<Box<[_]>>>()?;
                // The same record can be spelled with a handle written out or
                // as a reference back to an enum. Both are one shape.
                if let Some(&id) = self.records.get(&(name.clone(), fields.clone())) {
                    self.by_type.insert(ty.clone(), id);
                    return Some(ValueType::Composite(id));
                }
                let id = self.intern(ty, Shape::Tuple(fields.clone()), None)?;
                self.records.insert((name, fields), id);
                ValueType::Composite(id)
            }
            Type::Tuple(items) if !items.is_empty() => {
                if let Some(&id) = self.by_type.get(ty) {
                    return Some(ValueType::Composite(id));
                }
                let fields = items
                    .iter()
                    .map(|item| self.value(item))
                    .collect::<Option<Box<[_]>>>()?;
                if let Some(&id) = self.structural.get(&Shape::Tuple(fields.clone())) {
                    self.by_type.insert(ty.clone(), id);
                    return Some(ValueType::Composite(id));
                }
                let id = self.intern(ty, Shape::Tuple(fields.clone()), None)?;
                self.structural.insert(Shape::Tuple(fields), id);
                ValueType::Composite(id)
            }
            Type::Array(bounds, element) if array_len(bounds).is_some() => {
                if let Some(&id) = self.by_type.get(ty) {
                    return Some(ValueType::Composite(id));
                }
                let element = self.value(element)?;
                let shape = Shape::Array {
                    element,
                    bounds: bounds.as_slice().into(),
                };
                if let Some(&id) = self.structural.get(&shape) {
                    self.by_type.insert(ty.clone(), id);
                    return Some(ValueType::Composite(id));
                }
                let id = self.intern(ty, shape.clone(), None)?;
                self.structural.insert(shape, id);
                ValueType::Composite(id)
            }
            Type::Enum(definition) if !definition.open => {
                if let Some(&id) = self.by_type.get(ty) {
                    return Some(ValueType::Composite(id));
                }
                if let Some(&id) = self.enum_ids.get(&definition.identity) {
                    return Some(ValueType::Composite(id));
                }
                // The shape's number is reserved first so that a handle inside
                // its payloads can refer back to it.
                let declarations: Vec<_> = definition
                    .variants
                    .iter()
                    .map(|variant| variant.declaration)
                    .collect();
                let id = self.intern(ty, Shape::Tuple(Box::new([])), Some(declarations))?;
                self.enum_ids.insert(definition.identity.clone(), id);
                let payloads = definition
                    .variants
                    .iter()
                    .map(|variant| match &variant.payload {
                        None => Some(None),
                        Some(payload) => self.value(payload).map(Some),
                    })
                    .collect::<Option<Box<[_]>>>();
                let Some(payloads) = payloads else {
                    self.by_type.remove(ty);
                    self.enum_ids.remove(&definition.identity);
                    return None;
                };
                self.shapes[id.index()] = Shape::Enum(payloads);
                ValueType::Composite(id)
            }
            Type::EnumRef(identity, _) => ValueType::Composite(*self.enum_ids.get(&**identity)?),
            Type::Managed {
                kind: MemoryKind::Gc,
                locality: Locality::Local,
                pointee,
                ..
            } => {
                let pointee = self.value(pointee)?;
                let index = match self.managed.iter().position(|known| *known == pointee) {
                    Some(index) => index,
                    None => {
                        self.managed.push(pointee);
                        self.managed.len() - 1
                    }
                };
                ValueType::Gc(ManagedId(u32::try_from(index).ok()?))
            }
            _ => return None,
        })
    }

    fn intern(
        &mut self,
        ty: &Type,
        shape: Shape,
        variants: Option<Vec<VariantId>>,
    ) -> Option<ShapeId> {
        let id = ShapeId(u32::try_from(self.shapes.len()).ok()?);
        self.shapes.push(shape);
        self.by_type.insert(ty.clone(), id);
        if let Some(variants) = variants {
            self.variants.insert(id, variants);
        }
        Some(id)
    }

    /// Index of a declared variant within an enum shape.
    pub fn variant_index(&self, shape: ShapeId, declaration: VariantId) -> Option<u32> {
        let position = self
            .variants
            .get(&shape)?
            .iter()
            .position(|&variant| variant == declaration)?;
        u32::try_from(position).ok()
    }

    /// The type a reference to `ty` points at. A closure that borrows is
    /// pointed at like the closure it is.
    pub fn pointee(&mut self, ty: &Type) -> Option<ValueType> {
        match ty {
            Type::Borrowed(inner) if matches!(**inner, Type::Function(..)) => self.value(inner),
            other => self.value(other),
        }
    }

    /// What a handle of this managed type points at.
    pub fn managed_pointee(&self, managed: ManagedId) -> Option<ValueType> {
        self.managed.get(managed.index()).copied()
    }

    pub fn is_array(&self, shape: ShapeId) -> bool {
        matches!(self.shapes.get(shape.index()), Some(Shape::Array { .. }))
    }

    pub fn into_parts(self) -> Tables {
        Tables {
            shapes: self.shapes.into_boxed_slice(),
            signatures: self.signatures.into_boxed_slice(),
            managed: self.managed.into_boxed_slice(),
        }
    }
}
