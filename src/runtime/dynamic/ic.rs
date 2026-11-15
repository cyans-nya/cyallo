//! Session-owned native views. Generated code never embeds a heap address or a
//! Session-local shape identity in an executable shared by other Sessions.
use super::*;
use crate::runtime::native::{self, NativeSlot};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ObjectIcMode {
    #[default]
    Enabled,
    Disabled,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(C)]
pub struct ObjectStats {
    pub native_reads: u64,
    pub native_writes: u64,
    pub native_misses: u64,
    pub lookups: u64,
    pub shape_misses: u64,
    pub megamorphic_misses: u64,
}
#[derive(Default, Clone, Copy)]
#[repr(C)]
pub(crate) struct NativeCase {
    pub shape: u64,
    pub slot: u64,
    pub kind: u64,
    pub result_kind: u64,
    pub input_kind: u64,
}
#[derive(Default)]
#[repr(C)]
pub(crate) struct NativeCache {
    pub key: u64,
    pub cases: [NativeCase; 4],
}
#[derive(Default)]
#[repr(C)]
pub(crate) struct NativeObject {
    pub shape: u64,
    pub slots: *const NativeSlot,
    pub length: u64,
}
struct Mirror {
    header: NativeObject,
    slots: Box<[NativeSlot]>,
}
#[derive(Default)]
enum Preparation {
    #[default]
    Cold,
    Empty,
    Ready,
}
#[derive(Default)]
enum FunctionCaches {
    #[default]
    Cold,
    Absent,
    Ready(Box<[NativeCache]>),
}
#[derive(Default)]
pub(super) struct NativeObjects {
    rows: Vec<Option<Box<Mirror>>>,
    pointers: Vec<*const NativeObject>,
    caches: Vec<FunctionCaches>,
    pub(super) stats: Box<ObjectStats>,
    mode: ObjectIcMode,
    preparation: Preparation,
}
// SAFETY: pointers borrow the boxed mirrors owned here. Only an exclusive
// Session executes native code; every helper/collection refreshes its view.
unsafe impl Send for NativeObjects {}
impl NativeObjects {
    fn refresh(&mut self, id: usize, value: &Value) {
        if self.rows.len() <= id {
            self.rows.resize_with(id + 1, || None);
            self.pointers.resize(id + 1, std::ptr::null());
        }
        let Value::ObjectStorage { shape, slots } = value else {
            self.rows[id] = None;
            self.pointers[id] = std::ptr::null();
            return;
        };
        let values: Box<[_]> = slots.iter().map(NativeSlot::from_value).collect();
        if let Some(mirror) = &mut self.rows[id] {
            mirror.slots = values;
            mirror.header = NativeObject {
                shape: shape.0 as u64 + 1,
                slots: mirror.slots.as_ptr(),
                length: mirror.slots.len() as u64,
            };
        } else {
            let header = NativeObject {
                shape: shape.0 as u64 + 1,
                slots: values.as_ptr(),
                length: values.len() as u64,
            };
            self.rows[id] = Some(Box::new(Mirror {
                header,
                slots: values,
            }));
        }
        self.pointers[id] = &self.rows[id].as_ref().unwrap().header;
    }
    pub(super) fn reset_caches(&mut self) {
        self.caches.clear();
    }
    fn function_caches(
        &mut self,
        function: usize,
        words: usize,
        properties: bool,
    ) -> Option<&mut [NativeCache]> {
        if self.caches.len() <= function {
            self.caches
                .resize_with(function + 1, FunctionCaches::default);
        }
        if matches!(&self.caches[function], FunctionCaches::Cold)
            || matches!(&self.caches[function],FunctionCaches::Ready(cache) if cache.len()!=words)
        {
            self.caches[function] = if properties {
                FunctionCaches::Ready((0..words).map(|_| NativeCache::default()).collect())
            } else {
                FunctionCaches::Absent
            };
        }
        match &mut self.caches[function] {
            FunctionCaches::Ready(cache) => Some(cache),
            _ => None,
        }
    }
    fn cache(&mut self, function: usize, words: usize, pc: usize) -> &mut NativeCache {
        &mut self
            .function_caches(function, words, true)
            .expect("resolved property site")[pc]
    }
}
impl Session {
    pub fn object_stats(&self) -> ObjectStats {
        *self.objects.native.stats
    }
    pub fn set_object_ic_mode(&mut self, mode: ObjectIcMode) {
        self.objects.native.mode = mode;
    }
    pub(crate) fn rebuild_object_native(&mut self) {
        let native = &mut self.objects.native;
        native.rows.clear();
        native.pointers.clear();
        for (id, cell) in self.heap.iter().enumerate() {
            if matches!(cell.value, Value::ObjectStorage { .. }) {
                native.refresh(id, &cell.value);
            }
        }
        native.preparation = if native.rows.is_empty() {
            Preparation::Empty
        } else {
            Preparation::Ready
        };
    }
    pub(crate) fn prepare_object_native(&mut self) {
        if matches!(self.objects.native.preparation, Preparation::Cold) {
            self.rebuild_object_native();
        }
        let native = &mut self.objects.native;
        let cache = if let Some(frame) = self.frames.last() {
            let function = frame.function;
            let routine = &self.program.functions[function];
            let properties = if native
                .caches
                .get(function)
                .is_none_or(|cache| matches!(cache, FunctionCaches::Cold))
            {
                routine.code.iter().any(|inst| match &inst.op {
                    crate::ir::Op::Dynamic(
                        destination,
                        Operation::Get(_) | Operation::GetKey,
                        _,
                    ) => native::property_scalar_candidate(&routine.registers[*destination]),
                    crate::ir::Op::Dynamic(_, Operation::Set(_) | Operation::SetKey, args) => {
                        native::property_scalar_candidate(&routine.registers[*args.last().unwrap()])
                    }
                    _ => false,
                })
            } else {
                true
            };
            native
                .function_caches(function, routine.code.len(), properties)
                .map_or(std::ptr::null_mut(), |cache| cache.as_mut_ptr())
        } else {
            std::ptr::null_mut()
        };
        self.native_view.objects = native.pointers.as_ptr();
        self.native_view.object_count = native.pointers.len() as u64;
        self.native_view.property_caches = if native.mode == ObjectIcMode::Enabled {
            cache
        } else {
            std::ptr::null_mut()
        };
        self.native_view.object_stats = native.stats.as_mut();
    }
    pub(super) fn refresh_object_native(&mut self, id: usize) {
        if self.native_enabled {
            self.objects.native.refresh(id, &self.heap[id].value);
            self.objects.native.preparation = Preparation::Ready;
        }
    }
    pub(super) fn property_key(&mut self, key: Arc<str>) {
        let frame = self.frames.last().unwrap();
        let site = self
            .objects
            .sites
            .entry((frame.function, frame.pc - 1))
            .or_default();
        if site
            .key_value
            .as_ref()
            .is_none_or(|old| old.as_ref() != key.as_ref())
        {
            site.cases.clear();
        }
        site.key_value = Some(key);
    }
    pub(super) fn warm_property_ic(
        &mut self,
        object: usize,
        slot: usize,
        destination: Reg,
        operation: &Operation,
        input: Option<&Value>,
    ) {
        let frame = self.frames.last().unwrap();
        let fi = frame.function;
        let pc = frame.pc - 1;
        let words = self.program.functions[fi].code.len();
        let shape = self.object_shape(object);
        let descriptor = self.objects.domain.get(shape);
        let Value::ObjectStorage { slots, .. } = &self.heap[object].value else {
            unreachable!()
        };
        let slot_kind = NativeSlot::from_value(&slots[slot]).kind;
        let Some(bare_kind) = bare_scalar(slot_kind) else {
            return;
        };
        let expected = &self.program.functions[fi].registers[destination];
        let result_kind = match operation {
            Operation::Get(_) | Operation::GetKey if *expected == Type::Dynamic => slot_kind,
            Operation::Get(_) | Operation::GetKey
                if scalar_type(bare_kind).as_ref() == Some(expected) =>
            {
                bare_kind
            }
            _ => 0,
        };
        let input_kind = match input {
            Some(value) if descriptor.access == Access::Mutable => {
                let kind = NativeSlot::from_value(value).kind;
                if bare_scalar(kind).unwrap_or(kind) == bare_kind {
                    kind
                } else {
                    0
                }
            }
            _ => 0,
        };
        if result_kind == 0 && input_kind == 0 {
            return;
        }
        let key = self
            .objects
            .sites
            .get(&(fi, pc))
            .and_then(|site| site.key_value.as_ref())
            .map_or(0, |key| key.as_ptr() as u64);
        let cache = self.objects.native.cache(fi, words, pc);
        if cache.key != key {
            cache.cases.fill(NativeCase::default());
            cache.key = key;
        }
        if let Some(case) = cache
            .cases
            .iter_mut()
            .find(|case| case.shape == shape.0 as u64 + 1 || case.shape == 0)
        {
            *case = NativeCase {
                shape: shape.0 as u64 + 1,
                slot: slot as u64,
                kind: slot_kind,
                result_kind,
                input_kind,
            };
        }
    }
    /// The generated guard has proven a mutable, same-representation scalar
    /// write. Synchronization before this entry cannot collect or change shapes.
    pub(crate) fn native_property_step(&mut self) {
        let span = self.current_span().unwrap_or_default();
        let object = self.native_view.property_object as usize;
        let slot = self.native_view.property_slot as usize;
        let input = self.native_view.property_input as usize;
        let destination = self.native_view.property_destination as usize;
        self.frames.last_mut().unwrap().pc += 1;
        self.record_work(1, self.execution_engine);
        let value = self.register(input);
        let ty =
            self.program.functions[self.frames.last().unwrap().function].registers[input].clone();
        let (ty, value) = self.dynamic_parts(value, ty);
        let Value::ObjectStorage { slots, .. } = &mut self.heap[object].value else {
            unreachable!("guarded object")
        };
        slots[slot] = Value::Dynamic(ty, Box::new(value));
        self.objects.native.rows[object].as_mut().unwrap().slots[slot] =
            NativeSlot::from_value(&slots[slot]);
        self.objects.native.stats.native_writes =
            self.objects.native.stats.native_writes.saturating_add(1);
        if let Err(error) = self.bump_cell_version(object) {
            self.fail(error, span);
            return;
        }
        self.set(destination, Value::ArraySnapshot(vec![]));
    }
}
pub(super) fn bare_scalar(kind: u64) -> Option<u64> {
    Some(match kind {
        native::DYNAMIC_INTEGER => native::INTEGER,
        native::DYNAMIC_REAL => native::REAL,
        native::DYNAMIC_BOOLEAN => native::BOOLEAN,
        native::DYNAMIC_CHAR => native::CHAR,
        _ => return None,
    })
}
fn scalar_type(kind: u64) -> Option<Type> {
    Some(match kind {
        native::INTEGER => Type::Integer,
        native::REAL => Type::Real,
        native::BOOLEAN => Type::Boolean,
        native::CHAR => Type::Char,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scalar_only_native_entries_do_not_allocate_object_views_or_site_arrays() {
        let mut session = Session::new(Arc::new(
            crate::compile("let number=42\nstd::io::print(number)").unwrap(),
        ))
        .unwrap();
        session.prepare_native();
        assert!(session.objects.native.rows.is_empty());
        assert!(session.objects.native.pointers.is_empty());
        assert!(matches!(
            session.objects.native.caches[0],
            FunctionCaches::Absent
        ));
        assert!(session.native_view.property_caches.is_null());
        assert_eq!(session.native_view.object_count, 0);
    }
    #[test]
    fn preparing_a_caller_preserves_its_callee_feedback() {
        let mut native = NativeObjects::default();
        native.function_caches(5, 8, true).unwrap()[3].cases[0].shape = 41;
        assert!(native.function_caches(0, 12, false).is_none());
        assert_eq!(
            native.function_caches(5, 8, true).unwrap()[3].cases[0].shape,
            41
        );
    }
}
