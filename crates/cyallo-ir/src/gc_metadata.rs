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
        Type::Enum(..)
            | Type::EnumRef(..)
            | Type::String
            | Type::Reference { .. }
            | Type::Managed { .. }
            | Type::Slice(..)
            | Type::Array(..)
            | Type::SparseArray(..)
            | Type::Record(..)
            | Type::AnonymousRecord(..)
            | Type::RecordRef(_)
            | Type::Tuple(..)
            | Type::Function(..)
            | Type::Borrowed(..)
            | Type::Storage(..)
            | Type::Opaque(..)
            | Type::Dynamic
            | Type::Object(_)
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
            | Op::Field(r, ..)
            | Op::EnumPayload(r, _, _, crate::enums::Projection::Address, _) = inst.op
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
                    // The operation's contract, not the generic builtin rule: a scalar
                    // cast or length cannot collect even though it runs in a helper.
                    Op::Lookup(_, operation, _) => {
                        operation.contract().safepoint != crate::contract::Safepoint::Never
                    }
                    Op::Builtin(_, intrinsic, _) => {
                        intrinsic.contract().safepoint != crate::contract::Safepoint::Never
                    }
                    Op::Dynamic(..)
                    | Op::Call(..)
                    | Op::TraitCall(..)
                    | Op::TraitBox(..)
                    | Op::Service(..)
                    | Op::Invoke(..)
                    | Op::ArrayLiteral(..)
                    | Op::Frozen(..)
                    | Op::EnumPayload(..)
                    | Op::EnumConstruct(..)
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
                    | Op::Binary(value, ..) => {
                        owned(&function.registers[*value])
                            && !matches!(function.registers[*value], Type::Object(_))
                    }
                    _ => false,
                };
                // A lookup whose contract never collects or re-enters is not a
                // re-entry point either.
                let non_reentrant_lookup = matches!(
                    &inst.op,
                    Op::Lookup(_, operation, _)
                        if operation.contract().safepoint == crate::contract::Safepoint::Never
                );
                SafepointEffects {
                    may_collect,
                    may_reenter: !non_reentrant_lookup
                        && (may_collect
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
                            )),
                }
            })
            .collect();
        Self {
            registers: registers.into_boxed_slice(),
            instructions,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{intrinsic::Intrinsic, ir::Instruction};
    use cyallo_ast::{RoutineAttributes, RoutineKind, RoutineOrigin, Span};
    fn function(code: Vec<Op>, registers: Vec<Type>) -> Function {
        Function {
            generator: None,
            symbol: "f".into(),
            origin: RoutineOrigin::default(),
            attributes: RoutineAttributes::default(),
            analysis_error: None,
            resume: Default::default(),
            gc: Default::default(),
            kind: RoutineKind::Function,
            span: Span::default(),
            pure: false,
            effects: Default::default(),
            name: "f".into(),
            locals: vec![],
            local_storage: Box::new([]),
            params: vec![],
            result: Type::Void,
            registers,
            code: code
                .into_iter()
                .map(|op| Instruction {
                    op,
                    span: Span::default(),
                })
                .collect(),
        }
    }
    #[test]
    fn builtin_safepoints_follow_the_intrinsic_contract() {
        let registers = vec![Type::Integer, Type::Integer, Type::String, Type::String];
        let f = function(
            vec![
                Op::Builtin(0, Intrinsic::CastI64, vec![1]),
                Op::Builtin(0, Intrinsic::Length, vec![2]),
                Op::Builtin(3, Intrinsic::Upper, vec![2]),
            ],
            registers,
        );
        let gc = GcMetadata::build(&f);
        // Scalar conversions and lengths cannot collect or re-enter.
        assert_eq!(gc.instructions[0], SafepointEffects::default());
        assert_eq!(gc.instructions[1], SafepointEffects::default());
        // A builtin that allocates a string keeps the conservative safepoint.
        assert!(gc.instructions[2].may_collect && gc.instructions[2].may_reenter);
    }
}
