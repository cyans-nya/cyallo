//! Interned composite shapes for checked IR types. A shape is keyed by the
//! complete checked type, so two enums with the same structure but different
//! declarations stay distinct and the verifier compares them by identity.

use super::{ManagedId, ScalarType, Shape, ShapeId, Signature, SignatureId, ValueType, array_len};
use crate::Type;
use cyallo_ast::{EnumEncoding, EnumIdentity, Locality, MemoryKind, NumberKind, VariantId};
use std::collections::HashMap;

/// The suffix of the standard weak reference's declared name.
const WEAK_NAME_SUFFIX: &str = "$Type$Weak";

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
    /// The encoding of each variant of an enum shape.
    encodings: HashMap<ShapeId, Vec<EnumEncoding>>,
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
            Type::Numeric(NumberKind::Usize | NumberKind::U64) => ScalarType::U64.into(),
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
                self.encodings.insert(
                    id,
                    definition
                        .variants
                        .iter()
                        .map(|variant| variant.encoding)
                        .collect(),
                );
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
            Type::Opaque(name, arguments, _) if name.ends_with(WEAK_NAME_SUFFIX) => {
                // The standard weak reference is spelled by name until opaque
                // types come from provider bindings.
                let [owner] = arguments.as_slice() else {
                    return None;
                };
                let ValueType::Rc(managed) = self.value(owner)? else {
                    return None;
                };
                ValueType::Weak(managed)
            }
            Type::Managed {
                kind: kind @ (MemoryKind::Gc | MemoryKind::Rc),
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
                let id = ManagedId(u32::try_from(index).ok()?);
                match kind {
                    MemoryKind::Gc => ValueType::Gc(id),
                    MemoryKind::Rc => ValueType::Rc(id),
                }
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

    /// Whether releasing a value changes counts: it holds an owner or a weak
    /// reference, directly or inside a composite.
    pub fn needs_drop(&self, ty: ValueType) -> bool {
        match ty {
            ValueType::Rc(_) | ValueType::Weak(_) => true,
            ValueType::Composite(shape) => match self.shapes.get(shape.index()) {
                Some(Shape::Tuple(fields)) => fields.iter().any(|&field| self.needs_drop(field)),
                Some(Shape::Enum(variants)) => variants
                    .iter()
                    .flatten()
                    .any(|&payload| self.needs_drop(payload)),
                Some(Shape::Array { element, .. }) => self.needs_drop(*element),
                None => false,
            },
            ValueType::Scalar(_) | ValueType::Text | ValueType::Function(_) | ValueType::Gc(_) => {
                false
            }
        }
    }

    /// What a variant of an enum shape carries.
    pub fn variant_payload(&self, shape: ShapeId, variant: u32) -> Option<ValueType> {
        match self.shapes.get(shape.index())? {
            Shape::Enum(variants) => variants.get(variant as usize).copied().flatten(),
            _ => None,
        }
    }

    /// The `Some` and `None` variants of an option-shaped enum.
    pub fn option_variants(&self, shape: ShapeId) -> Option<(u32, u32)> {
        let encodings = self.encodings.get(&shape)?;
        let some = encodings
            .iter()
            .position(|e| *e == EnumEncoding::OptionSome)?;
        let none = encodings
            .iter()
            .position(|e| *e == EnumEncoding::OptionNone)?;
        Some((u32::try_from(some).ok()?, u32::try_from(none).ok()?))
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
