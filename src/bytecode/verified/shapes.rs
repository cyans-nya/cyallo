//! Interned composite shapes for checked IR types. A shape is keyed by the
//! complete checked type, so two enums with the same structure but different
//! declarations stay distinct and the verifier compares them by identity.

use super::{
    LoanAccess, ManagedId, Method, ScalarType, Shape, ShapeId, Signature, SignatureId,
    TraitDescription, TraitId, ValueType, array_len,
};
use crate::Type;
use cyallo_ast::{
    EnumEncoding, EnumIdentity, Locality, MemoryKind, NumberKind, StorageKind, VariantId,
};
use cyallo_ir::services::{Metadata, Receiver};
use std::{collections::HashMap, sync::Arc};

/// The module-level tables an interner produced.
pub(super) struct Tables {
    pub shapes: Box<[Shape]>,
    pub signatures: Box<[Signature]>,
    pub managed: Box<[ValueType]>,
    pub traits: Box<[TraitDescription]>,
}

pub(super) struct Shapes {
    /// The declared traits, for objects of a trait.
    services: Option<Arc<Metadata>>,
    traits: Vec<TraitDescription>,
    trait_ids: HashMap<Box<str>, TraitId>,
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
    /// Records are nominal: one shape per declared name.
    record_names: HashMap<Box<str>, ShapeId>,
}

impl Shapes {
    pub fn new(services: Option<Arc<Metadata>>) -> Self {
        Self {
            services,
            traits: Vec::new(),
            trait_ids: HashMap::new(),
            by_type: HashMap::new(),
            shapes: Vec::new(),
            variants: HashMap::new(),
            signatures: Vec::new(),
            managed: Vec::new(),
            enum_ids: HashMap::new(),
            encodings: HashMap::new(),
            structural: HashMap::new(),
            record_names: HashMap::new(),
        }
    }

    /// The trait an object type names, described once.
    fn trait_object(&mut self, name: &str) -> Option<ValueType> {
        if let Some(&id) = self.trait_ids.get(name) {
            return Some(ValueType::Object(id));
        }
        let services = self.services.clone()?;
        let declared = services
            .traits
            .iter()
            .find(|candidate| &*candidate.name == name)?;
        // Reserve the number first: a method may mention the trait itself.
        let id = TraitId(u32::try_from(self.traits.len()).ok()?);
        self.traits.push(TraitDescription {
            methods: Box::new([]),
        });
        self.trait_ids.insert(name.into(), id);
        let methods = declared
            .methods
            .iter()
            .map(|method| {
                let receiver = match method.receiver {
                    Receiver::Shared => LoanAccess::Shared,
                    Receiver::Mutable => LoanAccess::Exclusive,
                    Receiver::Value => return None,
                };
                if method.params.iter().any(|&(_, by_reference)| by_reference) {
                    return None;
                }
                let parameters = method
                    .params
                    .iter()
                    .map(|(ty, _)| self.value(ty))
                    .collect::<Option<Box<[_]>>>()?;
                let result = match &method.result {
                    Type::Void => None,
                    other => Some(self.value(other)?),
                };
                Some(Method {
                    receiver,
                    parameters,
                    result,
                })
            })
            .collect::<Option<Box<[_]>>>();
        let Some(methods) = methods else {
            self.trait_ids.remove(name);
            return None;
        };
        self.traits[id.index()] = TraitDescription { methods };
        Some(ValueType::Object(id))
    }

    /// The `Object` type of a declared trait, by its index in the metadata.
    pub fn trait_of(&mut self, declared: usize) -> Option<TraitId> {
        let name = self.services.as_ref()?.traits.get(declared)?.name.clone();
        match self.trait_object(&name)? {
            ValueType::Object(id) => Some(id),
            _ => None,
        }
    }
    /// The verified type of a type that needs no new table entry: scalars,
    /// strings, objects, erased values and shapes already interned.
    pub fn value_of(&self, ty: &Type) -> Option<ValueType> {
        Some(match ty {
            Type::Integer => ScalarType::I64.into(),
            Type::Real => ScalarType::F64.into(),
            Type::Numeric(NumberKind::Usize | NumberKind::U64) => ScalarType::U64.into(),
            Type::Numeric(NumberKind::I64) => ScalarType::I64.into(),
            Type::Numeric(NumberKind::F64) => ScalarType::F64.into(),
            Type::String => ValueType::Text,
            Type::Boolean => ScalarType::Bool.into(),
            Type::Object(_) => ValueType::OpenObject,
            Type::Dynamic => ValueType::Dynamic,
            other => ValueType::Composite(*self.by_type.get(other)?),
        })
    }

    /// The verified type of a checked type, or `None` while it has no verified
    /// representation. Components are interned first, which numbers them below
    /// the composite that contains them.
    pub fn value(&mut self, ty: &Type) -> Option<ValueType> {
        Some(match ty {
            Type::Integer => ScalarType::I64.into(),
            Type::Real => ScalarType::F64.into(),
            Type::Numeric(NumberKind::Usize | NumberKind::U64) => ScalarType::U64.into(),
            Type::Numeric(NumberKind::I64) => ScalarType::I64.into(),
            Type::Numeric(NumberKind::F64) => ScalarType::F64.into(),
            Type::String => ValueType::Text,
            Type::Boolean => ScalarType::Bool.into(),
            Type::TraitObject(name) => self.trait_object(name)?,
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
            Type::Record(name, fields) if !fields.is_empty() => {
                if let Some(&id) = self.by_type.get(ty) {
                    return Some(ValueType::Composite(id));
                }
                if let Some(&id) = self.record_names.get(name) {
                    return Some(ValueType::Composite(id));
                }
                // The shape's number is reserved first so that a handle inside
                // its fields can refer back to the record by name.
                let id = self.intern(ty, Shape::Tuple(Box::new([])), None)?;
                self.record_names.insert(name.clone(), id);
                let fields = fields
                    .iter()
                    .map(|(_, field)| self.value(field))
                    .collect::<Option<Box<[_]>>>();
                let Some(fields) = fields else {
                    self.by_type.remove(ty);
                    self.record_names.remove(name);
                    return None;
                };
                self.shapes[id.index()] = Shape::Tuple(fields);
                ValueType::Composite(id)
            }
            Type::RecordRef(name) => ValueType::Composite(*self.record_names.get(name)?),
            Type::AnonymousRecord(fields) if !fields.is_empty() => {
                let fields = fields
                    .iter()
                    .map(|(_, field)| self.value(field))
                    .collect::<Option<Box<[_]>>>()?;
                self.structural_shape(Shape::Tuple(fields))?
            }
            // An object, open or typed, and a value of any type.
            Type::Object(_) => ValueType::OpenObject,
            Type::Dynamic => ValueType::Dynamic,
            // A task over its result type, and how a task ended.
            Type::Opaque(_, arguments, traits) if traits.task() && arguments.len() == 1 => {
                let result = self.value_or_unit(&arguments[0])?;
                ValueType::Task(self.managed_id(result)?)
            }
            Type::Opaque(_, arguments, traits) if traits.task_outcome() && arguments.len() == 1 => {
                let result = self.value_or_unit(&arguments[0])?;
                self.structural_shape(Shape::Enum(Box::new([Some(result), None, None, None])))?
            }
            Type::Opaque(_, _, traits) if traits.clock() => ValueType::Clock,
            // The two ends of a channel over an element type.
            Type::Opaque(_, arguments, traits)
                if (traits.sender() || traits.receiver()) && arguments.len() == 1 =>
            {
                let element = self.value(&arguments[0])?;
                let managed = self.managed_id(element)?;
                if traits.sender() {
                    ValueType::Sender(managed)
                } else {
                    ValueType::Receiver(managed)
                }
            }
            // The empty tuple is the unit value: a composite with no fields.
            Type::Tuple(items) if items.is_empty() => {
                self.structural_shape(Shape::Tuple(Box::new([])))?
            }
            // A generator: what it yields, what a resume passes in and what it
            // returns, in the signature table.
            Type::Opaque(_, arguments, traits) if traits.generator() && arguments.len() == 3 => {
                let yielded = self.value(&arguments[0])?;
                let resume = self.value(&arguments[1])?;
                let returned = self.value(&arguments[2])?;
                let description = Signature {
                    parameters: Box::new([yielded, resume]),
                    result: Some(returned),
                };
                let index = match self
                    .signatures
                    .iter()
                    .position(|known| *known == description)
                {
                    Some(index) => index,
                    None => {
                        self.signatures.push(description);
                        self.signatures.len() - 1
                    }
                };
                ValueType::Generator(SignatureId(u32::try_from(index).ok()?))
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
                        // A variant that carries nothing, written `()`.
                        Some(Type::Void) => {
                            self.structural_shape(Shape::Tuple(Box::new([]))).map(Some)
                        }
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
            Type::Opaque(_, arguments, traits) if traits.weak() => {
                let [owner] = arguments.as_slice() else {
                    return None;
                };
                let ValueType::Rc(managed) = self.value(owner)? else {
                    return None;
                };
                ValueType::Weak(managed)
            }
            // A relative storage recipe is its declared kind of opaque type.
            Type::Opaque(_, arguments, traits) if traits.path() && arguments.len() == 1 => {
                ValueType::Path
            }
            // A growable collection, a mutable cell and a range are described
            // by the traits and kinds their declarations carry.
            Type::Opaque(_, arguments, traits)
                if traits.collection() && !traits.double_ended() && arguments.len() == 1 =>
            {
                let element = self.value(&arguments[0])?;
                self.structural_shape(Shape::Vec { element })?
            }
            Type::Opaque(_, arguments, traits) if traits.deref() && arguments.len() == 1 => {
                let element = self.value(&arguments[0])?;
                self.structural_shape(Shape::Cell { element })?
            }
            // A step of a generator in its storage form: yielded, then returned.
            Type::Storage(StorageKind::GeneratorStep, inner) => {
                let Type::Tuple(items) = &**inner else {
                    return None;
                };
                let [yielded, returned] = items.as_slice() else {
                    return None;
                };
                let yielded = self.value(yielded)?;
                let returned = self.value(returned)?;
                self.structural_shape(Shape::Enum(Box::new([Some(yielded), Some(returned)])))?
            }
            // The allocator protocol's request is a size and an alignment.
            Type::Opaque(_, _, traits) if traits.layout() => {
                let count = ValueType::Scalar(ScalarType::U64);
                self.structural_shape(Shape::Tuple(Box::new([count, count])))?
            }
            Type::Opaque(_, _, traits) if traits.allocation() => ValueType::Allocation,
            // A failure that carries no data is one unit variant.
            Type::Storage(StorageKind::ParseError | StorageKind::AllocError, _) => {
                self.structural_shape(Shape::Enum(Box::new([None])))?
            }
            Type::Storage(StorageKind::Range, element) => {
                let bound = self.value(element)?;
                if bound != ValueType::Scalar(ScalarType::I64) {
                    return None;
                }
                self.structural_shape(Shape::Tuple(Box::new([bound, bound])))?
            }
            Type::Opaque(_, arguments, traits) if traits.location() && arguments.len() == 1 => {
                let field = self.value(&arguments[0])?;
                ValueType::Location(self.managed_id(field)?)
            }
            Type::Managed {
                kind: kind @ (MemoryKind::Gc | MemoryKind::Rc),
                locality: Locality::Local,
                pointee,
                ..
            } => {
                let pointee = self.value(pointee)?;
                let id = self.managed_id(pointee)?;
                match kind {
                    MemoryKind::Gc => ValueType::Gc(id),
                    MemoryKind::Rc => ValueType::Rc(id),
                }
            }
            _ => return None,
        })
    }

    /// The type of a value that may be absent: the unit tuple for `()`.
    fn value_or_unit(&mut self, ty: &Type) -> Option<ValueType> {
        match ty {
            Type::Void => self.structural_shape(Shape::Tuple(Box::new([]))),
            other => self.value(other),
        }
    }

    /// The entry of the managed-type table for a pointee, interned once.
    fn managed_id(&mut self, pointee: ValueType) -> Option<ManagedId> {
        let index = match self.managed.iter().position(|known| *known == pointee) {
            Some(index) => index,
            None => {
                self.managed.push(pointee);
                self.managed.len() - 1
            }
        };
        Some(ManagedId(u32::try_from(index).ok()?))
    }

    /// The composite type of a structural shape, interned once.
    fn structural_shape(&mut self, shape: Shape) -> Option<ValueType> {
        if let Some(&id) = self.structural.get(&shape) {
            return Some(ValueType::Composite(id));
        }
        let id = self.push_shape(shape.clone())?;
        self.structural.insert(shape, id);
        Some(ValueType::Composite(id))
    }

    /// Add a shape that no checked type names.
    fn push_shape(&mut self, shape: Shape) -> Option<ShapeId> {
        let id = ShapeId(u32::try_from(self.shapes.len()).ok()?);
        self.shapes.push(shape);
        Some(id)
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
            Type::Slice(element) => {
                let element = self.value(element)?;
                let shape = Shape::Slice { element };
                if let Some(&id) = self.structural.get(&shape) {
                    return Some(ValueType::Composite(id));
                }
                let id = self.push_shape(shape.clone())?;
                self.structural.insert(shape, id);
                Some(ValueType::Composite(id))
            }
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
            ValueType::Rc(_)
            | ValueType::Weak(_)
            | ValueType::Generator(_)
            | ValueType::Sender(_)
            | ValueType::Receiver(_) => true,
            ValueType::Composite(shape) => match self.shapes.get(shape.index()) {
                Some(Shape::Tuple(fields)) => fields.iter().any(|&field| self.needs_drop(field)),
                Some(Shape::Enum(variants)) => variants
                    .iter()
                    .flatten()
                    .any(|&payload| self.needs_drop(payload)),
                Some(Shape::Array { element, .. }) => self.needs_drop(*element),
                Some(Shape::Vec { element } | Shape::Cell { element }) => self.needs_drop(*element),
                // A view owns nothing.
                Some(Shape::Slice { .. }) => false,
                None => false,
            },
            ValueType::Scalar(_)
            | ValueType::Text
            | ValueType::Function(_)
            | ValueType::Gc(_)
            | ValueType::Object(_)
            | ValueType::Path
            | ValueType::Location(_)
            | ValueType::Allocation
            | ValueType::Task(_)
            | ValueType::Clock
            | ValueType::OpenObject
            | ValueType::Dynamic => false,
        }
    }

    /// What a variant of an enum shape carries.
    pub fn variant_payload(&self, shape: ShapeId, variant: u32) -> Option<ValueType> {
        match self.shapes.get(shape.index())? {
            Shape::Enum(variants) => variants.get(variant as usize).copied().flatten(),
            _ => None,
        }
    }

    /// The `Ok` and `Err` variants of a result-shaped enum.
    pub fn result_variants(&self, shape: ShapeId) -> Option<(u32, u32)> {
        let encodings = self.encodings.get(&shape)?;
        let ok = encodings
            .iter()
            .position(|e| *e == EnumEncoding::ResultOk)?;
        let err = encodings
            .iter()
            .position(|e| *e == EnumEncoding::ResultErr)?;
        Some((u32::try_from(ok).ok()?, u32::try_from(err).ok()?))
    }

    /// The variant of an enum shape that carries this encoding.
    pub fn variant_with(&self, shape: ShapeId, encoding: EnumEncoding) -> Option<u32> {
        let position = self
            .encodings
            .get(&shape)?
            .iter()
            .position(|e| *e == encoding)?;
        u32::try_from(position).ok()
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
            traits: self.traits.into_boxed_slice(),
        }
    }
}
