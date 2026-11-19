//! Resolve scalar places and loan provenance from checked IR.

use super::{
    GlobalId, LoanAccess, LoanOrigin, Register, RegisterType, ScalarType, ValueType,
    cfg::CheckedBlock,
    lower::{Globals, LowerError, describe, lower_index, storage as storage_error},
    shapes::Shapes,
};
use crate::{
    Type,
    ir::{Function, LocalStorage, Op},
};
use cyallo_ir::{enums::Projection, lookup::Operation};

pub(super) struct Places {
    routine: usize,
    pub registers: Vec<RegisterType>,
    pub locals: Vec<Register>,
    addresses: Vec<Option<Register>>,
    /// Registers that hold a copy of an enum payload read by address.
    payload_copies: Vec<bool>,
    /// Registers addressing one element of a local array.
    elements: Vec<Option<ElementPlace>>,
    /// Addresses that are loans on a projection chain.
    projected: Vec<bool>,
    /// Addresses of module-level variables.
    global_addresses: Vec<Option<GlobalId>>,
    /// Addresses of locals that a call receives as a borrow of the local.
    borrowed_addresses: Vec<Option<Register>>,
}

/// An element of a local array reached through checked indices.
#[derive(Clone)]
struct ElementPlace {
    array: Register,
    indices: Box<[usize]>,
    /// False when the array register holds a copy, so a write would be lost.
    writable: bool,
}

impl Places {
    pub fn collect(
        function: &Function,
        routine: usize,
        blocks: &[CheckedBlock<'_>],
        shapes: &mut Shapes,
        globals: &Globals,
        param_access: &dyn Fn(usize, usize) -> Option<LoanAccess>,
    ) -> Result<Self, LowerError> {
        let is_main = &*function.name == "<main>";
        let mut registers: Vec<_> = function
            .registers
            .iter()
            .map(|ty| storage_type(ty, shapes))
            .collect();
        let mut loan_types: Vec<_> = function
            .registers
            .iter()
            .map(|ty| loan_type(ty, shapes).or_else(|| borrowing_closure(ty, shapes)))
            .collect();
        let mut borrowing: Vec<bool> = function
            .registers
            .iter()
            .map(|ty| matches!(ty, Type::Borrowed(_)) && borrowing_closure(ty, shapes).is_some())
            .collect();
        if function.local_storage.len() != function.locals.len() {
            return Err(storage_error(
                routine,
                "locals and their storage differ in number",
            ));
        }
        let mut locals = Vec::with_capacity(function.locals.len());
        for (position, (local, storage)) in function
            .locals
            .iter()
            .zip(function.local_storage.iter())
            .enumerate()
        {
            let register = match storage {
                // A module-level variable lives outside the frame.
                LocalStorage::Cell(_) if is_main && globals.pointee(position).is_some() => {
                    let index = lower_index(routine, registers.len())?;
                    registers.push(RegisterType::Unused);
                    loan_types.push(None);
                    borrowing.push(false);
                    index
                }
                LocalStorage::Register(index) => lower_index(routine, *index)?,
                LocalStorage::Cell(_) => {
                    let ty = storage_type(&local.ty, shapes);
                    let loan = loan_type(&local.ty, shapes)
                        .or_else(|| borrowing_closure(&local.ty, shapes));
                    borrowing.push(
                        matches!(local.ty, Type::Borrowed(_))
                            && borrowing_closure(&local.ty, shapes).is_some(),
                    );
                    if ty == RegisterType::Unused && loan.is_none() {
                        return Err(storage_error(
                            routine,
                            format!("local {position} has type {}", describe(&local.ty)),
                        ));
                    }
                    let index = lower_index(routine, registers.len())?;
                    registers.push(ty);
                    loan_types.push(loan);
                    index
                }
            };
            locals.push(Register(register));
        }
        let ops = || {
            blocks
                .iter()
                .flat_map(|block| block.operations.iter().map(|inst| &inst.op))
        };
        let mut addresses = vec![None; function.registers.len()];
        let mut global_addresses: Vec<Option<GlobalId>> = vec![None; function.registers.len()];
        for op in ops() {
            if let Op::Address(dst, slot) = *op
                && (slot.global || (is_main && globals.pointee(slot.index).is_some()))
            {
                global_addresses[dst] = Some(GlobalId(lower_index(routine, slot.index)?));
                registers[dst] = RegisterType::Unused;
            } else if let Op::Address(dst, slot) = *op
                && !slot.global
            {
                let place = *locals.get(slot.index).ok_or_else(|| {
                    storage_error(routine, format!("address of unknown local {}", slot.index))
                })?;
                addresses[dst] = Some(place);
                registers[dst] = RegisterType::Unused;
                loan_types[dst] = None;
            }
        }
        let count = function.registers.len();
        let all_ops: Vec<&Op> = ops().collect();
        // Field, element and payload addresses form chains rooted in a local or
        // a loan. An address that is written through, or reborrowed, needs a
        // real loan, and so does every address it is projected from.
        let mut written = vec![false; count];
        let mut lent = vec![false; count];
        let mut projects = vec![false; count];
        for op in &all_ops {
            match op {
                Op::Field(dst, ..)
                | Op::Index(dst, ..)
                | Op::EnumPayload(dst, _, _, Projection::Address, _) => projects[*dst] = true,
                _ => {}
            }
        }
        for op in &all_ops {
            match op {
                Op::Store(addr, _) | Op::Input(addr, _) => written[*addr] = true,
                // Taking an owner out of a projected place empties that place.
                Op::Take(_, addr)
                    if projects[*addr]
                        && shapes
                            .value(&function.registers[*addr])
                            .is_some_and(|ty| shapes.needs_drop(ty)) =>
                {
                    written[*addr] = true;
                }
                Op::Move(dst, addr) if projects[*addr] && loan_types[*dst].is_some() => {
                    // A reference to a projected place needs a real loan.
                    lent[*addr] = true;
                    if matches!(
                        function.registers[*dst],
                        Type::Reference { mutable: true, .. }
                    ) {
                        written[*addr] = true;
                    }
                }
                _ => {}
            }
        }
        for op in all_ops.iter().rev() {
            match op {
                Op::Field(dst, base, _)
                | Op::Index(dst, base, ..)
                | Op::EnumPayload(dst, base, _, Projection::Address, _)
                    if written[*dst] || lent[*dst] =>
                {
                    written[*base] |= written[*dst];
                    lent[*base] |= lent[*dst];
                }
                _ => {}
            }
        }
        // Reads from a local's field or payload observe a copy of the value.
        // Everything else that projects is a loan on the chain's root.
        let mut payload_copies = vec![false; count];
        let mut projected = vec![false; count];
        let mut downstream: Vec<Vec<usize>> = vec![Vec::new(); loan_types.len()];
        let mut origins: Vec<(usize, LoanOrigin)> = Vec::new();
        // A local's address handed to a routine that takes a reference is an
        // implicit borrow of the local.
        let mut borrowed_addresses: Vec<Option<Register>> = vec![None; count];
        for op in &all_ops {
            if let Op::Call(_, callee, args) = op {
                for (position, &arg) in args.iter().enumerate() {
                    let (Some(owner), Some(access)) =
                        (addresses[arg], param_access(*callee, position))
                    else {
                        continue;
                    };
                    let Some(pointee) = places_value(&registers, owner) else {
                        continue;
                    };
                    loan_types[arg] = Some((access, pointee));
                    registers[arg] = RegisterType::Unused;
                    origins.push((arg, LoanOrigin::Local(owner)));
                    borrowed_addresses[arg] = Some(owner);
                }
            }
        }
        // A borrow of a module-level variable is exclusive when it is written
        // through or lent out mutably.
        let mut moved_mut = vec![false; count];
        for op in &all_ops {
            if let Op::Move(dst, src) = op
                && matches!(loan_types[*dst], Some((LoanAccess::Exclusive, _)))
            {
                moved_mut[*src] = true;
            }
        }
        for (dst, global) in global_addresses.iter().enumerate() {
            let Some(global) = *global else { continue };
            let pointee = globals.pointee(global.index()).ok_or_else(|| {
                storage_error(
                    routine,
                    format!("module variable {} has no verified type", global.index()),
                )
            })?;
            let access = if written[dst] || moved_mut[dst] {
                LoanAccess::Exclusive
            } else {
                LoanAccess::Shared
            };
            loan_types[dst] = Some((access, pointee));
            origins.push((dst, LoanOrigin::Global(global)));
        }
        // A slice views the array its source loan borrows, with its root.
        for op in &all_ops {
            if let Op::Lookup(dst, Operation::Slice(_), args) = op
                && let [source] = args.as_slice()
                && loan_types[*source].is_some()
                && loan_types[*dst].is_some()
            {
                downstream[*source].push(*dst);
            }
        }
        // The address of a heap object is a place rooted at the object its
        // handle names, as exclusive as its uses require.
        for op in &all_ops {
            if let Op::ManagedAddress(dst, handle) = op
                && let RegisterType::Gc(managed) | RegisterType::Rc(managed) = registers[*handle]
                && let Some(pointee) = shapes.managed_pointee(managed)
            {
                let access = if written[*dst] || moved_mut[*dst] {
                    LoanAccess::Exclusive
                } else {
                    LoanAccess::Shared
                };
                loan_types[*dst] = Some((access, pointee));
                registers[*dst] = RegisterType::Unused;
                origins.push((*dst, LoanOrigin::Heap(managed)));
            }
        }
        let mut elements: Vec<Option<ElementPlace>> = vec![None; count];
        for op in &all_ops {
            let (dst, base, value) = match op {
                Op::Field(dst, base, _) | Op::Index(dst, base, ..) => {
                    (*dst, *base, shapes.value(&function.registers[*dst]))
                }
                Op::EnumPayload(dst, base, _, Projection::Address, ty) => {
                    (*dst, *base, shapes.value(ty))
                }
                _ => continue,
            };
            let Some(value) = value else { continue };
            let base_local = addresses[base];
            let base_loan = loan_types[base].is_some() || projected[base];
            if base_loan {
                projected[dst] = true;
                let access = if written[dst] {
                    LoanAccess::Exclusive
                } else {
                    LoanAccess::Shared
                };
                loan_types[dst] = Some((access, value));
                registers[dst] = RegisterType::Unused;
                downstream[base].push(dst);
            } else if let Op::Index(_, _, indices, _) = op {
                let array = match base_local {
                    Some(place) => Some(place),
                    None if matches!(registers.get(base), Some(RegisterType::Composite(_))) => {
                        Some(Register(lower_index(routine, base)?))
                    }
                    None => None,
                };
                if let Some(array) = array
                    && lent[dst]
                {
                    // A reference to an element borrows the array.
                    projected[dst] = true;
                    let access = if written[dst] {
                        LoanAccess::Exclusive
                    } else {
                        LoanAccess::Shared
                    };
                    loan_types[dst] = Some((access, value));
                    registers[dst] = RegisterType::Unused;
                    origins.push((dst, LoanOrigin::Local(array)));
                } else if let Some(array) = array {
                    elements[dst] = Some(ElementPlace {
                        array,
                        indices: indices.as_slice().into(),
                        writable: !payload_copies[base],
                    });
                    registers[dst] = RegisterType::Unused;
                    loan_types[dst] = None;
                }
            } else if let Some(owner) = base_local
                && (written[dst] || lent[dst])
            {
                // A write through, or a reference to, a local's field or payload
                // borrows the local.
                projected[dst] = true;
                let access = if written[dst] {
                    LoanAccess::Exclusive
                } else {
                    LoanAccess::Shared
                };
                loan_types[dst] = Some((access, value));
                registers[dst] = RegisterType::Unused;
                origins.push((dst, LoanOrigin::Local(owner)));
            } else if base_local.is_some() || payload_copies[base] {
                payload_copies[dst] = true;
                registers[dst] = value.into();
                loan_types[dst] = None;
            }
        }
        // A closure that captured an exclusive loan borrows exclusively.
        let borrow_access = if all_ops.iter().any(|op| {
            matches!(op, Op::FunctionValue(dst, _, captures)
            if borrowing[*dst]
                && captures.iter().any(|&capture| {
                    matches!(loan_types[capture], Some((LoanAccess::Exclusive, _)))
                }))
        }) {
            LoanAccess::Exclusive
        } else {
            LoanAccess::Shared
        };
        for (index, flag) in borrowing.iter().enumerate() {
            if *flag && let Some((access, _)) = loan_types[index].as_mut() {
                *access = borrow_access;
            }
        }
        for op in &all_ops {
            if let Op::FunctionValue(dst, _, captures) = op
                && borrowing[*dst]
            {
                for &capture in captures {
                    if loan_types[capture].is_some() {
                        downstream[capture].push(*dst);
                    }
                }
            }
        }
        let mut places = Self {
            routine,
            registers,
            locals,
            addresses,
            payload_copies,
            elements,
            projected,
            global_addresses,
            borrowed_addresses,
        };
        if loan_types.iter().any(Option::is_some) {
            let mut aliases = vec![Vec::new(); loan_types.len()];
            origins.extend(
                places
                    .locals
                    .iter()
                    .take(function.params.len())
                    .filter(|&&register| loan_types[register.index()].is_some())
                    .map(|&register| (register.index(), LoanOrigin::Parameter { register })),
            );
            let mut alias = |a: usize, b: usize| {
                if loan_types[a].is_some() && loan_types[a] == loan_types[b] {
                    aliases[a].push(b);
                    aliases[b].push(a);
                }
            };
            for op in &all_ops {
                match **op {
                    Op::Move(dst, src) if loan_types[dst].is_some() => {
                        if let Some(owner) = places.address(src) {
                            if borrowing[owner.index()]
                                || !matches!(
                                    places.registers[owner.index()],
                                    RegisterType::Unused | RegisterType::Loan { .. }
                                )
                            {
                                origins.push((dst, LoanOrigin::Local(owner)));
                            }
                        } else {
                            alias(dst, src);
                        }
                    }
                    Op::Load(dst, addr) if loan_types[dst].is_some() => {
                        if let Some(local) = places.address(addr) {
                            alias(dst, local.index());
                        }
                    }
                    Op::Store(addr, src) if loan_types[src].is_some() => {
                        if let Some(local) = places.address(addr) {
                            alias(local.index(), src);
                        }
                    }
                    _ => {}
                }
            }
            while let Some((index, origin)) = origins.pop() {
                let (access, pointee) =
                    loan_types[index].ok_or(LowerError::UnsupportedRegister {
                        routine,
                        register: index,
                    })?;
                let kind = match (borrowing[index], pointee) {
                    (true, ValueType::Function(signature)) => RegisterType::BorrowingFunction {
                        signature,
                        access,
                        origin,
                    },
                    _ => RegisterType::Loan {
                        access,
                        origin,
                        pointee,
                    },
                };
                match places.registers[index] {
                    RegisterType::Unused => {
                        places.registers[index] = kind;
                        origins.extend(aliases[index].iter().map(|&other| (other, origin)));
                        origins.extend(downstream[index].iter().map(|&other| (other, origin)));
                    }
                    previous if previous == kind => {}
                    _ => {
                        return Err(LowerError::ConflictingLoanOwners {
                            routine,
                            register: index,
                        });
                    }
                }
            }
        }
        Ok(places)
    }

    /// The array and index registers behind an element address.
    pub fn element(&self, index: usize) -> Option<(Register, &[usize], bool)> {
        self.elements
            .get(index)?
            .as_ref()
            .map(|place| (place.array, &*place.indices, place.writable))
    }

    /// The local an address borrows when a routine receives it as a reference.
    pub fn borrowed_address(&self, index: usize) -> Option<Register> {
        self.borrowed_addresses.get(index).copied().flatten()
    }

    /// The module-level variable an address refers to.
    pub fn global_address(&self, index: usize) -> Option<GlobalId> {
        self.global_addresses.get(index).copied().flatten()
    }

    /// Whether the address is a loan on a projection chain.
    pub fn is_projected(&self, index: usize) -> bool {
        self.projected.get(index).copied().unwrap_or(false)
    }

    pub fn is_location(&self, index: usize) -> bool {
        matches!(self.registers.get(index), Some(RegisterType::Location(_)))
    }

    pub fn is_path(&self, index: usize) -> bool {
        matches!(self.registers.get(index), Some(RegisterType::Path))
    }

    pub fn is_text(&self, index: usize) -> bool {
        matches!(self.registers.get(index), Some(RegisterType::Text))
    }

    /// The scalar type of a register that holds a scalar.
    pub fn scalar(&self, index: usize) -> Option<ScalarType> {
        match self.registers.get(index)? {
            RegisterType::Scalar(ty) => Some(*ty),
            _ => None,
        }
    }

    /// Whether the register holds a copy of an enum payload read by address.
    pub fn is_payload_copy(&self, index: usize) -> bool {
        self.payload_copies.get(index).copied().unwrap_or(false)
    }

    pub fn address(&self, index: usize) -> Option<Register> {
        self.addresses.get(index).copied().flatten()
    }

    pub fn register(&self, index: usize) -> Result<Register, LowerError> {
        match self.registers.get(index) {
            None | Some(RegisterType::Unused) => Err(LowerError::UnsupportedRegister {
                routine: self.routine,
                register: index,
            }),
            Some(_) => lower_index(self.routine, index).map(Register),
        }
    }
}

/// The value type held by a register of the function, if it holds a value.
fn places_value(registers: &[RegisterType], register: Register) -> Option<ValueType> {
    registers.get(register.index())?.value()
}

fn storage_type(ty: &Type, shapes: &mut Shapes) -> RegisterType {
    shapes
        .value(ty)
        .map_or(RegisterType::Unused, RegisterType::from)
}

fn loan_type(ty: &Type, shapes: &mut Shapes) -> Option<(LoanAccess, ValueType)> {
    let Type::Reference {
        mutable, pointee, ..
    } = ty
    else {
        return None;
    };
    let access = if *mutable {
        LoanAccess::Exclusive
    } else {
        LoanAccess::Shared
    };
    Some((access, shapes.pointee(pointee)?))
}

/// A closure value that holds loans. Its access is settled from its captures.
fn borrowing_closure(ty: &Type, shapes: &mut Shapes) -> Option<(LoanAccess, ValueType)> {
    let Type::Borrowed(inner) = ty else {
        return None;
    };
    Some((LoanAccess::Exclusive, shapes.pointee(inner)?))
}
