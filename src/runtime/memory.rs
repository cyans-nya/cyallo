//! Session-local reference-counted owners. VM copies of an owner token do not
//! clone language ownership; explicit clone creates a distinct counted token.
use super::*;
use std::{
    alloc::{self, Layout},
    ptr::NonNull,
};
/// Tokens include a generation so an obsolete VM snapshot cannot release a
/// newly allocated owner when a table slot is reused.
struct TokenSlot<T> {
    generation: u32,
    value: Option<T>,
}
struct TokenPool<T> {
    slots: Vec<TokenSlot<T>>,
    free: Vec<u32>,
}
impl<T> Default for TokenPool<T> {
    fn default() -> Self {
        Self {
            slots: vec![],
            free: vec![],
        }
    }
}
impl<T> TokenPool<T> {
    fn insert(&mut self, value: T) -> usize {
        let index = self.free.pop().unwrap_or_else(|| {
            let index = u32::try_from(self.slots.len()).expect("owner token capacity");
            self.slots.push(TokenSlot {
                generation: 0,
                value: None,
            });
            index
        });
        let slot = &mut self.slots[index as usize];
        slot.value = Some(value);
        ((slot.generation as usize) << 32) | index as usize
    }
    fn get(&self, token: usize) -> Option<&T> {
        self.slots
            .get(token as u32 as usize)
            .filter(|slot| slot.generation == (token >> 32) as u32)?
            .value
            .as_ref()
    }
    fn remove(&mut self, token: usize) -> Option<T> {
        let index = token as u32;
        let slot = self
            .slots
            .get_mut(index as usize)
            .filter(|slot| slot.generation == (token >> 32) as u32)?;
        let value = slot.value.take()?;
        if let Some(next) = slot.generation.checked_add(1) {
            slot.generation = next;
            self.free.push(index);
        }
        Some(value)
    }
}
pub(super) struct NewRc {
    pub allocator: Value,
    pub allocation: Value,
    pub value: Value,
    pub deallocate: usize,
    pub borrowed: bool,
    pub fallible: bool,
}
struct Allocation {
    pointer: NonNull<u8>,
    layout: Layout,
}
// SAFETY: this is a uniquely owned allocation from std::alloc. Its pointer is
// never exposed as a Rust borrow across Session calls; ownership and deallocation
// may move to another execution thread together with the owning Session.
unsafe impl Send for Allocation {}
impl Drop for Allocation {
    fn drop(&mut self) {
        // SAFETY: each backing block has one table owner and its original layout.
        unsafe {
            alloc::dealloc(self.pointer.as_ptr(), self.layout);
        }
    }
}
struct AllocatorState {
    roots: gc::RootScope,
    cell: gc::RootId,
    deallocate: usize,
    borrowed: bool,
}
struct Finalizer {
    block: usize,
    allocation: usize,
}

struct Handle {
    block: usize,
    weak: bool,
}
struct Payload {
    roots: gc::RootScope,
    cell: gc::RootId,
}
struct ControlBlock {
    payload: Option<Payload>,
    storage: Option<usize>,
    allocator: Option<AllocatorState>,
    finalizing: bool,
    strong: usize,
    weak: usize,
}
#[derive(Default)]
pub(super) struct Memory {
    handles: TokenPool<Handle>,
    blocks: Vec<Option<ControlBlock>>,
    allocations: TokenPool<Allocation>,
    allocated_bytes: usize,
    free_blocks: Vec<usize>,
    finalizers: VecDeque<Finalizer>,
}
impl Session {
    pub(super) fn relink_memory(&mut self, functions: &[usize]) {
        for state in self
            .memory
            .blocks
            .iter_mut()
            .filter_map(Option::as_mut)
            .filter_map(|block| block.allocator.as_mut())
        {
            state.deallocate = functions[state.deallocate];
        }
    }
    pub(super) fn allocate_backing(&mut self, size: usize, alignment: usize) -> RunResult<usize> {
        let layout = Layout::from_size_align(size.max(1), alignment)
            .map_err(|_| AllocatorError::InvalidLayout)?;
        let limit = self
            .context
            .max_cells
            .saturating_mul(std::mem::size_of::<Cell>());
        let total = self
            .memory
            .allocated_bytes
            .checked_add(layout.size())
            .ok_or(AllocatorError::SizeOverflow)?;
        if total > limit {
            return Err(ResourceLimit::Allocation.into());
        }
        // SAFETY: Layout validates size/alignment. Null is a recoverable failure.
        let pointer = NonNull::new(unsafe { alloc::alloc_zeroed(layout) })
            .ok_or(AllocatorError::AllocationFailed)?;
        self.memory.allocated_bytes = total;
        let id = self
            .memory
            .allocations
            .insert(Allocation { pointer, layout });
        Ok(id)
    }
    pub(super) fn deallocate_backing(&mut self, id: usize) {
        if let Some(allocation) = self.memory.allocations.remove(id) {
            self.memory.allocated_bytes -= allocation.layout.size();
            drop(allocation);
        }
    }
    fn rc_payload(&self, cell: usize, allocation: usize) -> RunResult<Payload> {
        let storage = self
            .memory
            .allocations
            .get(allocation)
            .ok_or(OwnershipError::ReleasedAllocation)?;
        if storage.layout.size() < std::mem::size_of::<usize>()
            || storage.layout.align() < std::mem::align_of::<usize>()
        {
            return Err(AllocatorError::InsufficientBlock.into());
        }
        let pointer = storage.pointer.cast::<usize>();
        // SAFETY: the checked backing block holds this live descriptor until the
        // payload root scope is dropped before deallocation. GC exclusively edits it.
        unsafe {
            pointer.as_ptr().write(cell);
        }
        let roots = self.root_scope();
        let cell = unsafe { roots.external_cell(pointer.as_ptr()) };
        Ok(Payload { roots, cell })
    }
    pub(super) fn managed_new_in(
        &mut self,
        ty: &Type,
        allocator_ty: &Type,
        request: NewRc,
    ) -> RunResult<Value> {
        let NewRc {
            allocator,
            allocation: allocated,
            value,
            deallocate,
            borrowed,
            fallible,
        } = request;
        let Value::Result(allocated) = allocated else {
            return Err(AllocatorError::InvalidResultType.into());
        };
        let allocation = match allocated {
            Ok(value) => match *value {
                Value::Allocation(id) => id,
                _ => return Err(AllocatorError::ResultNotAllocation.into()),
            },
            Err(error) => {
                self.drop_value(value)?;
                self.drop_value(allocator)?;
                return if fallible {
                    Ok(Value::Result(Err(error)))
                } else {
                    Err(AllocatorError::Rejected(error.to_string().into()).into())
                };
            }
        };
        let roots = self.root_scope();
        let value = roots.value(value);
        let allocator = roots.value(allocator);
        let previous = self.allocation_owner;
        self.allocation_owner = None;
        let allocated_cells = (|| {
            let cell = roots.cell(self.alloc(ty)?);
            self.store(roots.cell_id(cell), roots.value_copy(value))?;
            let state = roots.cell(self.alloc(allocator_ty)?);
            self.store(roots.cell_id(state), roots.value_copy(allocator))?;
            Ok::<_, RuntimeError>((cell, state))
        })();
        self.allocation_owner = previous;
        let (cell, state) = allocated_cells?;
        let payload = self.rc_payload(roots.cell_id(cell), allocation)?;
        let state_roots = self.root_scope();
        let state_cell = state_roots.cell(roots.cell_id(state));
        let block = self.reserve_rc_block();
        self.memory.blocks[block] = Some(ControlBlock {
            payload: Some(payload),
            storage: Some(allocation),
            allocator: Some(AllocatorState {
                roots: state_roots,
                cell: state_cell,
                deallocate,
                borrowed,
            }),
            finalizing: false,
            strong: 1,
            weak: 0,
        });
        let owner = self.new_rc_handle(block, false);
        Ok(if fallible {
            Value::Result(Ok(Box::new(owner)))
        } else {
            owner
        })
    }
    fn retire_rc_block(&mut self, block: usize) -> RunResult<()> {
        if self.memory.blocks[block]
            .as_ref()
            .is_some_and(|control| control.strong == 0 && control.weak == 0 && !control.finalizing)
        {
            let control = self.memory.blocks[block]
                .take()
                .expect("live control block");
            self.memory.free_blocks.push(block);
            if let Some(state) = control.allocator {
                self.drop_cell(state.roots.cell_id(state.cell))?;
            }
        }
        Ok(())
    }
    pub(super) fn start_memory_finalizer(&mut self) -> RunResult<()> {
        let Some(finalizer) = self.memory.finalizers.pop_front() else {
            return Ok(());
        };
        let state = self.memory.blocks[finalizer.block]
            .as_ref()
            .and_then(|control| control.allocator.as_ref())
            .ok_or(AllocatorError::MissingState)?;
        let function = state.deallocate;
        let cell = state.roots.cell_id(state.cell);
        let receiver = if state.borrowed {
            self.read(cell)?
        } else {
            Value::Address(cell)
        };
        let depth = self.frames.len();
        self.call(
            function,
            vec![receiver, Value::Allocation(finalizer.allocation)],
            None,
        )?;
        if self.frames.len() > depth {
            self.frames.last_mut().unwrap().memory_finalizer = Some(finalizer.block);
            if self.status == Status::Finished {
                self.status = Status::Paused;
            }
        }
        Ok(())
    }
    pub(super) fn finish_memory_finalizer(&mut self, block: usize) -> RunResult<()> {
        self.memory.blocks[block]
            .as_mut()
            .ok_or(AllocatorError::MissingState)?
            .finalizing = false;
        self.retire_rc_block(block)
    }

    pub(super) fn clone_value(&mut self, value: Value) -> RunResult<Value> {
        Ok(match value {
            Value::Allocation(_) => return Err(OwnershipError::MoveOnlyAllocation.into()),
            Value::Generator(_) => return Err(OwnershipError::MoveOnlyGenerator.into()),
            Value::GeneratorStep(step) => Value::GeneratorStep(match step {
                cyallo_values::GeneratorStep::Yielded(value) => {
                    cyallo_values::GeneratorStep::Yielded(Box::new(self.clone_value(*value)?))
                }
                cyallo_values::GeneratorStep::Complete(value) => {
                    cyallo_values::GeneratorStep::Complete(Box::new(self.clone_value(*value)?))
                }
            }),
            value @ (Value::Rc(_) | Value::Weak(_)) => self.rc_clone(&value, false, false)?,
            Value::Cell(value) => Value::Cell(Box::new(self.clone_value(*value)?)),
            Value::ArraySnapshot(values) => Value::ArraySnapshot(
                values
                    .into_iter()
                    .map(|value| self.clone_value(value))
                    .collect::<RunResult<Vec<_>>>()?,
            ),
            Value::SparseSnapshot(values, length) => Value::SparseSnapshot(
                values
                    .into_iter()
                    .map(|(index, value)| Ok((index, self.clone_value(value)?)))
                    .collect::<RunResult<_>>()?,
                length,
            ),
            Value::Optional(value) => Value::Optional(
                value
                    .map(|value| self.clone_value(*value).map(Box::new))
                    .transpose()?,
            ),
            Value::Result(value) => Value::Result(match value {
                Ok(value) => Ok(Box::new(self.clone_value(*value)?)),
                Err(value) => Err(Box::new(self.clone_value(*value)?)),
            }),
            Value::Function(function, captures) => Value::Function(
                function,
                captures
                    .into_iter()
                    .map(|value| self.clone_value(value))
                    .collect::<RunResult<Vec<_>>>()?,
            ),
            Value::Erased(ty, value) => Value::Erased(ty, Box::new(self.clone_value(*value)?)),
            value => value,
        })
    }
    pub(super) fn managed_new(
        &mut self,
        kind: cyallo_ast::MemoryKind,
        ty: &Type,
        value: Value,
    ) -> RunResult<Value> {
        let roots = self.root_scope();
        let source = roots.value(value);
        let previous = self.allocation_owner;
        self.allocation_owner = None;
        let allocated = self.alloc(ty);
        self.allocation_owner = previous;
        let cell = roots.cell(allocated?);
        self.store(roots.cell_id(cell), roots.value_copy(source))?;
        if kind == cyallo_ast::MemoryKind::Gc {
            return Ok(Value::Managed(roots.cell_id(cell)));
        }
        let allocation =
            self.allocate_backing(std::mem::size_of::<usize>(), std::mem::align_of::<usize>())?;
        let payload = self.rc_payload(roots.cell_id(cell), allocation)?;
        let block = self.reserve_rc_block();
        self.memory.blocks[block] = Some(ControlBlock {
            payload: Some(payload),
            storage: Some(allocation),
            allocator: None,
            finalizing: false,
            strong: 1,
            weak: 0,
        });
        Ok(self.new_rc_handle(block, false))
    }
    fn reserve_rc_block(&mut self) -> usize {
        self.memory.free_blocks.pop().unwrap_or_else(|| {
            let block = self.memory.blocks.len();
            self.memory.blocks.push(None);
            block
        })
    }
    fn new_rc_handle(&mut self, block: usize, weak: bool) -> Value {
        let token = self.memory.handles.insert(Handle { block, weak });
        if weak {
            Value::Weak(token)
        } else {
            Value::Rc(token)
        }
    }
    fn owner(&self, value: &Value) -> RunResult<&Handle> {
        let index = match value {
            Value::Rc(index) | Value::Weak(index) => *index,
            _ => return Err(expected(ValueKind::ReferenceCounted)),
        };
        self.memory
            .handles
            .get(index)
            .ok_or(OwnershipError::ReleasedOwner.into())
    }
    pub(super) fn owner_value(&self, value: &Value) -> RunResult<Value> {
        if let Value::Address(cell) = value {
            self.read(*cell)
        } else {
            Ok(value.clone())
        }
    }
    pub(super) fn rc_address(&self, value: &Value) -> RunResult<usize> {
        let handle = self.owner(value)?;
        if handle.weak {
            return Err(OwnershipError::WeakNotUpgraded.into());
        }
        let payload = self.memory.blocks[handle.block]
            .as_ref()
            .and_then(|block| block.payload.as_ref())
            .ok_or(OwnershipError::ReleasedAllocation)?;
        Ok(payload.roots.cell_id(payload.cell))
    }
    pub(super) fn rc_clone(
        &mut self,
        value: &Value,
        downgrade: bool,
        upgrade: bool,
    ) -> RunResult<Value> {
        let value = self.owner_value(value)?;
        let handle = self.owner(&value)?;
        let block = handle.block;
        let weak = if upgrade {
            false
        } else {
            downgrade || handle.weak
        };
        let control = self.memory.blocks[block]
            .as_mut()
            .ok_or(OwnershipError::ReleasedAllocation)?;
        if upgrade && control.strong == 0 {
            return Ok(Value::Optional(None));
        }
        if weak {
            control.weak = control
                .weak
                .checked_add(1)
                .ok_or(OwnershipError::WeakCountOverflow)?;
        } else {
            control.strong = control
                .strong
                .checked_add(1)
                .ok_or(OwnershipError::StrongCountOverflow)?;
        }
        let result = self.new_rc_handle(block, weak);
        Ok(if upgrade {
            Value::Optional(Some(Box::new(result)))
        } else {
            result
        })
    }
    pub(super) fn rc_count(&self, value: &Value, weak: bool) -> RunResult<usize> {
        let value = self.owner_value(value)?;
        let handle = self.owner(&value)?;
        let block = self.memory.blocks[handle.block]
            .as_ref()
            .ok_or(OwnershipError::ReleasedAllocation)?;
        Ok(if weak { block.weak } else { block.strong })
    }
    fn release_rc(&mut self, index: usize) -> RunResult<()> {
        let Some(handle) = self.memory.handles.remove(index) else {
            return Ok(());
        };
        let control = self.memory.blocks[handle.block]
            .as_mut()
            .ok_or(OwnershipError::ReleasedAllocation)?;
        if handle.weak {
            control.weak -= 1;
        } else {
            control.strong -= 1;
        }
        let payload = if control.strong == 0 {
            control.payload.take()
        } else {
            None
        };
        if let Some(payload) = payload {
            self.drop_cell(payload.roots.cell_id(payload.cell))?;
        }
        let control = self.memory.blocks[handle.block]
            .as_mut()
            .ok_or(OwnershipError::ReleasedAllocation)?;
        if control.strong == 0
            && let Some(allocation) = control.storage.take()
        {
            if control.allocator.is_some() {
                control.finalizing = true;
                self.memory.finalizers.push_back(Finalizer {
                    block: handle.block,
                    allocation,
                });
            } else {
                self.deallocate_backing(allocation);
            }
        }
        self.retire_rc_block(handle.block)?;
        Ok(())
    }
    pub(super) fn drop_value(&mut self, value: Value) -> RunResult<()> {
        match value {
            Value::Concurrent(handle) => handle.release(),
            Value::TaskOutcome(cyallo_concurrent_values::TaskOutcome::Completed(value)) => {
                self.drop_value(*value)?
            }
            Value::SendError(error) => self.drop_value(*error.value)?,
            Value::Generator(id) => self.close_generator(id)?,
            Value::Rc(index) | Value::Weak(index) => self.release_rc(index)?,
            Value::Allocation(index) => self.deallocate_backing(index),
            Value::Array(cells) => {
                for cell in cells {
                    self.drop_cell(cell)?;
                }
            }
            Value::Deque(cells) => {
                for cell in cells {
                    self.drop_cell(cell)?;
                }
            }
            Value::SparseArray(cells, _) => {
                for cell in cells.into_values() {
                    self.drop_cell(cell)?;
                }
            }
            Value::ArraySnapshot(values) | Value::Function(_, values) => {
                for value in values {
                    self.drop_value(value)?;
                }
            }
            Value::SparseSnapshot(values, _) => {
                for value in values.into_values() {
                    self.drop_value(value)?;
                }
            }
            Value::GeneratorStep(
                cyallo_values::GeneratorStep::Yielded(value)
                | cyallo_values::GeneratorStep::Complete(value),
            )
            | Value::Cell(value)
            | Value::Optional(Some(value))
            | Value::Result(Ok(value) | Err(value))
            | Value::Erased(_, value)
            | Value::StoragePath(value, _) => self.drop_value(*value)?,
            _ => {}
        }
        Ok(())
    }
    pub(super) fn drop_cell(&mut self, cell: usize) -> RunResult<()> {
        if matches!(
            self.heap[cell].ty,
            Type::Array(..) | Type::Record(..) | Type::Tuple(..)
        ) && let Value::Array(children) = self.heap[cell].value.clone()
        {
            for child in children {
                self.drop_cell(child)?;
            }
            return Ok(());
        }
        let value = std::mem::replace(&mut self.heap[cell].value, Value::Uninitialized);
        self.drop_value(value)?;
        if self.native_enabled {
            self.native_cells.set(cell, NativeSlot::default());
        }
        Ok(())
    }
    pub(super) fn take_cell(&mut self, cell: usize) -> RunResult<Value> {
        let value = self.read(cell)?;
        self.clear_moved_cell(cell);
        Ok(value)
    }
    fn clear_moved_cell(&mut self, cell: usize) {
        match self.heap[cell].value.clone() {
            Value::Array(cells)
                if matches!(
                    self.heap[cell].ty,
                    Type::Array(..) | Type::Record(..) | Type::Tuple(..)
                ) =>
            {
                for child in cells {
                    self.clear_moved_cell(child);
                }
            }
            _ => self.heap[cell].value = Value::Uninitialized,
        }
        if self.native_enabled {
            self.native_cells
                .set(cell, NativeSlot::from_value(&self.heap[cell].value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_ownership_cycles_reuse_metadata_without_reusing_identity() {
        let program = Arc::new(crate::compile("for i = 0 to 999 { let owner = @rc i; let weak = owner.downgrade(); std::mem::drop(owner); let missing = weak.upgrade().is_none() }\n").unwrap());
        let mut session = Session::new(program).unwrap();
        session.run(100_000);
        assert_eq!(session.status(), &Status::Finished);
        assert!(session.memory.handles.slots.len() < 16);
        assert!(session.memory.blocks.len() < 16);
        assert!(session.memory.allocations.slots.len() < 16);
        assert_eq!(session.memory.allocated_bytes, 0);
        let mut pool = TokenPool::default();
        let expired = pool.insert(1);
        assert_eq!(pool.remove(expired), Some(1));
        let current = pool.insert(2);
        assert_ne!(expired, current);
        assert_eq!(pool.remove(expired), None);
        assert_eq!(pool.get(current), Some(&2));
    }
}
