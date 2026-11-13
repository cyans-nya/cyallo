//! Immutable GC facts shared by the interpreter and every native emitter.
use crate::{
    Type,
    ir::{Function, Op},
};

/// Language types describe pointees too, so addresses need a separate kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceKind {
    Scalar,
    OwnedValue,
    Location,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SafepointEffects {
    pub may_collect: bool,
    pub may_reenter: bool,
}
#[derive(Debug, Clone, Default)]
pub struct GcMetadata {
    pub registers: Box<[ReferenceKind]>,
    pub instructions: Box<[SafepointEffects]>,
}
fn owned(ty: &Type) -> bool {
    matches!(
        ty,
        Type::String
            | Type::Reference { .. }
            | Type::Managed { .. }
            | Type::Slice(..)
            | Type::Array(..)
            | Type::SparseArray(..)
            | Type::Record(..)
            | Type::RecordRef(_)
            | Type::Tuple(..)
            | Type::Function(..)
            | Type::Borrowed(..)
            | Type::Storage(..)
            | Type::Any
            | Type::TraitObject(_)
    )
}
impl GcMetadata {
    pub fn build(function: &Function) -> Self {
        let mut registers: Vec<_> = function
            .registers
            .iter()
            .map(|ty| {
                if owned(ty) {
                    ReferenceKind::OwnedValue
                } else {
                    ReferenceKind::Scalar
                }
            })
            .collect();
        for inst in &function.code {
            if let Op::Address(r, _)
            | Op::ManagedAddress(r, _)
            | Op::Index(r, ..)
            | Op::Field(r, ..) = inst.op
            {
                registers[r] = ReferenceKind::Location;
            }
        }
        // Lowering can move a location independently of its pointee type.
        loop {
            let mut changed = false;
            for inst in &function.code {
                if let Op::Move(dst, src) = inst.op
                    && registers[src] == ReferenceKind::Location
                    && registers[dst] != ReferenceKind::Location
                {
                    registers[dst] = ReferenceKind::Location;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let instructions = function
            .code
            .iter()
            .map(|inst| {
                let may_collect = match &inst.op {
                    Op::Service(_, crate::services::Operation::Reference, _) => false,
                    Op::Lookup(..)
                    | Op::Call(..)
                    | Op::TraitCall(..)
                    | Op::TraitBox(..)
                    | Op::Service(..)
                    | Op::Invoke(..)
                    | Op::Builtin(..)
                    | Op::ArrayLiteral(..)
                    | Op::Frozen(..)
                    | Op::Aggregate(..)
                    | Op::FunctionValue(..)
                    | Op::Input(..) => true,
                    Op::Yield(..) => true,
                    Op::Index(_, base, _, write) => {
                        *write && matches!(function.registers[*base], Type::SparseArray(..))
                    }
                    Op::Store(_, value)
                    | Op::Load(value, _)
                    | Op::Unary(value, ..)
                    | Op::Binary(value, ..) => owned(&function.registers[*value]),
                    _ => false,
                };
                SafepointEffects {
                    may_collect,
                    may_reenter: may_collect
                        || matches!(inst.op, Op::Drop(_) | Op::Return(_))
                        || !matches!(
                            inst.op,
                            Op::Service(_, crate::services::Operation::Reference, _)
                        ) && matches!(
                            inst.op,
                            Op::Lookup(..)
                                | Op::Call(..)
                                | Op::TraitCall(..)
                                | Op::Service(..)
                                | Op::Invoke(..)
                                | Op::Input(..)
                                | Op::FrameInput(..)
                        ),
                }
            })
            .collect();
        Self {
            registers: registers.into_boxed_slice(),
            instructions,
        }
    }
}
