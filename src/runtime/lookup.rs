//! Bounded per-site plan caches. Entries contain no strong heap references.
use super::*;
use crate::lookup::Operation;
use std::collections::HashMap;
const ELEMENT: usize = 1usize << (usize::BITS - 1);
const TARGET: usize = usize::MAX;
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LookupStats {
    pub hits: u64,
    pub guard_checks: u64,
    pub native_hits: u64,
    pub misses: u64,
    pub mapper_calls: u64,
    pub invalidations: u64,
    pub max_cases: usize,
}
#[derive(Debug, Clone)]
struct Dependency {
    identity: u64,
    index: usize,
    version: u64,
}
#[derive(Debug, Clone, PartialEq)]
enum Atom {
    Tag(u8, u64),
    Text(Arc<str>),
    Type(Type),
}
type Guard = smallvec::SmallVec<[Atom; 16]>;
type Dependencies = smallvec::SmallVec<[Dependency; 4]>;
type Path = smallvec::SmallVec<[usize; 4]>;
struct Entry {
    guard: Guard,
    dependencies: Dependencies,
    path: Option<Path>,
}
struct Pending {
    depth: usize,
    site: (usize, usize),
    destination: Reg,
    guard: Guard,
    receiver_guard: Guard,
    roots: gc::RootScope,
    receiver: gc::RootId,
    cutoff: u64,
    dependencies: Dependencies,
    valid: bool,
}
#[derive(Default)]
pub(super) struct Lookups {
    sites: Vec<Box<[Vec<Entry>]>>,
    pending: Vec<Pending>,
    stats: LookupStats,
}
impl Session {
    pub(super) fn has_pending_lookup(&self) -> bool {
        !self.lookups.pending.is_empty()
    }
    pub(super) fn check_lookup_mapper(
        &self,
        function: usize,
        seen: &mut [bool],
    ) -> Result<(), RuntimeFailure> {
        if std::mem::replace(&mut seen[function], true) {
            return Ok(());
        }
        let frame = self.frames.last().unwrap();
        let span = self.program.functions[frame.function].code[frame.pc.saturating_sub(1)].span;
        let body = self.program.functions[function]
            .checked()
            .map_err(|diagnostic| {
                RuntimeFailure::new(
                    RuntimeError::DeferredCheck(diagnostic.clone()),
                    diagnostic.span,
                )
            })?;
        let invalid = |error: LookupError| RuntimeFailure::new(error.into(), span);
        if !crate::effects::readonly_body(body, &self.program.functions[0], &self.program.services)
        {
            return Err(invalid(LookupError::MapperNotReadonly));
        }
        for instruction in &body.code {
            match &instruction.op {
                crate::ir::Op::Call(_, to, _) => self.check_lookup_mapper(*to, seen)?,
                crate::ir::Op::Service(..) => {
                    return Err(invalid(LookupError::MapperInvokesService));
                }
                crate::ir::Op::TraitCall(_, receiver, _, _)
                    if matches!(body.registers[*receiver], Type::ServiceRef(_)) =>
                {
                    return Err(invalid(LookupError::MapperResolvesServiceBinding));
                }
                crate::ir::Op::Invoke(_, target, _)
                    if cyallo_ast::callable(&body.registers[*target])
                        .is_none_or(|(kind, _, _)| kind != cyallo_ast::RoutineKind::Function) =>
                {
                    return Err(invalid(LookupError::MapperInvokesProcedure));
                }
                _ => {}
            }
        }
        Ok(())
    }
    pub fn lookup_stats(&self) -> LookupStats {
        self.lookups.stats
    }
    fn lookup_fingerprint(&self, value: &Value, out: &mut Guard) -> RunResult<()> {
        match value {
            Value::Generator(id) => out.push(Atom::Tag(48, *id as u64)),
            Value::GeneratorStep(step) => {
                out.push(Atom::Tag(49, u64::from(step.is_complete())));
                self.lookup_fingerprint(step.value(), out)?;
            }
            Value::Layout { size, align } => {
                out.push(Atom::Tag(44, *size as u64));
                out.push(Atom::Tag(45, *align as u64));
            }
            Value::Allocation(id) => out.push(Atom::Tag(46, *id as u64)),
            Value::AllocError(message) => {
                out.push(Atom::Tag(47, 0));
                out.push(Atom::Text(message.clone()));
            }
            Value::Cell(value) => {
                out.push(Atom::Tag(43, 0));
                self.lookup_fingerprint(value, out)?;
            }
            Value::Rc(index) | Value::Weak(index) => out.push(Atom::Tag(42, *index as u64)),
            Value::Slice {
                owner,
                offset,
                length,
            } => {
                out.push(Atom::Tag(39, self.heap[*owner].identity));
                out.push(Atom::Tag(40, *offset as u64));
                out.push(Atom::Tag(41, *length as u64));
            }
            Value::Result(result) => {
                out.push(Atom::Tag(37, u64::from(result.is_ok())));
                self.lookup_fingerprint(
                    match result {
                        Ok(v) | Err(v) => v,
                    },
                    out,
                )?;
            }
            Value::ParseError(message) => {
                out.push(Atom::Tag(38, 0));
                out.push(Atom::Text(message.clone()));
            }
            Value::Range(range) => {
                out.push(Atom::Tag(32, range.start.kind as u64));
                out.push(Atom::Tag(33, range.start.bits));
                out.push(Atom::Tag(34, range.end.bits));
                out.push(Atom::Tag(
                    35,
                    u64::from(range.inclusive) | (u64::from(range.reversed) << 1),
                ));
            }
            Value::Deque(ids) => {
                out.push(Atom::Tag(36, ids.len() as u64));
                for id in ids {
                    self.lookup_fingerprint(&self.heap[*id].value, out)?;
                }
            }
            Value::Number(n) => {
                out.push(Atom::Tag(30, n.kind as u64));
                out.push(Atom::Tag(31, n.bits));
            }
            Value::Integer(n) => out.push(Atom::Tag(1, *n as u64)),
            Value::Real(n) => out.push(Atom::Tag(2, n.to_bits())),
            Value::Boolean(n) => out.push(Atom::Tag(3, u64::from(*n))),
            Value::Char(n) => out.push(Atom::Tag(4, *n as u64)),
            Value::Symbol(n) => out.push(Atom::Tag(5, *n)),
            Value::String(s) => out.push(Atom::Text(s.clone())),
            Value::Managed(id) => out.push(Atom::Tag(6, self.heap[*id].identity)),
            Value::TraitObject(id, implementation) => {
                out.push(Atom::Tag(7, self.heap[*id].identity));
                out.push(Atom::Tag(8, *implementation as u64));
            }
            Value::Function(id, captures) => {
                out.push(Atom::Tag(9, *id as u64));
                out.push(Atom::Tag(10, captures.len() as u64));
                for v in captures {
                    self.lookup_fingerprint(v, out)?
                }
            }
            Value::ArraySnapshot(values) => {
                out.push(Atom::Tag(11, values.len() as u64));
                for v in values {
                    self.lookup_fingerprint(v, out)?
                }
            }
            Value::SparseSnapshot(values, len) => {
                out.push(Atom::Tag(12, *len));
                out.push(Atom::Tag(13, values.len() as u64));
                for (i, v) in values {
                    out.push(Atom::Tag(14, *i as u64));
                    self.lookup_fingerprint(v, out)?
                }
            }
            Value::Optional(v) => {
                out.push(Atom::Tag(15, u64::from(v.is_some())));
                if let Some(v) = v {
                    self.lookup_fingerprint(v, out)?
                }
            }
            Value::Erased(t, v) => {
                out.push(Atom::Type(t.clone()));
                self.lookup_fingerprint(v, out)?
            }
            Value::Context(id) => out.push(Atom::Tag(16, *id as u64)),
            Value::Registration(id) => out.push(Atom::Tag(17, *id as u64)),
            Value::ServiceKey(id) => out.push(Atom::Tag(18, *id as u64)),
            Value::ServiceRef(view, key) => {
                out.push(Atom::Tag(19, *view as u64));
                out.push(Atom::Tag(20, *key as u64));
            }
            Value::Concurrent(_)
            | Value::TaskOutcome(_)
            | Value::SendError(_)
            | Value::Address(_)
            | Value::Array(_)
            | Value::SparseArray(..)
            | Value::StoragePath(..)
            | Value::MissingStoragePath
            | Value::Uninitialized => {
                return Err(LookupError::UninitializedGuard.into());
            }
        }
        Ok(())
    }
    fn observe_lookup(&mut self, index: usize) {
        let cell = &self.heap[index];
        for pending in &mut self.lookups.pending {
            if cell.identity < pending.cutoff
                && !pending
                    .dependencies
                    .iter()
                    .any(|d| d.identity == cell.identity)
            {
                pending.dependencies.push(Dependency {
                    identity: cell.identity,
                    index,
                    version: cell.version,
                });
            }
        }
    }
    pub(super) fn observe_lookup_read(&mut self, index: usize) {
        // Owned-child writes propagate to this cell's aggregate version. One
        // guard therefore protects an entire immutable snapshot in O(1).
        self.observe_lookup(index);
    }
    fn apply_lookup(&mut self, receiver: Value, path: Option<&[usize]>) -> RunResult<Value> {
        let Some(path) = path else {
            return Ok(Value::Optional(None));
        };
        enum Cursor {
            Value(Value),
            Cell(usize),
        }
        let mut cursor = Cursor::Value(receiver);
        for &step in path {
            cursor = match cursor {
                Cursor::Cell(cell) => {
                    if step == TARGET {
                        self.observe_lookup_read(cell);
                        match self.read(cell)? {
                            Value::Managed(cell) => Cursor::Cell(cell),
                            value @ Value::Rc(_) => Cursor::Cell(self.rc_address(&value)?),
                            Value::Cell(value) => Cursor::Value(*value),
                            _ => return Err(expected(ValueKind::Cell)),
                        }
                    } else {
                        let index = step & !ELEMENT;
                        let cell = match &self.heap[cell].value {
                            Value::Array(values) => values.get(index),
                            Value::Deque(values) if step & ELEMENT != 0 => values.get(index),
                            _ => {
                                return Err(Invariant::InvalidStorage.into());
                            }
                        }
                        .copied()
                        .ok_or(BoundsError::StoragePath)?;
                        Cursor::Cell(cell)
                    }
                }
                Cursor::Value(value) => match (value, step) {
                    (Value::Managed(cell), TARGET) => Cursor::Cell(cell),
                    (value @ Value::Rc(_), TARGET) => Cursor::Cell(self.rc_address(&value)?),
                    (Value::Cell(value), TARGET) => Cursor::Value(*value),
                    (Value::ArraySnapshot(values), index) => Cursor::Value(
                        values
                            .get(index & !ELEMENT)
                            .cloned()
                            .ok_or(BoundsError::StoragePath)?,
                    ),
                    _ => return Err(Invariant::InvalidStorage.into()),
                },
            };
        }
        let value = match cursor {
            Cursor::Cell(cell) => {
                self.observe_lookup_read(cell);
                self.read(cell)?
            }
            Cursor::Value(value) => value,
        };
        Ok(Value::Optional(Some(Box::new(self.clone_value(value)?))))
    }
    pub(super) fn finish_lookup(&mut self, value: Value) -> RunResult<Value> {
        let pending = self.lookups.pending.pop().expect("mapper continuation");
        let path = match value {
            Value::MissingStoragePath => None,
            Value::StoragePath(root, path) => {
                let mut guard = Guard::new();
                self.lookup_fingerprint(&root, &mut guard)?;
                if guard != pending.receiver_guard {
                    return Err(LookupError::ForeignPlan.into());
                }
                Some(path.into_iter().collect::<Path>())
            }
            _ => return Err(LookupError::MapperResultNotPlan.into()),
        };
        let receiver = pending.roots.value_copy(pending.receiver);
        let result = self.apply_lookup(receiver, path.as_deref())?;
        if pending.valid {
            let entries = &mut self.lookups.sites[pending.site.0][pending.site.1];
            // Replacing a stale specialization never grows the cache.
            entries.retain(|e| e.guard != pending.guard);
            if entries.len() < 4 {
                entries.push(Entry {
                    guard: pending.guard,
                    dependencies: pending.dependencies,
                    path,
                });
            }
            self.lookups.stats.max_cases = self.lookups.stats.max_cases.max(entries.len());
        }
        Ok(result)
    }
    pub(super) fn lookup_return(&self) -> Option<Reg> {
        self.lookups
            .pending
            .last()
            .filter(|p| p.depth == self.frames.len())
            .map(|p| p.destination)
    }
    pub(super) fn refresh_lookups_after_gc(&mut self) {
        let cells: HashMap<_, _> = self
            .heap
            .iter()
            .enumerate()
            .map(|(i, c)| (c.identity, i))
            .collect();
        for i in 0..self.heap.len() {
            if let Some((identity, _)) = self.heap[i].parent {
                self.heap[i].parent = cells.get(&identity).map(|&index| (identity, index));
            }
        }
        let refresh = |deps: &mut Dependencies| {
            let mut valid = true;
            for d in deps {
                if let Some(&i) = cells.get(&d.identity) {
                    d.index = i
                } else {
                    valid = false
                }
            }
            valid
        };
        for sites in &mut self.lookups.sites {
            for entries in sites.iter_mut() {
                entries.retain_mut(|e| refresh(&mut e.dependencies));
            }
        }
        for pending in &mut self.lookups.pending {
            pending.valid &= refresh(&mut pending.dependencies);
        }
    }
    pub(super) fn reset_lookup_caches(&mut self) {
        self.lookups.sites.clear();
        self.lookups.pending.clear();
    }
    pub(super) fn execute_lookup(
        &mut self,
        destination: Reg,
        operation: Operation,
        args: Vec<Reg>,
    ) -> RunResult<()> {
        let values: Vec<_> = args.iter().map(|r| self.register(*r)).collect();
        let function = self.frames.last().unwrap().function;
        let ty = self.program.functions[function].registers[destination].clone();
        let value = match operation {
            Operation::GeneratorResume(with_input) => {
                let id = self.generator_id(&values[0])?;
                return self.resume_generator(
                    id,
                    with_input.then(|| values[1].clone()),
                    destination,
                );
            }
            Operation::GeneratorClose => {
                let id = self.generator_id(&values[0])?;
                self.close_generator(id)?;
                Value::Uninitialized
            }
            Operation::GeneratorIsComplete | Operation::GeneratorIsYielded => {
                let Value::GeneratorStep(step) = &values[0] else {
                    return Err(expected(ValueKind::GeneratorStep));
                };
                Value::Boolean(
                    step.is_complete() == matches!(operation, Operation::GeneratorIsComplete),
                )
            }
            Operation::GeneratorYielded | Operation::GeneratorCompleted => {
                let Value::GeneratorStep(step) = values[0].clone() else {
                    return Err(expected(ValueKind::GeneratorStep));
                };
                match (step, operation) {
                    (cyallo_values::GeneratorStep::Yielded(value), Operation::GeneratorYielded)
                    | (
                        cyallo_values::GeneratorStep::Complete(value),
                        Operation::GeneratorCompleted,
                    ) => *value,
                    _ => return Err(UnwrapError::StepVariant.into()),
                }
            }
            Operation::LayoutForRc => Value::Layout {
                size: std::mem::size_of::<usize>(),
                align: std::mem::align_of::<usize>(),
            },
            Operation::LayoutNew => {
                let size = usize::try_from(integer(values[0].clone())?)
                    .map_err(|_| AllocatorError::InvalidSize)?;
                let align = usize::try_from(integer(values[1].clone())?)
                    .map_err(|_| AllocatorError::InvalidAlignment)?;
                Value::Result(
                    std::alloc::Layout::from_size_align(size, align)
                        .map(|_| Box::new(Value::Layout { size, align }))
                        .map_err(|_| Box::new(Value::AllocError("invalid layout".into()))),
                )
            }
            Operation::LayoutSize | Operation::LayoutAlign => {
                let Value::Layout { size, align } = values[0] else {
                    return Err(expected(ValueKind::Layout));
                };
                Value::from_number(cyallo_ast::Number::integer(
                    cyallo_ast::NumberKind::Usize,
                    if matches!(operation, Operation::LayoutSize) {
                        size
                    } else {
                        align
                    } as i128,
                )?)
            }
            Operation::Allocate => {
                let Value::Layout { size, align } = values[0] else {
                    return Err(expected(ValueKind::Layout));
                };
                Value::Result(
                    self.allocate_backing(size, align)
                        .map(|id| Box::new(Value::Allocation(id)))
                        .map_err(|error| Box::new(Value::AllocError(error.to_string().into()))),
                )
            }
            Operation::Deallocate => {
                let Value::Allocation(id) = values[0] else {
                    return Err(expected(ValueKind::Allocation));
                };
                self.deallocate_backing(id);
                Value::Uninitialized
            }
            Operation::AllocErrorNew => {
                let Value::String(message) = &values[0] else {
                    return Err(expected(ValueKind::AllocError));
                };
                Value::AllocError(message.clone())
            }
            Operation::ResultNew(ok) => Value::Result(if ok {
                Ok(Box::new(values[0].clone()))
            } else {
                Err(Box::new(values[0].clone()))
            }),
            Operation::RcNewIn {
                deallocate,
                borrowed,
                fallible,
            } => {
                let owner_ty = if fallible {
                    let Type::Storage(cyallo_ast::StorageKind::Result, inner) = &ty else {
                        return Err(AllocatorError::ResultNotAllocation.into());
                    };
                    let Type::Tuple(types) = inner.as_ref() else {
                        return Err(AllocatorError::ResultNotAllocation.into());
                    };
                    &types[0]
                } else {
                    &ty
                };
                let Type::Managed { pointee, .. } = owner_ty else {
                    return Err(Invariant::InvalidManagedType.into());
                };
                let allocator_ty = self.program.functions[function].registers[args[0]].clone();
                self.managed_new_in(
                    pointee,
                    &allocator_ty,
                    super::memory::NewRc {
                        allocator: values[0].clone(),
                        allocation: values[1].clone(),
                        value: values[2].clone(),
                        deallocate,
                        borrowed,
                        fallible,
                    },
                )?
            }

            Operation::RcClone => self.rc_clone(&values[0], false, false)?,
            Operation::CloneValue => {
                let value = self.owner_value(&values[0])?;
                self.clone_value(value)?
            }
            Operation::RcDowngrade => self.rc_clone(&values[0], true, false)?,
            Operation::RcUpgrade => self.rc_clone(&values[0], false, true)?,
            Operation::RcStrongCount | Operation::RcWeakCount => {
                Value::from_number(cyallo_ast::Number::integer(
                    cyallo_ast::NumberKind::Usize,
                    self.rc_count(&values[0], matches!(operation, Operation::RcWeakCount))? as i128,
                )?)
            }
            Operation::GcClone => self.owner_value(&values[0])?,
            Operation::DropValue => {
                self.drop_value(values[0].clone())?;
                Value::Uninitialized
            }
            Operation::ManagedNew(kind, _) => {
                let Type::Managed { pointee, .. } = &ty else {
                    return Err(Invariant::InvalidManagedType.into());
                };
                self.managed_new(kind, pointee, values[0].clone())?
            }
            Operation::Slice(_) => match values[0] {
                Value::Slice { .. } => values[0].clone(),
                Value::Address(owner) => {
                    let Value::Array(elements) = &self.heap[owner].value else {
                        return Err(Invariant::InvalidSliceStorage.into());
                    };
                    Value::Slice {
                        owner,
                        offset: 0,
                        length: elements.len(),
                    }
                }
                _ => return Err(expected(ValueKind::Address)),
            },
            Operation::Parse(kind) => {
                let source = text(&values[0])?;
                Value::Result(
                    cyallo_ast::Number::parse(kind, source)
                        .map(|number| Box::new(Value::from_number(number)))
                        .map_err(|message| Box::new(Value::ParseError(message.to_string().into()))),
                )
            }
            Operation::IsOk | Operation::IsErr => {
                let Value::Result(result) = &values[0] else {
                    return Err(expected(ValueKind::Result));
                };
                Value::Boolean(result.is_ok() != matches!(operation, Operation::IsErr))
            }
            Operation::UnwrapOk | Operation::UnwrapErr => {
                let Value::Result(result) = &values[0] else {
                    return Err(expected(ValueKind::Result));
                };
                match (result, operation) {
                    (Ok(value), Operation::UnwrapOk) | (Err(value), Operation::UnwrapErr) => {
                        *value.clone()
                    }
                    (Err(error), _) => {
                        return Err(UnwrapError::Err(error.to_string().into()).into());
                    }
                    (Ok(_), _) => return Err(UnwrapError::UnwrapErrOnOk.into()),
                }
            }
            Operation::CollectionLiteral(_) => Value::ArraySnapshot(values),
            Operation::Range(inclusive) => {
                let start = values[0].as_number().ok_or(expected(ValueKind::Integer))?;
                let end = values[1].as_number().ok_or(expected(ValueKind::Integer))?;
                Value::Range(cyallo_values::RangeValue {
                    start,
                    end,
                    inclusive,
                    reversed: false,
                })
            }
            Operation::RangeReverse => {
                let Value::Range(range) = values[0] else {
                    return Err(expected(ValueKind::Range));
                };
                Value::Range(cyallo_values::RangeValue {
                    reversed: !range.reversed,
                    ..range
                })
            }
            Operation::CollectionFrom(_) | Operation::ArrayFrom => {
                let Value::Range(range) = values[0] else {
                    return Err(expected(ValueKind::Range));
                };
                let start = range.start.int();
                let end = range.end.int() + i128::from(range.inclusive);
                let count = (end - start).max(0);
                if count > self.context.max_cells as i128 {
                    return Err(ResourceLimit::Storage.into());
                }
                let number = |n| {
                    cyallo_ast::Number::integer(range.start.kind, n)
                        .map(Value::from_number)
                        .map_err(RuntimeError::from)
                };
                let elements = if range.reversed {
                    (start..end)
                        .rev()
                        .map(number)
                        .collect::<RunResult<Vec<_>>>()?
                } else {
                    (start..end).map(number).collect::<RunResult<Vec<_>>>()?
                };
                Value::ArraySnapshot(elements)
            }
            Operation::CollectionLength => {
                if let Value::Slice { length, .. } = values[0] {
                    self.set(
                        destination,
                        Value::from_number(cyallo_ast::Number::integer(
                            cyallo_ast::NumberKind::Usize,
                            length as i128,
                        )?),
                    );
                    return Ok(());
                }
                let Value::Address(id) = values[0] else {
                    return Err(expected(ValueKind::Address));
                };
                let len = match &self.heap[id].value {
                    Value::Array(ids) => ids.len(),
                    Value::Deque(ids) => ids.len(),
                    _ => return Err(UninitializedRead::Collection.into()),
                };
                Value::from_number(cyallo_ast::Number::integer(
                    cyallo_ast::NumberKind::Usize,
                    len as i128,
                )?)
            }
            Operation::CollectionPush(front) => {
                let Value::Address(id) = values[0] else {
                    return Err(expected(ValueKind::Address));
                };
                let Type::Storage(_, element) = self.heap[id].ty.clone() else {
                    return Err(Invariant::InvalidCollectionType.into());
                };
                let roots = self.root_scope();
                let parent = roots.cell(id);
                let source = roots.value(values[1].clone());
                let child = self.alloc_for_cell(&element, id)?;
                self.attach_child(child, roots.cell_id(parent));
                match &mut self.heap[roots.cell_id(parent)].value {
                    Value::Array(ids) if !front => ids.push(child),
                    Value::Deque(ids) => {
                        if front {
                            ids.push_front(child)
                        } else {
                            ids.push_back(child)
                        }
                    }
                    _ => return Err(Invariant::InvalidCollectionType.into()),
                }
                self.store(child, roots.value_copy(source))?;
                self.bump_cell_version(roots.cell_id(parent))?;
                Value::Uninitialized
            }
            Operation::CollectionPop(front) => {
                let Value::Address(id) = values[0] else {
                    return Err(expected(ValueKind::Address));
                };
                let child = match &mut self.heap[id].value {
                    Value::Array(ids) if !front => ids.pop(),
                    Value::Deque(ids) => {
                        if front {
                            ids.pop_front()
                        } else {
                            ids.pop_back()
                        }
                    }
                    _ => return Err(Invariant::InvalidCollectionType.into()),
                };
                self.bump_cell_version(id)?;
                Value::Optional(
                    child
                        .map(|id| self.take_cell(id).map(Box::new))
                        .transpose()?,
                )
            }
            Operation::Symbol => {
                let id = self.next_symbol;
                self.next_symbol = self
                    .next_symbol
                    .checked_add(1)
                    .ok_or(IdentitySpace::Symbol)?;
                Value::Symbol(id)
            }
            Operation::Some => Value::Optional(Some(Box::new(values[0].clone()))),
            Operation::Empty => Value::ArraySnapshot(Vec::new()),
            Operation::None => Value::Optional(None),
            Operation::Erase => Value::Erased(
                self.program.functions[function].registers[args[0]].clone(),
                Box::new(values[0].clone()),
            ),
            Operation::Checked => {
                let Value::Erased(actual, v) = &values[0] else {
                    return Err(expected(ValueKind::Any));
                };
                if !cyallo_ir::memory::same_regions(actual, &ty) {
                    return Err(RuntimeError::CheckedTypeMismatch);
                }
                *v.clone()
            }
            Operation::IsSome | Operation::IsNone => {
                let Value::Optional(v) = &values[0] else {
                    return Err(expected(ValueKind::Option));
                };
                Value::Boolean(v.is_some() != matches!(operation, Operation::IsNone))
            }
            Operation::Unwrap => {
                let Value::Optional(v) = &values[0] else {
                    return Err(expected(ValueKind::Option));
                };
                *v.clone().ok_or_else(|| match values.get(1) {
                    Some(message) => UnwrapError::Expect(message.to_string().into()),
                    None => UnwrapError::NoneValue,
                })?
            }
            Operation::CellNew => Value::Cell(Box::new(values[0].clone())),
            Operation::CellRead => {
                let Value::Address(cell) = values[0] else {
                    return Err(expected(ValueKind::Address));
                };
                self.observe_lookup_read(cell);
                let Value::Cell(value) = &self.heap[cell].value else {
                    return Err(expected(ValueKind::Cell));
                };
                // Type analysis permits only Copy contents here.
                *value.clone()
            }
            Operation::CellWrite | Operation::CellReplace => {
                let Value::Address(cell) = values[0] else {
                    return Err(expected(ValueKind::Address));
                };
                let old = std::mem::replace(
                    &mut self.heap[cell].value,
                    Value::Cell(Box::new(values[1].clone())),
                );
                self.bump_cell_version(cell)?;
                if matches!(operation, Operation::CellReplace) {
                    let Value::Cell(value) = old else {
                        return Err(expected(ValueKind::Cell));
                    };
                    *value
                } else {
                    self.drop_value(old)?;
                    Value::Uninitialized
                }
            }
            Operation::LocationLoad => {
                let Value::Managed(id) = values[0] else {
                    return Err(expected(ValueKind::Location));
                };
                self.observe_lookup_read(id);
                let value = self.read(id)?;
                self.clone_value(value)?
            }
            Operation::LocationStore => {
                let Value::Managed(id) = values[0] else {
                    return Err(expected(ValueKind::Location));
                };
                self.store(id, values[1].clone())?;
                Value::Uninitialized
            }
            Operation::StoragePathNew => Value::StoragePath(Box::new(values[0].clone()), vec![]),
            Operation::Missing => Value::MissingStoragePath,
            Operation::Target => {
                let Value::StoragePath(root, mut path) = values[0].clone() else {
                    return Err(LookupError::MissingPlan.into());
                };
                path.push(TARGET);
                Value::StoragePath(root, path)
            }
            Operation::Field(_) | Operation::Element
                if matches!(&values[0], Value::StoragePath(..)) =>
            {
                let Value::StoragePath(root, mut path) = values[0].clone() else {
                    unreachable!()
                };
                let step = match operation {
                    Operation::Field(index) => index,
                    _ => {
                        usize::try_from(integer(values[1].clone())?)
                            .map_err(|_| LookupError::NegativePlanIndex)?
                            | ELEMENT
                    }
                };
                path.push(step);
                Value::StoragePath(root, path)
            }
            Operation::Field(index) => {
                let Value::Managed(parent) = self.owner_value(&values[0])? else {
                    return Err(expected(ValueKind::Record));
                };
                let Value::Array(fields) = &self.heap[parent].value else {
                    return Err(expected(ValueKind::Record));
                };
                Value::Managed(*fields.get(index).ok_or(Invariant::UnknownRecordField)?)
            }
            Operation::Element => {
                return Err(LookupError::MissingPlan.into());
            }
            Operation::Mapped => {
                let mut guard = Guard::new();
                for v in &values {
                    self.lookup_fingerprint(v, &mut guard)?;
                }
                let site = (function, self.frames.last().unwrap().pc - 1);
                if self.lookups.sites.len() <= function {
                    self.lookups.sites.resize_with(function + 1, Box::default);
                }
                if self.lookups.sites[function].is_empty() {
                    self.lookups.sites[function] = std::iter::repeat_with(Vec::new)
                        .take(self.program.functions[function].code.len())
                        .collect();
                }
                let entry = self.lookups.sites[function][site.1]
                    .iter()
                    .find(|e| e.guard == guard);
                if let Some(entry) = entry {
                    self.lookups.stats.guard_checks += entry.dependencies.len() as u64;
                    let valid = entry.dependencies.iter().all(|d| {
                        d.index < self.heap.len()
                            && self.heap[d.index].identity == d.identity
                            && self.heap[d.index].version == d.version
                    });
                    if valid {
                        let path = entry.path.clone();
                        let deps = entry.dependencies.clone();
                        for d in deps {
                            self.observe_lookup(d.index);
                        }
                        self.lookups.stats.hits += 1;
                        if self.execution_engine != ExecutionEngine::Interpreter {
                            self.lookups.stats.native_hits += 1
                        }
                        let value = self.apply_lookup(values[0].clone(), path.as_deref())?;
                        self.set(destination, value);
                        return Ok(());
                    }
                    self.lookups.stats.invalidations += 1;
                }
                self.lookups.stats.misses += 1;
                self.lookups.stats.mapper_calls += 1;
                let Value::Function(mapper, mut captures) = values[2].clone() else {
                    return Err(expected(ValueKind::Callable));
                };
                if let Err(failure) =
                    self.check_lookup_mapper(mapper, &mut vec![false; self.program.functions.len()])
                {
                    self.status = Status::Failed(failure);
                    return Ok(());
                }
                let mut receiver_guard = Guard::new();
                self.lookup_fingerprint(&values[0], &mut receiver_guard)?;
                let roots = self.root_scope();
                let receiver = roots.value(values[0].clone());
                let depth = self.frames.len() + 1;
                self.lookups.pending.push(Pending {
                    depth,
                    site,
                    destination,
                    guard,
                    receiver_guard,
                    roots,
                    receiver,
                    cutoff: self.next_cell_identity,
                    dependencies: Dependencies::new(),
                    valid: true,
                });
                captures.extend_from_slice(&values[..2]);
                self.call(mapper, captures, Some(destination))?;
                return Ok(());
            }
        };
        self.set(destination, value);
        Ok(())
    }
}
