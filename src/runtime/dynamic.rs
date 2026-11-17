//! One object/property/call implementation for open objects and module namespaces.
mod ic;
use super::{RunResult, Session, Value};
use crate::dynamic_modules::{Adapter, CallRequest, CompiledCalls};
use crate::ir::Reg;
use cyallo_ast::{ObjectKind, Operator, RoutineKind, Type};
use cyallo_ir::dynamic::{CallableTarget, Operation};
use cyallo_shapes::{
    Access, Contract, Domain, DynamicError as E, Property, PropertyId, Representation, ShapeId,
};
pub(crate) use ic::{NativeCache, NativeCase, NativeObject};
pub use ic::{ObjectIcMode, ObjectStats};
use std::collections::HashMap;
#[derive(Default)]
pub(super) struct Objects {
    native: ic::NativeObjects,
    domain: Domain,
    sites: HashMap<(usize, usize), Site>,
    adapters: Vec<Adapter>,
}
#[derive(Default)]
struct Site {
    key_value: Option<std::sync::Arc<str>>,
    key: Option<PropertyId>,
    name: Option<Box<str>>,
    cases: Vec<(ShapeId, Option<usize>)>,
}
impl Objects {
    pub(super) fn relink(&mut self, functions: &[usize]) {
        for adapter in &mut self.adapters {
            adapter.function = functions[adapter.function];
        }
        self.sites.clear();
        self.native.reset_caches();
    }
}
type RequiredFields = Vec<(Box<str>, Type)>;
type ViewedObjects = Vec<(usize, RequiredFields)>;
#[derive(Clone, Copy)]
pub(super) enum ViewMode {
    Result,
    Trap,
}
pub(super) enum SavedAction {
    Invoke {
        roots: super::gc::RootScope,
        arguments: Vec<super::gc::RootId>,
    },
    View {
        roots: super::gc::RootScope,
        value: super::gc::RootId,
        actual: Type,
        expected: Type,
        mode: ViewMode,
    },
}
impl Session {
    pub(super) fn validate_dynamic_owner(&self, value: &Value) -> Result<(), E> {
        let mut pending = vec![value];
        let mut seen = std::collections::HashSet::new();
        let mut generators = std::collections::HashSet::new();
        while let Some(value) = pending.pop() {
            match value {
                Value::Address(_) | Value::Slice { .. } | Value::StoragePath(..) => {
                    return Err(E::BorrowedValue);
                }
                Value::Dynamic(ty, value) | Value::Erased(ty, value) => {
                    if !cyallo_ir::dynamic::owned(ty) {
                        return Err(E::BorrowedValue);
                    }
                    pending.push(value);
                }
                Value::Object(id) | Value::Managed(id) | Value::TraitObject(id, _) => {
                    if seen.insert(*id) {
                        pending.push(&self.heap[*id].value);
                    }
                }
                Value::Function(_, values)
                | Value::ArraySnapshot(values)
                | Value::ObjectStorage { slots: values, .. } => pending.extend(values),
                Value::Array(ids) => {
                    for id in ids {
                        if seen.insert(*id) {
                            pending.push(&self.heap[*id].value);
                        }
                    }
                }
                Value::Deque(ids) => {
                    for id in ids {
                        if seen.insert(*id) {
                            pending.push(&self.heap[*id].value);
                        }
                    }
                }
                Value::SparseArray(ids, _) => {
                    for id in ids.values() {
                        if seen.insert(*id) {
                            pending.push(&self.heap[*id].value);
                        }
                    }
                }
                Value::SparseSnapshot(values, _) => pending.extend(values.values()),
                Value::Rc(_) | Value::Weak(_) => {
                    if let Some(id) = self.rc_payload_cell(value).map_err(|_| E::BorrowedValue)?
                        && seen.insert(id)
                    {
                        pending.push(&self.heap[id].value);
                    }
                }
                Value::Generator(id) => {
                    if generators.insert(*id) {
                        pending.extend(self.generator_owned_values(*id)?);
                    }
                }
                Value::TaskOutcome(cyallo_concurrent_values::TaskOutcome::Completed(value)) => {
                    pending.push(value)
                }
                Value::EnumStorage(_, Some(id)) | Value::NativeEnumStorage(_, Some(id)) => {
                    if seen.insert(*id) {
                        pending.push(&self.heap[*id].value);
                    }
                }
                Value::Enum(_, Some(value))
                | Value::Cell(value) => pending.push(value),
                _ => {}
            }
        }
        Ok(())
    }
    fn callable_target(value: &Value) -> Option<CallableTarget> {
        match value {
            Value::Dynamic(_, v) => Self::callable_target(v),
            Value::NativeCallable(id) => Some(CallableTarget::Native(*id)),
            Value::DeferredCallable(name) => Some(CallableTarget::Source(name.clone())),
            _ => None,
        }
    }
    fn dynamic_parts(&self, value: Value, ty: Type) -> (Type, Value) {
        match value {
            Value::Dynamic(ty, value) => self.dynamic_parts(*value, ty),
            value => (ty, value),
        }
    }
    fn representation(value: &Value) -> Representation {
        match value {
            Value::Dynamic(_, value) => Self::representation(value),
            Value::Integer(_) => Representation::Number(cyallo_ast::NumberKind::I64),
            Value::Real(_) => Representation::Number(cyallo_ast::NumberKind::F64),
            Value::Number(value) => Representation::Number(value.kind),
            Value::Boolean(_) => Representation::Boolean,
            Value::Char(_) => Representation::Char,
            Value::String(_) => Representation::String,
            Value::Object(_) => Representation::Object,
            Value::Function(..) | Value::NativeCallable(_) => Representation::Callable,
            _ => Representation::Other,
        }
    }
    fn object_id(&self, value: &Value, ty: &Type) -> Result<usize, E> {
        match value {
            Value::Dynamic(actual, value) => self.object_id(value, actual),
            Value::Object(id) => Ok(*id),
            Value::Address(id) | Value::Managed(id) => {
                self.object_id(&self.heap[*id].value, ty.pointee_or_self())
            }
            Value::Rc(_) => self.object_id(
                &self.heap[self
                    .rc_address(value)
                    .map_err(|_| E::ExpectedObject(ty.clone()))?]
                .value,
                ty.pointee_or_self(),
            ),
            _ => Err(E::ExpectedObject(ty.clone())),
        }
    }
    fn object_shape(&self, id: usize) -> ShapeId {
        let Value::ObjectStorage { shape, .. } = self.heap[id].value else {
            unreachable!("object handles identify property storage")
        };
        shape
    }
    fn property_slot(&mut self, shape: ShapeId, name: &str) -> Result<Option<usize>, E> {
        self.objects.native.stats.lookups = self.objects.native.stats.lookups.saturating_add(1);
        let frame = self.frames.last().unwrap();
        let site = self
            .objects
            .sites
            .entry((frame.function, frame.pc - 1))
            .or_default();
        let key = if site.name.as_deref() == Some(name) {
            site.key.unwrap()
        } else {
            let id = self.objects.domain.property(name)?;
            site.key = Some(id);
            site.name = Some(name.into());
            site.cases.clear();
            id
        };
        if let Some((_, slot)) = site.cases.iter().find(|(id, _)| *id == shape) {
            return Ok(*slot);
        }
        let slot = self.objects.domain.get(shape).slot(key);
        self.objects.native.stats.shape_misses =
            self.objects.native.stats.shape_misses.saturating_add(1);
        if site.cases.len() < 4 {
            site.cases.push((shape, slot));
        } else {
            self.objects.native.stats.megamorphic_misses = self
                .objects
                .native
                .stats
                .megamorphic_misses
                .saturating_add(1);
        }
        Ok(slot)
    }
    fn compatible(expected: &Type, actual: &Type) -> bool {
        *expected == Type::Dynamic
            || cyallo_ir::declarations::equivalent_types(expected, actual)
            || matches!((expected,actual),(Type::Function(RoutineKind::Procedure,a,r),Type::Function(RoutineKind::Function,b,s)) if a==b && cyallo_ir::declarations::equivalent_types(r,s))
    }
    fn check_value(&mut self, value: Value, actual: Type, expected: &Type) -> Result<Value, E> {
        let (actual, value) = self.dynamic_parts(value, actual);
        if *expected == Type::Dynamic {
            return Ok(Value::Dynamic(actual, Box::new(value)));
        }
        if let Some(target) = Self::callable_target(&value)
            && matches!(expected, Type::Function(..))
        {
            if let Some(adapter) = self.objects.adapters.iter().find(|adapter| {
                adapter.request.target == target
                    && adapter.request.expected.as_ref() == Some(expected)
            }) {
                return Ok(Value::Function(adapter.function, vec![]));
            }
            return Err(E::ExpectedCallable(actual));
        }
        if let Type::Object(fields) = expected {
            let object = self.object_id(&value, &actual)?;
            self.establish_view(object, fields)?;
            return Ok(Value::Object(object));
        }
        if Self::compatible(expected, &actual) {
            Ok(value)
        } else {
            Err(E::Type {
                expected: expected.clone(),
                actual,
            })
        }
    }
    fn establish_view(&mut self, object: usize, fields: &[(Box<str>, Type)]) -> Result<(), E> {
        let mut changes = vec![];
        self.validate_view(object, fields, &mut vec![], &mut changes)?;
        for (object, properties, access) in changes {
            let shape = self.objects.domain.shape(properties, access)?;
            let Value::ObjectStorage { shape: stored, .. } = &mut self.heap[object].value else {
                unreachable!()
            };
            *stored = shape;
            self.refresh_object_native(object);
        }
        Ok(())
    }
    fn validate_view(
        &mut self,
        object: usize,
        fields: &[(Box<str>, Type)],
        seen: &mut ViewedObjects,
        changes: &mut Vec<(usize, Vec<Property>, Access)>,
    ) -> Result<(), E> {
        if seen.iter().any(|(id, old)| *id == object && old == fields) {
            return Ok(());
        }
        seen.push((object, fields.to_vec()));
        let shape = self.objects.domain.get(self.object_shape(object));
        let Value::ObjectStorage { slots, .. } = &self.heap[object].value else {
            unreachable!()
        };
        let values = slots.clone();
        let mut properties = changes
            .iter()
            .find(|(id, ..)| *id == object)
            .map(|(_, p, _)| p.clone())
            .unwrap_or_else(|| shape.fields.to_vec());
        for (name, expected) in fields {
            let key = self.objects.domain.property(name)?;
            let slot = shape
                .slot(key)
                .ok_or_else(|| E::MissingProperty(name.clone()))?;
            let Value::Dynamic(actual, value) = &values[slot] else {
                unreachable!("property values retain their types")
            };
            self.check_view_value(
                value.as_ref().clone(),
                actual.clone(),
                expected,
                seen,
                changes,
            )?;
            if shape.access == Access::Mutable
                && let Contract::Required(old) = &properties[slot].contract
                && *old != Type::Dynamic
                && !cyallo_ir::declarations::equivalent_types(old, expected)
            {
                return Err(E::Type {
                    expected: expected.clone(),
                    actual: old.clone(),
                });
            }
            if shape.access == Access::Mutable {
                properties[slot].contract = Contract::Required(expected.clone());
            }
        }
        if let Some((_, old, _)) = changes.iter_mut().find(|(id, ..)| *id == object) {
            *old = properties;
        } else {
            changes.push((object, properties, shape.access));
        }
        Ok(())
    }
    fn check_view_value(
        &mut self,
        value: Value,
        actual: Type,
        expected: &Type,
        seen: &mut ViewedObjects,
        changes: &mut Vec<(usize, Vec<Property>, Access)>,
    ) -> Result<(), E> {
        if let Type::Object(fields) = expected {
            return self.validate_view(self.object_id(&value, &actual)?, fields, seen, changes);
        }
        self.check_value(value, actual, expected).map(|_| ())
    }
    pub(super) fn freeze_object(&mut self, value: &Value, ty: &Type) -> Result<(), E> {
        let object = self.object_id(value, ty)?;
        let shape = self.objects.domain.get(self.object_shape(object));
        let frozen = self
            .objects
            .domain
            .shape(shape.fields.to_vec(), Access::Readonly)?;
        let Value::ObjectStorage { shape, .. } = &mut self.heap[object].value else {
            unreachable!()
        };
        *shape = frozen;
        self.refresh_object_native(object);
        Ok(())
    }
    fn view_requests(
        &self,
        value: &Value,
        actual: &Type,
        expected: &Type,
        requests: &mut Vec<CallRequest>,
        seen: &mut Vec<(usize, Type)>,
    ) -> Result<(), E> {
        let (actual, value) = self.dynamic_parts(value.clone(), actual.clone());
        if let Type::Object(fields) = expected {
            let object = self.object_id(&value, &actual)?;
            if seen.iter().any(|(id, ty)| *id == object && ty == expected) {
                return Ok(());
            }
            seen.push((object, expected.clone()));
            let shape = self.objects.domain.get(self.object_shape(object));
            let Value::ObjectStorage { slots, .. } = &self.heap[object].value else {
                unreachable!()
            };
            for (name, ty) in fields {
                let slot = shape
                    .fields
                    .iter()
                    .position(|property| self.objects.domain.name(property.id) == name.as_ref())
                    .ok_or_else(|| E::MissingProperty(name.clone()))?;
                self.view_requests(&slots[slot], &Type::Dynamic, ty, requests, seen)?;
            }
        } else if let Some(target) = Self::callable_target(&value)
            && let Type::Function(_, parameters, _) = expected
        {
            if parameters
                .iter()
                .any(|(ty, by_ref)| *by_ref || !cyallo_ir::dynamic::owned(ty))
            {
                return Err(E::BorrowedCall);
            }
            let request = CallRequest {
                target,
                arguments: parameters.iter().map(|(ty, _)| ty.clone()).collect(),
                expected: Some(expected.clone()),
                span: self.current_span().unwrap_or_default(),
            };
            if !self
                .objects
                .adapters
                .iter()
                .any(|a| a.request.same_contract(&request))
                && !requests.iter().any(|r| r.same_contract(&request))
            {
                requests.push(request);
            }
        } else if !Self::compatible(expected, &actual) {
            return Err(E::Type {
                expected: expected.clone(),
                actual,
            });
        }
        Ok(())
    }
    fn view_result(
        &mut self,
        destination: Reg,
        result: Result<Value, E>,
        mode: ViewMode,
    ) -> RunResult<()> {
        let value = if matches!(mode, ViewMode::Result) {
            Value::result(
                result
                    .map(Box::new)
                    .map_err(|e| Box::new(Value::ViewError(std::sync::Arc::new(e)))),
            )
        } else {
            result?
        };
        self.set(destination, value);
        Ok(())
    }
    fn begin_view(
        &mut self,
        destination: Reg,
        value: Value,
        actual: Type,
        expected: Type,
        mode: ViewMode,
    ) -> RunResult<()> {
        let mut requests = vec![];
        if let Err(error) =
            self.view_requests(&value, &actual, &expected, &mut requests, &mut vec![])
        {
            return self.view_result(destination, Err(error), mode);
        }
        if requests.is_empty() {
            let result = self.check_value(value, actual, &expected);
            return self.view_result(destination, result, mode);
        }
        let roots = self.root_scope();
        let value = roots.value(value);
        self.compile_dynamic(
            destination,
            requests,
            SavedAction::View {
                roots,
                value,
                actual,
                expected,
                mode,
            },
        )
    }
    fn compile_dynamic(
        &mut self,
        destination: Reg,
        requests: Vec<CallRequest>,
        action: SavedAction,
    ) -> RunResult<()> {
        self.ensure_concurrency()?;
        let spawner = self.import_spawner();
        let program = self.program.clone();
        self.park_operation(
            destination,
            Box::pin(async move {
                let result = match spawner
                    .blocking(move || crate::dynamic_modules::specialize_calls(program, requests))
                {
                    Ok(operation) => match operation.await {
                        Ok(Ok(result)) => result,
                        Ok(Err(error)) => {
                            Err(E::Call(Box::new(cyallo_modules::LoadError::Host(error))))
                        }
                        Err(_) => Err(E::Call(Box::new(cyallo_modules::LoadError::Cancelled))),
                    },
                    Err(error) => Err(E::Call(Box::new(cyallo_modules::LoadError::Host(error)))),
                };
                Ok(super::concurrent::CompletedOperation::Calls(
                    result,
                    Box::new(action),
                ))
            }),
        );
        Ok(())
    }
    pub(super) fn finish_dynamic(
        &mut self,
        destination: Reg,
        result: Result<CompiledCalls, E>,
        action: SavedAction,
    ) -> RunResult<()> {
        let compiled = match result {
            Ok(compiled) => compiled,
            Err(error) => {
                return match action {
                    SavedAction::View { mode, .. } => {
                        self.view_result(destination, Err(error), mode)
                    }
                    _ => Err(error.into()),
                };
            }
        };
        if !std::sync::Arc::ptr_eq(&self.program, &compiled.program) {
            self.publish_code(compiled.program)?;
        }
        let adapter = compiled.adapters.first().cloned();
        self.objects.adapters.extend(compiled.adapters);
        match action {
            SavedAction::Invoke { roots, arguments } => self.call(
                adapter.unwrap().function,
                arguments.into_iter().map(|r| roots.value_copy(r)).collect(),
                Some(destination),
            )?,
            SavedAction::View {
                roots,
                value,
                actual,
                expected,
                mode,
            } => {
                let result = self.check_value(roots.value_copy(value), actual, &expected);
                self.view_result(destination, result, mode)?;
            }
        }
        for initializer in compiled.initializers.into_iter().rev() {
            if self.initialized_modules.insert(initializer) {
                self.call(initializer, vec![], None)?;
            }
        }
        Ok(())
    }
    pub(super) fn execute_dynamic(
        &mut self,
        destination: Reg,
        operation: &Operation,
        arguments: &[Reg],
    ) -> RunResult<()> {
        let frame = self.frames.last().unwrap();
        let routine = &self.program.functions[frame.function];
        let types: Vec<_> = arguments
            .iter()
            .map(|r| routine.registers[*r].clone())
            .collect();
        let result = routine.registers[destination].clone();
        let values: Vec<_> = arguments.iter().map(|r| self.register(*r)).collect();
        let value = match operation {
            Operation::TypeDescriptor(descriptor) => Value::TypeDescriptor(descriptor.clone()),
            Operation::DeferredCallable(name) => Value::Dynamic(
                Type::Dynamic,
                Box::new(Value::DeferredCallable(name.clone())),
            ),
            Operation::Object(kind, names) => {
                let roots = self.root_scope();
                let mut fields = vec![];
                let mut slots = vec![];
                for ((name, value), ty) in names.iter().zip(values).zip(types) {
                    let (ty, value) = self.dynamic_parts(value, ty);
                    self.validate_dynamic_owner(&value)?;
                    let id = self.objects.domain.property(name)?;
                    fields.push(Property {
                        id,
                        contract: if *kind == ObjectKind::Typed {
                            Contract::Required(ty.clone())
                        } else {
                            Contract::Configurable
                        },
                        representation: Self::representation(&value),
                    });
                    slots.push(roots.value(Value::Dynamic(ty, Box::new(value))));
                }
                let shape = self.objects.domain.shape(fields, Access::Mutable)?;
                let object = self.alloc(&Type::Object(vec![]))?;
                self.heap[object].value = Value::ObjectStorage {
                    shape,
                    slots: slots
                        .into_iter()
                        .map(|root| roots.value_copy(root))
                        .collect(),
                };
                self.refresh_object_native(object);
                if *kind == ObjectKind::Dynamic {
                    Value::Dynamic(Type::Object(vec![]), Box::new(Value::Object(object)))
                } else {
                    Value::Object(object)
                }
            }
            Operation::Erase => {
                if !cyallo_ir::dynamic::owned(&types[0]) {
                    return Err(E::BorrowedValue.into());
                }
                self.validate_dynamic_owner(&values[0])?;
                let (ty, value) = self.dynamic_parts(values[0].clone(), types[0].clone());
                Value::Dynamic(ty, Box::new(value))
            }
            Operation::Check(expected) => {
                return self.begin_view(
                    destination,
                    values[0].clone(),
                    types[0].clone(),
                    expected.clone(),
                    ViewMode::Trap,
                );
            }
            Operation::Freeze => {
                self.freeze_object(&values[0], &types[0])?;
                values[0].clone()
            }
            Operation::View => {
                let Some((value, _)) = cyallo_ir::lookup::result_payload(&result) else {
                    unreachable!()
                };
                return self.begin_view(
                    destination,
                    values[0].clone(),
                    types[0].clone(),
                    value.clone(),
                    ViewMode::Result,
                );
            }
            Operation::Get(name) | Operation::Set(name) | Operation::Delete(name) => {
                return self.dynamic_property(
                    destination,
                    operation,
                    &values,
                    &types,
                    name.clone(),
                );
            }
            Operation::GetKey | Operation::SetKey | Operation::DeleteKey => {
                let (key_type, key) = self.dynamic_parts(values[1].clone(), types[1].clone());
                if !matches!(key, Value::String(_)) && matches!(operation, Operation::GetKey) {
                    if self.object_id(&values[0], &types[0]).is_ok() {
                        return Err(E::ExpectedKey(key_type).into());
                    }
                    return self.dynamic_index(destination, &values[0], &types[0], key);
                }
                let Value::String(key) = key else {
                    return Err(E::ExpectedKey(key_type).into());
                };
                self.property_key(key.clone());
                return self.dynamic_property(
                    destination,
                    operation,
                    &values,
                    &types,
                    key.as_ref().into(),
                );
            }
            Operation::Invoke => return self.dynamic_invoke(destination, values, types),
            Operation::Unary(operator) => {
                let (ty, value) = self.dynamic_parts(values[0].clone(), types[0].clone());
                if !matches!((operator, &value), (Operator::Not, Value::Boolean(_)))
                    && !(matches!(operator, Operator::Add | Operator::Subtract)
                        && ty.number_kind().is_some())
                {
                    return Err(E::Operator {
                        operator: *operator,
                        left: ty.clone(),
                        right: None,
                    }
                    .into());
                }
                let value = super::unary(*operator, &value)?;
                if result == Type::Dynamic {
                    Value::Dynamic(ty, Box::new(value))
                } else {
                    value
                }
            }
            Operation::Binary(operator) => {
                let (left, a) = self.dynamic_parts(values[0].clone(), types[0].clone());
                let (right, b) = self.dynamic_parts(values[1].clone(), types[1].clone());
                if matches!(operator, Operator::Equal | Operator::NotEqual)
                    && matches!((&a, &b), (Value::Object(_), Value::Object(_)))
                {
                    Value::Boolean(if *operator == Operator::Equal {
                        a == b
                    } else {
                        a != b
                    })
                } else {
                    let ty = cyallo_ir::analysis::binary_type(
                        *operator,
                        &left,
                        &right,
                        self.current_span().unwrap_or_default(),
                    )
                    .map_err(|_| E::Operator {
                        operator: *operator,
                        left: left.clone(),
                        right: Some(right.clone()),
                    })?;
                    let operator = if *operator == Operator::Add && ty == Type::String {
                        Operator::Concat
                    } else {
                        *operator
                    };
                    let value = super::binary(operator, &a, &b)?;
                    if result == Type::Dynamic {
                        Value::Dynamic(ty, Box::new(value))
                    } else {
                        value
                    }
                }
            }
        };
        self.set(destination, value);
        Ok(())
    }
    fn dynamic_property(
        &mut self,
        destination: Reg,
        operation: &Operation,
        values: &[Value],
        types: &[Type],
        name: Box<str>,
    ) -> RunResult<()> {
        let object = self.object_id(&values[0], &types[0])?;
        let shape_id = self.object_shape(object);
        let shape = self.objects.domain.get(shape_id);
        let slot = self.property_slot(shape_id, &name)?;
        let value = match operation {
            Operation::Get(_) | Operation::GetKey => {
                let slot = slot.ok_or_else(|| E::MissingProperty(name.clone()))?;
                let Value::ObjectStorage { slots, .. } = &self.heap[object].value else {
                    unreachable!()
                };
                let roots = self.root_scope();
                let property = roots.value(slots[slot].clone());
                let value = self.clone_value(roots.value_copy(property))?;
                let frame = self.frames.last().unwrap();
                let expected =
                    self.program.functions[frame.function].registers[destination].clone();
                self.check_value(value, Type::Dynamic, &expected)?
            }
            Operation::Set(_) | Operation::SetKey => {
                if shape.access == Access::Readonly {
                    return Err(E::Readonly.into());
                }
                let index = values.len() - 1;
                let (mut ty, mut value) =
                    self.dynamic_parts(values[index].clone(), types[index].clone());
                self.validate_dynamic_owner(&value)?;
                let mut properties = shape.fields.to_vec();
                let slot = if let Some(slot) = slot {
                    if let Contract::Required(expected) = &properties[slot].contract {
                        value = self.check_value(value, ty, expected)?;
                        ty = expected.clone();
                        if ty == Type::Dynamic {
                            let unpack = self.dynamic_parts(value, ty);
                            ty = unpack.0;
                            value = unpack.1;
                        }
                    }
                    properties[slot].representation = Self::representation(&value);
                    slot
                } else {
                    let id = self.objects.domain.property(&name)?;
                    let slot = properties.len();
                    properties.push(Property {
                        id,
                        contract: Contract::Configurable,
                        representation: Self::representation(&value),
                    });
                    slot
                };
                let new_shape = self.objects.domain.shape(properties, Access::Mutable)?;
                let replacement = Value::Dynamic(ty, Box::new(value));
                let Value::ObjectStorage { shape, slots } = &mut self.heap[object].value else {
                    unreachable!()
                };
                *shape = new_shape;
                let old = if slot < slots.len() {
                    Some(std::mem::replace(&mut slots[slot], replacement))
                } else {
                    slots.push(replacement);
                    None
                };
                self.refresh_object_native(object);
                self.bump_cell_version(object)?;
                if let Some(old) = old {
                    self.drop_value(old)?;
                }
                Value::ArraySnapshot(vec![])
            }
            Operation::Delete(_) | Operation::DeleteKey => {
                if shape.access == Access::Readonly {
                    return Err(E::Readonly.into());
                }
                if let Some(slot) = slot {
                    if matches!(shape.fields[slot].contract, Contract::Required(_)) {
                        return Err(E::RequiredProperty(name).into());
                    }
                    let mut properties = shape.fields.to_vec();
                    properties.remove(slot);
                    let new_shape = self.objects.domain.shape(properties, Access::Mutable)?;
                    let Value::ObjectStorage { shape, slots } = &mut self.heap[object].value else {
                        unreachable!()
                    };
                    *shape = new_shape;
                    let old = slots.remove(slot);
                    self.refresh_object_native(object);
                    self.bump_cell_version(object)?;
                    self.drop_value(old)?;
                    Value::Boolean(true)
                } else {
                    Value::Boolean(false)
                }
            }
            _ => unreachable!(),
        };
        if self.native_enabled
            && matches!(
                operation,
                Operation::Get(_) | Operation::GetKey | Operation::Set(_) | Operation::SetKey
            )
        {
            let Value::ObjectStorage { slots, .. } = &self.heap[object].value else {
                unreachable!()
            };
            let index = slot.unwrap_or(slots.len() - 1);
            self.warm_property_ic(
                object,
                index,
                destination,
                operation,
                if matches!(operation, Operation::Set(_) | Operation::SetKey) {
                    values.last()
                } else {
                    None
                },
            );
        }
        self.set(destination, value);
        Ok(())
    }
    fn dynamic_index(
        &mut self,
        destination: Reg,
        value: &Value,
        ty: &Type,
        key: Value,
    ) -> RunResult<()> {
        let index = super::integer(key)?;
        let (ty, value) = self.dynamic_parts(value.clone(), ty.clone());
        let (element, value) = match (&ty, value) {
            (Type::Array(bounds, element), Value::ArraySnapshot(values)) if bounds.len() == 1 => {
                let (low, high) = bounds[0];
                let offset = index
                    .checked_sub(low)
                    .and_then(|i| usize::try_from(i).ok())
                    .filter(|i| *i < values.len())
                    .ok_or(super::error::BoundsError::Array {
                        index,
                        lower: low,
                        upper: high,
                    })?;
                (*element.clone(), values[offset].clone())
            }
            (Type::Tuple(types), Value::ArraySnapshot(values)) => {
                let offset = usize::try_from(index)
                    .ok()
                    .filter(|i| *i < values.len())
                    .ok_or(super::error::BoundsError::Collection {
                        index,
                        length: values.len(),
                    })?;
                (types[offset].clone(), values[offset].clone())
            }
            (Type::String, Value::String(value)) => {
                let character = usize::try_from(index)
                    .ok()
                    .and_then(|i| value.chars().nth(i))
                    .ok_or(super::error::BoundsError::Collection {
                        index,
                        length: value.chars().count(),
                    })?;
                (Type::Char, Value::Char(character))
            }
            _ => return Err(E::ExpectedObject(ty).into()),
        };
        let roots = self.root_scope();
        let value = roots.value(value);
        let value = self.clone_value(roots.value_copy(value))?;
        self.set(destination, Value::Dynamic(element, Box::new(value)));
        Ok(())
    }
    fn dynamic_invoke(
        &mut self,
        destination: Reg,
        mut values: Vec<Value>,
        mut types: Vec<Type>,
    ) -> RunResult<()> {
        let (ty, callee) = self.dynamic_parts(values.remove(0), types.remove(0));
        if let Some(target) = Self::callable_target(&callee) {
            let roots = self.root_scope();
            let mut arguments = vec![];
            let mut argument_types = vec![];
            for (value, ty) in values.into_iter().zip(types) {
                let (ty, value) = self.dynamic_parts(value, ty);
                if !cyallo_ir::dynamic::owned(&ty) {
                    return Err(E::BorrowedCall.into());
                }
                self.validate_dynamic_owner(&value)?;
                arguments.push(roots.value(value));
                argument_types.push(ty);
            }
            let request = CallRequest {
                target,
                arguments: argument_types,
                expected: None,
                span: self.current_span().unwrap_or_default(),
            };
            if let Some(adapter) = self
                .objects
                .adapters
                .iter()
                .find(|a| a.request.same_contract(&request))
            {
                return self.call(
                    adapter.function,
                    arguments.into_iter().map(|r| roots.value_copy(r)).collect(),
                    Some(destination),
                );
            }
            return self.compile_dynamic(
                destination,
                vec![request],
                SavedAction::Invoke { roots, arguments },
            );
        }
        let Type::Function(_, parameters, result) = &ty else {
            return Err(E::ExpectedCallable(ty).into());
        };
        if parameters.len() != values.len() {
            return Err(E::ArgumentCount {
                expected: parameters.len(),
                actual: values.len(),
            }
            .into());
        }
        let Value::Function(function, mut captures) = callee else {
            return Err(E::ExpectedCallable(ty).into());
        };
        let roots = self.root_scope();
        let mut arguments = vec![];
        for ((value, actual), (expected, by_ref)) in values.into_iter().zip(types).zip(parameters) {
            if *by_ref {
                return Err(E::BorrowedCall.into());
            }
            arguments.push(roots.value(self.check_value(value, actual, expected)?));
        }
        let generator = self.program.functions[function].generator.is_some();
        let result = *result.clone();
        if generator {
            let copied = self.clone_value(Value::Function(function, captures))?;
            let Value::Function(_, values) = copied else {
                unreachable!()
            };
            captures = values;
        }
        captures.extend(arguments.into_iter().map(|root| roots.value_copy(root)));
        self.call(function, captures, Some(destination))?;
        if generator {
            let value = self.register(destination);
            self.set(destination, Value::Dynamic(result, Box::new(value)));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Context, Status};
    fn run(source: &str) -> Session {
        let program = crate::compile(source).unwrap_or_else(|e| panic!("{e}\n{source}"));
        let mut session = Session::with_context(
            std::sync::Arc::new(program),
            Context {
                gc: crate::runtime::gc::GcConfig::stress(),
                ..Default::default()
            },
        )
        .unwrap();
        for _ in 0..100_000 {
            session.run(1);
            if session.status() != &Status::Paused {
                break;
            }
        }
        session
    }
    #[test]
    fn open_objects_and_dynamic_properties_share_identity() {
        let source = r#"
let item = object { name: "Ada", score: 10 }
let alias = item
alias.active = true
let raw: dynamic = item
std::io::print(raw["score"], ":", raw.active)
raw.name = "Grace"
std::io::print(item.name)
"#;
        let session = run(source);
        assert_eq!(session.status(), &Status::Finished);
        assert_eq!(session.context.output, "10:true\nGrace\n");
    }
    #[test]
    fn configurable_properties_can_change_type_and_be_deleted() {
        let session = run(r#"
let bag = dynamic { score: 10 }
bag.score = "ten"
std::io::print(bag["score"])
std::io::print(delete bag.score)
"#);
        assert_eq!(session.status(), &Status::Finished);
        assert_eq!(session.context.output, "ten\ntrue\n");
    }
    #[test]
    fn dynamic_functions_check_arguments_and_box_results() {
        let session = run(r#"
fn twice(value: i64) -> i64 { return value * 2 }
let callback: dynamic = twice
std::io::print(callback(21))
"#);
        assert_eq!(session.status(), &Status::Finished);
        assert_eq!(session.context.output, "42\n");
    }
    #[test]
    fn structural_views_retain_identity_and_contracts() {
        let session = run(r#"
type Named = object { name: string, .. }
let raw = dynamic { name: "Ada", score: 10 }
let named = std::object::view<Named>(raw).unwrap()
named.name = "Grace"
std::io::print(raw.name, ":", named.score)
"#);
        assert_eq!(session.status(), &Status::Finished);
        assert_eq!(session.context.output, "Grace:10\n");
    }
    #[test]
    fn runtime_load_uses_general_dynamic_properties() {
        let path =
            std::env::temp_dir().join(format!("cyallo-objects-load-{}.ya", std::process::id()));
        std::fs::write(&path,"pub fn twice(value: i64) -> i64 { return value * 2 }\npub const extra = 7\npub fn id<T>(value:T)->T { return value }\npub use std::num::abs as magnitude\npub mod child { pub const value = 11 }\npub record Data { value:i64 }").unwrap();
        let source = format!(
            r#"
type Plugin = object {{ twice: fn(i64) -> i64, .. }}
let module = std::module::load("{}").unwrap()
let key = "twice"
std::io::print(module[key](21))
let plugin = std::object::view<Plugin>(module).unwrap()
std::io::print(plugin.twice(3), ":", plugin.extra)
std::io::print(module.id(9), ":", module.id("yes"), ":", module.magnitude(-4), ":", module.child.value)
type Generic = object {{ id: fn(string)->string, magnitude: fn(i64)->i64, .. }}
let checked = std::object::view<Generic>(module).unwrap()
std::io::print(checked.id("typed"), ":", checked.magnitude(-8), ":", module.Data)
"#,
            path.file_name().unwrap().to_string_lossy()
        );
        let program = crate::compile(&source).unwrap().with_module_loader(
            crate::module_loader::ModuleLoader::new(
                crate::module_loader::ReadGrant::directory(path.parent().unwrap()).unwrap(),
            ),
        );
        let mut session = Session::new(std::sync::Arc::new(program)).unwrap();
        for _ in 0..100_000 {
            session.run(1);
            match session.status() {
                Status::Paused => {}
                Status::Waiting => session.wait_for_wakeup(std::time::Duration::from_millis(1)),
                _ => break,
            }
        }
        let _ = std::fs::remove_file(path);
        assert_eq!(session.status(), &Status::Finished);
        assert_eq!(
            session.context.output,
            "42\n6:7\n9:yes:4:11\ntyped:8:type Data\n"
        );
    }
}
