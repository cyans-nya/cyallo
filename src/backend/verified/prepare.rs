//! Legalize verified scalar operations and classify their ABI homes.

use super::{AbiLayout, ByteOffset, Failure, NativeTarget, Relocation, RelocationKind};
use crate::{
    Span,
    bytecode::verified::{
        CallSiteId, CallTarget, FunctionId, Instruction, InstructionId, IntegerOp, Register,
        RegisterType, Scalar, ScalarType, StorageState, VerifiedFunction, VerifiedModule,
    },
};
use std::{collections::BTreeMap, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeHome {
    pub register: Register,
    pub offset: ByteOffset,
    pub ty: ScalarType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeVersion(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FunctionVersion {
    pub function: FunctionId,
    pub version: CodeVersion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Origin {
    pub function: FunctionId,
    pub version: CodeVersion,
    pub instruction: InstructionId,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    StorageLive(NativeHome),
    StorageDead(NativeHome),
    Constant {
        destination: NativeHome,
        value: Scalar,
    },
    Copy {
        destination: NativeHome,
        source: NativeHome,
    },
    Integer {
        operation: IntegerOp,
        destination: NativeHome,
        left: NativeHome,
        right: NativeHome,
    },
    Jump(InstructionId),
    Branch {
        condition: NativeHome,
        yes: InstructionId,
        no: InstructionId,
    },
    Return(Option<NativeHome>),
    Call(InstructionId),
    Trap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    pub origin: Origin,
    pub operation: Operation,
    pub successor: Option<InstructionId>,
    pub fuel: u64,
    pub recover: Box<[RecoverySlot]>,
    pub failures: &'static [Failure],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoverySlot {
    Uninitialized(NativeHome),
    Initialized(NativeHome),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallArgument {
    pub caller: NativeHome,
    pub callee: NativeHome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub target: FunctionVersion,
    pub arguments: Box<[CallArgument]>,
    pub destination: Option<NativeHome>,
    pub result: Option<ScalarType>,
    pub successor: InstructionId,
    pub link: Relocation,
}

const INSTRUCTION_FUEL: u64 = 1;

#[derive(Debug)]
pub struct PreparedFunction {
    target: NativeTarget,
    function: FunctionId,
    version: CodeVersion,
    arguments: Box<[NativeHome]>,
    result: Option<ScalarType>,
    frame_bytes: u32,
    sites: Box<[Site]>,
    calls: BTreeMap<InstructionId, Call>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrepareError {
    UnknownFunction(FunctionId),
    MissingHome(Register),
    NonScalar(Register),
    FrameTooLarge,
    InstructionOverflow,
    ConditionalInitialization {
        origin: Origin,
        register: Register,
    },
    UnsupportedOperation {
        origin: Origin,
        instruction: Instruction,
    },
    NonScalarResult(FunctionId),
}

impl fmt::Display for PrepareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownFunction(function) => {
                write!(f, "unknown verified function {}", function.0)
            }
            Self::MissingHome(register) => {
                write!(f, "register {} has no scalar frame home", register.0)
            }
            Self::NonScalar(register) => write!(
                f,
                "register {} requires a non-scalar native layout",
                register.0
            ),
            Self::NonScalarResult(function) => {
                write!(f, "function {} returns a composite value", function.0)
            }
            Self::FrameTooLarge => f.write_str("scalar frame exceeds the native ABI offset range"),
            Self::InstructionOverflow => {
                f.write_str("instruction index exceeds the native ABI range")
            }
            Self::ConditionalInitialization { origin, register } => write!(
                f,
                "register {} has path-dependent initialization at instruction {}",
                register.0, origin.instruction.0
            ),
            Self::UnsupportedOperation {
                origin,
                instruction,
            } => write!(
                f,
                "unsupported native operation {instruction:?} in function {} at instruction {}",
                origin.function.0, origin.instruction.0
            ),
        }
    }
}

impl std::error::Error for PrepareError {}

impl PreparedFunction {
    pub fn prepare(
        module: &VerifiedModule,
        function: FunctionId,
        version: CodeVersion,
        target: NativeTarget,
    ) -> Result<Self, PrepareError> {
        let body = module
            .function(function)
            .ok_or(PrepareError::UnknownFunction(function))?;
        let frame_bytes = frame_offset(body.frame_layout().slot_count())?.0;
        let arguments = body
            .parameters()
            .iter()
            .map(|&reg| home(body, reg))
            .collect::<Result<_, _>>()?;
        let mut sites = Vec::new();
        let mut calls = BTreeMap::new();
        body.visit_storage(|pc, state| -> Result<(), PrepareError> {
            let index = usize::try_from(pc.0).map_err(|_| PrepareError::InstructionOverflow)?;
            let instruction = body.instructions()[index];
            let origin = Origin {
                function,
                version,
                instruction: pc,
                span: body.spans()[index],
            };
            let successor = match instruction {
                Instruction::Branch(..)
                | Instruction::Jump(..)
                | Instruction::Return(..)
                | Instruction::Trap => None,
                _ => Some(InstructionId(
                    pc.0.checked_add(1)
                        .ok_or(PrepareError::InstructionOverflow)?,
                )),
            };
            if let (Instruction::Call(id), Some(next)) = (instruction, successor) {
                let link_offset = frame_offset(calls.len())?;
                calls.insert(
                    pc,
                    prepare_call(module, body, id, origin, next, link_offset)?,
                );
            }
            let operation = legalize(body, instruction, origin)?;
            let recover = recovery_map(body, state, origin)?;
            sites.push(Site {
                origin,
                operation,
                successor,
                fuel: INSTRUCTION_FUEL,
                recover,
                failures: failures(operation),
            });
            Ok(())
        })?;
        Ok(Self {
            target,
            function,
            version,
            arguments,
            result: scalar_result(function, body)?,
            frame_bytes,
            sites: sites.into_boxed_slice(),
            calls,
        })
    }

    pub fn target(&self) -> NativeTarget {
        self.target
    }

    pub fn function(&self) -> FunctionId {
        self.function
    }

    pub fn version(&self) -> CodeVersion {
        self.version
    }

    pub fn call(&self, pc: InstructionId) -> Option<&Call> {
        self.calls.get(&pc)
    }

    pub fn relocations(&self) -> impl ExactSizeIterator<Item = Relocation> + '_ {
        self.calls.values().map(|call| call.link)
    }

    pub fn abi(&self) -> AbiLayout {
        AbiLayout::SCALARS
    }

    pub fn arguments(&self) -> &[NativeHome] {
        &self.arguments
    }

    pub fn result(&self) -> Option<ScalarType> {
        self.result
    }

    pub fn frame_bytes(&self) -> u32 {
        self.frame_bytes
    }

    pub fn sites(&self) -> &[Site] {
        &self.sites
    }

    pub fn site(&self, instruction: InstructionId) -> Option<&Site> {
        let index = self
            .sites
            .binary_search_by_key(&instruction, |site| site.origin.instruction)
            .ok()?;
        self.sites.get(index)
    }
}

fn scalar_result(
    function: FunctionId,
    body: &VerifiedFunction,
) -> Result<Option<ScalarType>, PrepareError> {
    body.result()
        .map(|ty| ty.scalar().ok_or(PrepareError::NonScalarResult(function)))
        .transpose()
}

fn home(body: &VerifiedFunction, register: Register) -> Result<NativeHome, PrepareError> {
    let index = usize::try_from(register.0).map_err(|_| PrepareError::MissingHome(register))?;
    let Some(RegisterType::Scalar(ty)) = body.registers().get(index).copied() else {
        return Err(PrepareError::NonScalar(register));
    };
    // Native code handles integer and boolean words only.
    if matches!(ty, ScalarType::F64 | ScalarType::U64) {
        return Err(PrepareError::NonScalar(register));
    }
    let slot = body
        .frame_layout()
        .slot(register)
        .ok_or(PrepareError::MissingHome(register))?;
    Ok(NativeHome {
        register,
        offset: frame_offset(slot.index())?,
        ty,
    })
}

fn frame_offset(index: usize) -> Result<ByteOffset, PrepareError> {
    index
        .checked_mul(AbiLayout::SCALARS.slot_bytes)
        .and_then(|bytes| u32::try_from(bytes).ok())
        .map(ByteOffset)
        .ok_or(PrepareError::FrameTooLarge)
}

fn recovery_map(
    body: &VerifiedFunction,
    state: &[StorageState],
    origin: Origin,
) -> Result<Box<[RecoverySlot]>, PrepareError> {
    let mut slots = Vec::new();
    for (index, &value) in state.iter().enumerate() {
        if value == StorageState::Dead {
            continue;
        }
        let register = Register(u32::try_from(index).map_err(|_| PrepareError::FrameTooLarge)?);
        let slot = home(body, register)?;
        slots.push(match value {
            StorageState::Initialized => RecoverySlot::Initialized(slot),
            StorageState::Uninitialized => RecoverySlot::Uninitialized(slot),
            StorageState::MaybeInitialized => {
                return Err(PrepareError::ConditionalInitialization { origin, register });
            }
            StorageState::Dead => continue,
        });
    }
    Ok(slots.into_boxed_slice())
}

fn prepare_call(
    module: &VerifiedModule,
    body: &VerifiedFunction,
    id: CallSiteId,
    origin: Origin,
    successor: InstructionId,
    offset: ByteOffset,
) -> Result<Call, PrepareError> {
    let index = usize::try_from(id.0).map_err(|_| PrepareError::InstructionOverflow)?;
    let unsupported = || PrepareError::UnsupportedOperation {
        origin,
        instruction: Instruction::Call(id),
    };
    let call = body.calls().get(index).ok_or_else(unsupported)?;
    let CallTarget::Direct(function) = call.target else {
        return Err(unsupported());
    };
    let callee = module
        .function(function)
        .ok_or(PrepareError::UnknownFunction(function))?;
    let arguments = call
        .arguments
        .iter()
        .zip(callee.parameters())
        .map(|(&arg, &param)| {
            Ok(CallArgument {
                caller: home(body, arg)?,
                callee: home(callee, param)?,
            })
        })
        .collect::<Result<_, PrepareError>>()?;
    let destination = call
        .destination
        .map(|register| home(body, register))
        .transpose()?;
    let target = FunctionVersion {
        function,
        version: origin.version,
    };
    Ok(Call {
        target,
        arguments,
        destination,
        result: scalar_result(function, callee)?,
        successor,
        link: Relocation {
            instruction: origin.instruction,
            offset,
            kind: RelocationKind::FunctionAddress,
            target,
            addend: 0,
        },
    })
}

fn failures(operation: Operation) -> &'static [Failure] {
    use IntegerOp::*;
    match operation {
        Operation::Integer {
            operation: Add | Subtract | Multiply,
            ..
        } => &[Failure::Overflow],
        Operation::Integer {
            operation: Divide, ..
        } => &[Failure::DivisionByZero, Failure::Overflow],
        Operation::Integer {
            operation: Remainder,
            ..
        } => &[Failure::DivisionByZero],
        Operation::Trap => &[Failure::Trap],
        _ => &[],
    }
}

fn legalize(
    body: &VerifiedFunction,
    instruction: Instruction,
    origin: Origin,
) -> Result<Operation, PrepareError> {
    let home = |register| home(body, register);
    Ok(match instruction {
        Instruction::StorageLive(reg) => Operation::StorageLive(home(reg)?),
        Instruction::StorageDead(reg) => Operation::StorageDead(home(reg)?),
        Instruction::Constant(reg, value) => Operation::Constant {
            destination: home(reg)?,
            value,
        },
        Instruction::Copy(dst, src) => Operation::Copy {
            destination: home(dst)?,
            source: home(src)?,
        },
        Instruction::Integer(operation, dst, lhs, rhs) => Operation::Integer {
            operation,
            destination: home(dst)?,
            left: home(lhs)?,
            right: home(rhs)?,
        },
        Instruction::Jump(to) => Operation::Jump(to),
        Instruction::Branch(condition, yes, no) => Operation::Branch {
            condition: home(condition)?,
            yes,
            no,
        },
        Instruction::Return(value) => Operation::Return(value.map(home).transpose()?),
        Instruction::Trap => Operation::Trap,
        Instruction::Call(_) => Operation::Call(origin.instruction),
        Instruction::Borrow(..)
        | Instruction::ReadLoan(..)
        | Instruction::WriteLoan(..)
        | Instruction::CheckStep(..)
        | Instruction::Tuple(..)
        | Instruction::Extract(..)
        | Instruction::EnumUnit(..)
        | Instruction::EnumWrap(..)
        | Instruction::EnumIs(..)
        | Instruction::EnumPayload(..)
        | Instruction::Float(..)
        | Instruction::TextConstant(..)
        | Instruction::TextConcat(..)
        | Instruction::TextCompare(..)
        | Instruction::TextLength(..)
        | Instruction::TextLeft(..)
        | Instruction::TextRight(..)
        | Instruction::TextMid(..)
        | Instruction::Closure(..)
        | Instruction::BorrowGlobal(..)
        | Instruction::GcNew(..)
        | Instruction::Move(..)
        | Instruction::Drop(..)
        | Instruction::MoveOut(..)
        | Instruction::RangeNew(..)
        | Instruction::RangeInclusive(..)
        | Instruction::VecFrom(..)
        | Instruction::VecNew(..)
        | Instruction::VecPush(..)
        | Instruction::VecLength(..)
        | Instruction::CellNew(..)
        | Instruction::CellRead(..)
        | Instruction::CellWrite(..)
        | Instruction::CellReplace(..)
        | Instruction::Clone(..)
        | Instruction::TextMap(..)
        | Instruction::TextTest(..)
        | Instruction::TextReplace(..)
        | Instruction::ToText(..)
        | Instruction::TraitBox(..)
        | Instruction::Abs(..)
        | Instruction::CountToInteger(..)
        | Instruction::IntegerToCount(..)
        | Instruction::ParseInteger(..)
        | Instruction::PathNew(..)
        | Instruction::PathMissing(..)
        | Instruction::PathTarget(..)
        | Instruction::PathField(..)
        | Instruction::PathElement(..)
        | Instruction::Input(..)
        | Instruction::InputLoan(..)
        | Instruction::Extremum(..)
        | Instruction::SliceOf(..)
        | Instruction::SliceLength(..)
        | Instruction::Sum(..)
        | Instruction::RcNew(..)
        | Instruction::RcClone(..)
        | Instruction::RcDowngrade(..)
        | Instruction::RcUpgrade(..)
        | Instruction::RcStrongCount(..)
        | Instruction::EnumTake(..)
        | Instruction::GcBorrow(..)
        | Instruction::LoanField(..)
        | Instruction::LoanPayload(..)
        | Instruction::LoanElement(..)
        | Instruction::ArrayDeclare(..)
        | Instruction::ArrayNew(..)
        | Instruction::ArrayLoad(..)
        | Instruction::ArrayStore(..)
        | Instruction::ArrayLength(..)
        | Instruction::RealDivide(..)
        | Instruction::Boolean(..)
        | Instruction::NegateInteger(..)
        | Instruction::NegateFloat(..)
        | Instruction::Not(..)
        | Instruction::IntegerToFloat(..)
        | Instruction::FloatToInteger(..) => {
            return Err(PrepareError::UnsupportedOperation {
                origin,
                instruction,
            });
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_offsets_reject_arithmetic_overflow() {
        assert_eq!(frame_offset(0), Ok(ByteOffset(0)));
        assert_eq!(
            frame_offset(1),
            Ok(ByteOffset(
                u32::try_from(AbiLayout::SCALARS.slot_bytes).unwrap()
            ))
        );
        assert_eq!(frame_offset(usize::MAX), Err(PrepareError::FrameTooLarge));
    }
}
