//! Resolve scalar places and loan provenance from checked IR.

use super::{
    LoanAccess, LoanOrigin, Register, RegisterType, ScalarType, ValueType,
    cfg::CheckedBlock,
    lower::{LowerError, lower_index},
    shapes::Shapes,
};
use crate::{
    Type,
    ir::{Function, LocalStorage, Op},
};
use cyallo_ir::enums::Projection;

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
    ) -> Result<Self, LowerError> {
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
        let storage_error = || LowerError::UnsupportedStorage { routine };
        if function.local_storage.len() != function.locals.len() {
            return Err(storage_error());
        }
        let mut locals = Vec::with_capacity(function.locals.len());
        for (local, storage) in function.locals.iter().zip(function.local_storage.iter()) {
            let register = match storage {
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
                        return Err(storage_error());
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
        for op in ops() {
            if let Op::Address(dst, slot) = *op
                && !slot.global
            {
                let place = *locals.get(slot.index).ok_or_else(storage_error)?;
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
                Op::Store(addr, _) => written[*addr] = true,
                Op::Move(_, addr) if projects[*addr] => written[*addr] = true,
                _ => {}
            }
        }
        for op in all_ops.iter().rev() {
            match op {
                Op::Field(dst, base, _)
                | Op::Index(dst, base, ..)
                | Op::EnumPayload(dst, base, _, Projection::Address, _)
                    if written[*dst] =>
                {
                    written[*base] = true;
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
                if let Some(array) = array {
                    elements[dst] = Some(ElementPlace {
                        array,
                        indices: indices.as_slice().into(),
                        writable: !payload_copies[base],
                    });
                    registers[dst] = RegisterType::Unused;
                    loan_types[dst] = None;
                }
            } else if let Some(owner) = base_local
                && written[dst]
            {
                // A write through a local's field or payload borrows the local.
                projected[dst] = true;
                loan_types[dst] = Some((LoanAccess::Exclusive, value));
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

    /// Whether the address is a loan on a projection chain.
    pub fn is_projected(&self, index: usize) -> bool {
        self.projected.get(index).copied().unwrap_or(false)
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
