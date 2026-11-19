use super::{
    BooleanOp, CallSite, CallSiteId, CallTarget, FORMAT_VERSION, FloatOp, FunctionId,
    FunctionImage, HostBindingId, Instruction, InstructionId, IntegerOp, ModuleImage,
    OperandListId, OutputId, OutputPiece, OutputTemplate, Register, RegisterType, Scalar,
    ScalarHostBinding, ScalarType, TextId, TextOp, VerifiedModule, VerifyError,
    cfg::{self, Block, Exit, Located, Statement},
    encode, layout, lifetimes,
    loans::Places,
    shapes::Shapes,
};
use crate::{
    Diagnostic, Span, Type,
    ir::{Function, Op, Program, Routine},
};
use cyallo_ast::{Literal, Operator};
use cyallo_ir::{enums::Projection, intrinsic::Intrinsic};
use std::{collections::HashMap, fmt, sync::Arc};

#[derive(Debug)]
pub enum LowerError {
    UnknownRoutine(usize),
    Analysis {
        routine: usize,
        diagnostic: Box<Diagnostic>,
    },
    UnsupportedSignature {
        routine: usize,
    },
    UnsupportedStorage {
        routine: usize,
    },
    UnsupportedRegister {
        routine: usize,
        register: usize,
    },
    ConflictingLoanOwners {
        routine: usize,
        register: usize,
    },
    UnsupportedOperation {
        routine: usize,
        instruction: usize,
        span: Span,
    },
    IndexOverflow {
        routine: usize,
    },
    InvalidTarget {
        routine: usize,
        instruction: usize,
        target: InstructionId,
    },
    InvalidFallthrough {
        routine: usize,
    },
    Verification(VerifyError),
}

impl fmt::Display for LowerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownRoutine(id) => write!(f, "unknown checked routine {id}"),
            Self::Analysis {
                routine,
                diagnostic,
            } => write!(f, "routine {routine}: {diagnostic}"),
            Self::UnsupportedSignature { routine } => {
                write!(f, "unsupported scalar signature for routine {routine}")
            }
            Self::UnsupportedStorage { routine } => {
                write!(f, "unsupported scalar frame storage in routine {routine}")
            }
            Self::UnsupportedRegister { routine, register } => write!(
                f,
                "unsupported scalar register {register} in routine {routine}"
            ),
            Self::ConflictingLoanOwners { routine, register } => write!(
                f,
                "loan register {register} has incompatible owners in routine {routine}"
            ),
            Self::UnsupportedOperation {
                routine,
                instruction,
                ..
            } => write!(
                f,
                "unsupported scalar operation {instruction} in routine {routine}"
            ),
            Self::IndexOverflow { routine } => {
                write!(f, "bytecode index overflow in routine {routine}")
            }
            Self::InvalidTarget {
                routine,
                instruction,
                target,
            } => write!(
                f,
                "invalid target {} at instruction {instruction} in routine {routine}",
                target.0
            ),
            Self::InvalidFallthrough { routine } => {
                write!(
                    f,
                    "routine {routine} falls through past its last instruction"
                )
            }
            Self::Verification(error) => fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for LowerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Analysis { diagnostic, .. } => Some(diagnostic.as_ref()),
            Self::Verification(error) => Some(error),
            _ => None,
        }
    }
}

impl VerifiedModule {
    /// Lower a selected checked routine and its transitive direct calls. Routine
    /// indices belong to the supplied linked program. Unreferenced bodies stay
    /// deferred. Unsupported operations fail before any execution starts.
    pub fn lower(program: &Program, routine: usize) -> Result<Self, LowerError> {
        Self::lower_checked(program.functions(), routine)
    }

    pub(super) fn lower_checked(checked: &[Routine], routine: usize) -> Result<Self, LowerError> {
        Self::lower_with_hosts(checked, routine, &HashMap::new(), Vec::new())
    }

    pub(super) fn lower_with_hosts(
        checked: &[Routine],
        routine: usize,
        host_ids: &HashMap<usize, HostBindingId>,
        host_bindings: Vec<ScalarHostBinding>,
    ) -> Result<Self, LowerError> {
        let entry = FunctionId(0);
        let mut ids = HashMap::from([(routine, entry)]);
        let mut routines = vec![routine];
        let mut functions = Vec::new();
        let mut shapes = Shapes::default();
        while let Some(&id) = routines.get(functions.len()) {
            let function = checked
                .get(id)
                .ok_or(LowerError::UnknownRoutine(id))?
                .checked()
                .map_err(|diagnostic| LowerError::Analysis {
                    routine: id,
                    diagnostic: Box::new(diagnostic.clone()),
                })?;
            functions.push(lower_function(function, id, &mut shapes, |target| {
                if let Some(&binding) = host_ids.get(&target) {
                    return Ok(CallTarget::Bound(binding));
                }
                if let Some(&candidate) = ids.get(&target) {
                    return Ok(CallTarget::Direct(candidate));
                }
                let next = lower_index(id, routines.len()).map(FunctionId)?;
                routines.push(target);
                ids.insert(target, next);
                Ok(CallTarget::Direct(next))
            })?);
        }
        let (shapes, signatures) = shapes.into_parts();
        ModuleImage {
            version: FORMAT_VERSION,
            entry,
            shapes,
            signatures,
            functions: functions.into_boxed_slice(),
        }
        .verify_with_host_bindings(host_bindings)
        .map_err(LowerError::Verification)
    }
}

pub(super) fn lower_index(routine: usize, index: usize) -> Result<u32, LowerError> {
    u32::try_from(index).map_err(|_| LowerError::IndexOverflow { routine })
}

pub(super) fn lower_type(ty: &Type) -> Option<ScalarType> {
    match ty {
        Type::Integer => Some(ScalarType::I64),
        Type::Real => Some(ScalarType::F64),
        Type::Boolean => Some(ScalarType::Bool),
        _ => None,
    }
}

fn lower_integer(op: Operator) -> Option<IntegerOp> {
    Some(match op {
        Operator::Add => IntegerOp::Add,
        Operator::Subtract => IntegerOp::Subtract,
        Operator::Multiply => IntegerOp::Multiply,
        Operator::Divide | Operator::IntegerDivide => IntegerOp::Divide,
        Operator::Remainder => IntegerOp::Remainder,
        Operator::Less => IntegerOp::Less,
        Operator::LessEqual => IntegerOp::LessEqual,
        Operator::Greater => IntegerOp::Greater,
        Operator::GreaterEqual => IntegerOp::GreaterEqual,
        Operator::Equal => IntegerOp::Equal,
        Operator::NotEqual => IntegerOp::NotEqual,
        _ => return None,
    })
}

fn lower_float(op: Operator) -> Option<FloatOp> {
    Some(match op {
        Operator::Add => FloatOp::Add,
        Operator::Subtract => FloatOp::Subtract,
        Operator::Multiply => FloatOp::Multiply,
        Operator::Divide => FloatOp::Divide,
        Operator::RealDivide => FloatOp::CheckedDivide,
        Operator::Remainder => FloatOp::Remainder,
        Operator::Less => FloatOp::Less,
        Operator::LessEqual => FloatOp::LessEqual,
        Operator::Greater => FloatOp::Greater,
        Operator::GreaterEqual => FloatOp::GreaterEqual,
        Operator::Equal => FloatOp::Equal,
        Operator::NotEqual => FloatOp::NotEqual,
        _ => return None,
    })
}

fn lower_text(op: Operator) -> Option<TextOp> {
    Some(match op {
        Operator::Equal => TextOp::Equal,
        Operator::NotEqual => TextOp::NotEqual,
        Operator::Less => TextOp::Less,
        Operator::LessEqual => TextOp::LessEqual,
        Operator::Greater => TextOp::Greater,
        Operator::GreaterEqual => TextOp::GreaterEqual,
        _ => return None,
    })
}

fn is_text_builtin(intrinsic: Intrinsic) -> bool {
    matches!(
        intrinsic,
        Intrinsic::Length
            | Intrinsic::StringLength
            | Intrinsic::ArrayLength
            | Intrinsic::Left
            | Intrinsic::Right
            | Intrinsic::Mid
    )
}

/// Append an operand list and name it.
fn new_list(
    routine: usize,
    sites: &mut Sites,
    registers: impl Iterator<Item = Result<Register, LowerError>>,
) -> Result<OperandListId, LowerError> {
    let list = OperandListId(lower_index(routine, sites.lists.len())?);
    sites
        .lists
        .push(registers.collect::<Result<Box<[_]>, _>>()?);
    Ok(list)
}

/// Declared arrays that no statement assigns as a whole start uninitialized.
fn undeclared_arrays(
    function: &Function,
    places: &Places,
    shapes: &Shapes,
    blocks: &[cfg::CheckedBlock<'_>],
) -> Vec<Register> {
    let assigned: std::collections::HashSet<_> = blocks
        .iter()
        .flat_map(|block| block.operations.iter())
        .filter_map(|record| match record.op {
            Op::Store(addr, _) => places.address(addr),
            _ => None,
        })
        .collect();
    places
        .locals
        .iter()
        .enumerate()
        .skip(function.params.len())
        .filter(|&(_, &local)| {
            matches!(places.registers[local.index()],
                RegisterType::Composite(shape) if shapes.is_array(shape))
                && !assigned.contains(&local)
        })
        .map(|(_, &local)| local)
        .collect()
}

fn lower_boolean(op: Operator) -> Option<BooleanOp> {
    match op {
        Operator::Equal => Some(BooleanOp::Equal),
        Operator::NotEqual => Some(BooleanOp::NotEqual),
        _ => None,
    }
}

fn lower_function(
    function: &Function,
    routine: usize,
    shapes: &mut Shapes,
    mut call_target: impl FnMut(usize) -> Result<CallTarget, LowerError>,
) -> Result<FunctionImage, LowerError> {
    let signature_error = || LowerError::UnsupportedSignature { routine };
    if function.generator.is_some() || function.params.iter().any(|&by_ref| by_ref) {
        return Err(signature_error());
    }
    let result = match function.result {
        Type::Void => None,
        ref ty => Some(shapes.value(ty).ok_or_else(signature_error)?),
    };
    let reachable = cfg::reachable(&function.code, routine, result.is_some())?;
    let places = Places::collect(function, routine, &reachable, shapes)?;
    let context = Context {
        routine,
        places: &places,
        shapes,
    };
    let register = |index| places.register(index);
    let parameters: Box<[_]> = places
        .locals
        .get(..function.params.len())
        .ok_or(LowerError::UnsupportedStorage { routine })?
        .into();

    let mut blocks = Vec::new();
    let mut sites = Sites::default();
    let undeclared = undeclared_arrays(function, &places, shapes, &reachable);
    for (block_index, block) in reachable.into_iter().enumerate() {
        let exit = match block.exit.value {
            Exit::Fallthrough(to) => Exit::Fallthrough(to),
            Exit::Jump(to) => Exit::Jump(to),
            Exit::Branch(condition, yes, no) => Exit::Branch(register(condition)?, yes, no),
            Exit::Return(value) => Exit::Return(value.map(register).transpose()?),
            Exit::Trap => Exit::Trap,
        };
        let mut lowered = Block {
            operations: Vec::new(),
            statements: Vec::new(),
            exit: Located {
                value: exit,
                span: block.exit.span,
            },
        };
        if block_index == 0 {
            // Storage for a declared array exists before its elements are set.
            lowered
                .operations
                .extend(undeclared.iter().map(|&array| Located {
                    value: Instruction::ArrayDeclare(array),
                    span: function.span,
                }));
        }
        for (offset, record) in block.operations.iter().enumerate() {
            match record.op {
                Op::Statement => lowered.statements.push(Statement {
                    before: lowered.operations.len(),
                    span: record.span,
                }),
                Op::Nop => {}
                Op::Index(dst, ..) if places.element(dst).is_some() => {}
                // None of the values in this slice has a destructor to run.
                Op::Drop(addr) if places.address(addr).is_some() => {}
                Op::Address(dst, _) if places.address(dst).is_some() => {}
                // The unit value of a statement-position match has no storage.
                Op::Aggregate(dst, ref fields)
                    if fields.is_empty() && matches!(function.registers[dst], Type::Void) => {}
                _ => lowered.operations.push(Located {
                    value: lower_operation(
                        &record.op,
                        block.start + offset,
                        record.span,
                        &context,
                        &mut sites,
                        &mut call_target,
                    )?,
                    span: record.span,
                }),
            }
        }
        blocks.push(lowered);
    }
    let registers = places.registers.into_boxed_slice();
    let prologue = lifetimes::insert(
        &mut blocks,
        &parameters,
        &mut sites.calls,
        &sites.lists,
        &registers,
    );
    let body = layout::assign(prologue, blocks, routine)?;
    Ok(FunctionImage {
        registers,
        parameters,
        result,
        calls: sites.calls.into_boxed_slice(),
        lists: sites.lists.into_boxed_slice(),
        texts: sites.texts.into_boxed_slice(),
        outputs: sites.outputs.into_boxed_slice(),
        words: encode(body.instructions),
        spans: body.spans,
        statements: body.statements,
    })
}

/// Call sites and output templates built while lowering one function.
#[derive(Default)]
struct Sites {
    texts: Vec<Arc<str>>,
    calls: Vec<CallSite>,
    lists: Vec<Box<[Register]>>,
    outputs: Vec<OutputTemplate>,
}

/// Read-only facts about the function being lowered.
#[derive(Clone, Copy)]
struct Context<'a> {
    routine: usize,
    places: &'a Places,
    shapes: &'a Shapes,
}

fn lower_operation(
    op: &Op,
    instruction: usize,
    span: Span,
    context: &Context,
    sites: &mut Sites,
    call_target: &mut impl FnMut(usize) -> Result<CallTarget, LowerError>,
) -> Result<Instruction, LowerError> {
    let Context {
        routine,
        places,
        shapes,
    } = *context;
    let register = |index| places.register(index);
    let unsupported = || LowerError::UnsupportedOperation {
        routine,
        instruction,
        span,
    };
    Ok(match op {
        Op::Literal(dst, Literal::Integer(value)) => {
            Instruction::Constant(register(*dst)?, Scalar::I64(*value))
        }
        Op::Literal(dst, Literal::Real(value)) => {
            Instruction::Constant(register(*dst)?, Scalar::from_f64(*value))
        }
        Op::Literal(dst, Literal::Boolean(value)) => {
            Instruction::Constant(register(*dst)?, Scalar::Bool(*value))
        }
        Op::Move(dst, src) if places.address(*src).is_some() => {
            let owner = places.address(*src).ok_or_else(unsupported)?;
            Instruction::Borrow(register(*dst)?, owner)
        }
        Op::LiteralString(dst, text) => {
            let index = sites
                .texts
                .iter()
                .position(|known| known == text)
                .unwrap_or(sites.texts.len());
            if index == sites.texts.len() {
                sites.texts.push(text.clone());
            }
            Instruction::TextConstant(register(*dst)?, TextId(lower_index(routine, index)?))
        }
        Op::ArrayLiteral(dst, items) => {
            let list = new_list(routine, sites, items.iter().map(|&item| register(item)))?;
            Instruction::ArrayNew(register(*dst)?, list)
        }
        Op::Load(dst, addr) if places.element(*addr).is_some() => {
            let (array, indices, _) = places.element(*addr).ok_or_else(unsupported)?;
            let list = new_list(routine, sites, indices.iter().map(|&index| register(index)))?;
            Instruction::ArrayLoad(register(*dst)?, array, list)
        }
        Op::Store(addr, value) if places.element(*addr).is_some() => {
            let (array, indices, writable) = places.element(*addr).ok_or_else(unsupported)?;
            if !writable {
                return Err(unsupported());
            }
            let list = new_list(routine, sites, indices.iter().map(|&index| register(index)))?;
            Instruction::ArrayStore(array, list, register(*value)?)
        }
        Op::Store(addr, _) if places.is_payload_copy(*addr) => return Err(unsupported()),
        Op::Load(dst, addr) if places.is_payload_copy(*addr) => {
            Instruction::Copy(register(*dst)?, register(*addr)?)
        }
        Op::Field(dst, base, field) if places.is_projected(*dst) => {
            let base = enum_place(places, *base).ok_or_else(unsupported)?;
            Instruction::LoanField(register(*dst)?, base, lower_index(routine, *field)?)
        }
        Op::Index(dst, base, indices, _) if places.is_projected(*dst) => {
            let base = enum_place(places, *base).ok_or_else(unsupported)?;
            let list = new_list(routine, sites, indices.iter().map(|&index| register(index)))?;
            Instruction::LoanElement(register(*dst)?, base, list)
        }
        Op::EnumPayload(dst, base, variant, Projection::Address, _)
            if places.is_projected(*dst) =>
        {
            let base = enum_place(places, *base).ok_or_else(unsupported)?;
            let index = variant_index(places, shapes, base, variant).ok_or_else(unsupported)?;
            Instruction::LoanPayload(register(*dst)?, base, index)
        }
        Op::Take(dst, addr) if places.is_projected(*addr) => {
            Instruction::ReadLoan(register(*dst)?, register(*addr)?)
        }
        Op::Field(dst, base, field) if places.is_payload_copy(*dst) => {
            let base = enum_place(places, *base).ok_or_else(unsupported)?;
            Instruction::Extract(register(*dst)?, base, lower_index(routine, *field)?)
        }
        Op::FunctionValue(dst, target, captures) => {
            let CallTarget::Direct(function) = call_target(*target)? else {
                return Err(unsupported());
            };
            let list = new_list(
                routine,
                sites,
                captures.iter().map(|&capture| register(capture)),
            )?;
            Instruction::Closure(register(*dst)?, function, list)
        }
        Op::Invoke(dst, callee, args) => {
            let site = CallSiteId(lower_index(routine, sites.calls.len())?);
            sites.calls.push(CallSite {
                target: CallTarget::Closure(register(*callee)?),
                arguments: args
                    .iter()
                    .map(|&arg| register(arg))
                    .collect::<Result<Box<[_]>, _>>()?,
                destination: dst.map(register).transpose()?,
            });
            Instruction::Call(site)
        }
        // Values in this slice are plain data without destructors, so moving
        // out of a local leaves an unobservable copy behind.
        Op::Take(dst, addr) if places.is_payload_copy(*addr) => {
            Instruction::Copy(register(*dst)?, register(*addr)?)
        }
        Op::Take(dst, addr) if places.address(*addr).is_some() => {
            let place = places.address(*addr).ok_or_else(unsupported)?;
            Instruction::Copy(register(*dst)?, place)
        }
        Op::Load(dst, addr) => match places.address(*addr) {
            Some(place) => Instruction::Copy(register(*dst)?, place),
            None => Instruction::ReadLoan(register(*dst)?, register(*addr)?),
        },
        Op::Store(addr, src) => match places.address(*addr) {
            Some(place) => Instruction::Copy(place, register(*src)?),
            None => Instruction::WriteLoan(register(*addr)?, register(*src)?),
        },
        Op::Move(dst, src) | Op::FrameLoad(dst, src) | Op::FrameStore(dst, src) => {
            Instruction::Copy(register(*dst)?, register(*src)?)
        }
        Op::Binary(dst, op, lhs, rhs) => {
            let operands = (places.scalar(*lhs), places.scalar(*rhs));
            let (dst, lhs, rhs) = (register(*dst)?, register(*lhs)?, register(*rhs)?);
            if places.is_text(lhs.index()) && places.is_text(rhs.index()) {
                return Ok(match *op {
                    Operator::Concat => Instruction::TextConcat(dst, lhs, rhs),
                    other => Instruction::TextCompare(
                        lower_text(other).ok_or_else(unsupported)?,
                        dst,
                        lhs,
                        rhs,
                    ),
                });
            }
            match operands {
                (Some(ScalarType::I64), Some(ScalarType::I64)) if *op == Operator::RealDivide => {
                    Instruction::RealDivide(dst, lhs, rhs)
                }
                (Some(ScalarType::I64), Some(ScalarType::I64)) => {
                    Instruction::Integer(lower_integer(*op).ok_or_else(unsupported)?, dst, lhs, rhs)
                }
                (Some(ScalarType::F64), Some(ScalarType::F64)) => {
                    Instruction::Float(lower_float(*op).ok_or_else(unsupported)?, dst, lhs, rhs)
                }
                (Some(ScalarType::Bool), Some(ScalarType::Bool)) => {
                    Instruction::Boolean(lower_boolean(*op).ok_or_else(unsupported)?, dst, lhs, rhs)
                }
                _ => return Err(unsupported()),
            }
        }
        Op::Unary(dst, op, src) => {
            let (dst, source) = (register(*dst)?, register(*src)?);
            match (*op, places.scalar(*src)) {
                (Operator::Subtract, Some(ScalarType::I64)) => {
                    Instruction::NegateInteger(dst, source)
                }
                (Operator::Subtract, Some(ScalarType::F64)) => {
                    Instruction::NegateFloat(dst, source)
                }
                (Operator::Not, Some(ScalarType::Bool)) => Instruction::Not(dst, source),
                (Operator::Add, Some(_)) => Instruction::Copy(dst, source),
                _ => return Err(unsupported()),
            }
        }
        Op::Builtin(dst, intrinsic, args) if is_text_builtin(*intrinsic) => {
            let operands = args
                .iter()
                .map(|&arg| register(arg))
                .collect::<Result<Vec<_>, _>>()?;
            let dst = register(*dst)?;
            match (intrinsic, operands.as_slice()) {
                (Intrinsic::Length | Intrinsic::StringLength, &[text])
                    if places.is_text(text.index()) =>
                {
                    Instruction::TextLength(dst, text)
                }
                (Intrinsic::Length | Intrinsic::ArrayLength, &[array])
                    if !places.is_text(array.index()) =>
                {
                    Instruction::ArrayLength(dst, array)
                }
                (Intrinsic::Left, &[text, count]) => Instruction::TextLeft(dst, text, count),
                (Intrinsic::Right, &[text, count]) => Instruction::TextRight(dst, text, count),
                (Intrinsic::Mid, &[text, start, count]) => {
                    let list = OperandListId(lower_index(routine, sites.lists.len())?);
                    sites.lists.push(Box::new([start, count]));
                    Instruction::TextMid(dst, text, list)
                }
                _ => return Err(unsupported()),
            }
        }
        Op::Builtin(dst, intrinsic, args) => {
            let [source] = args.as_slice() else {
                return Err(unsupported());
            };
            let (dst, source) = (register(*dst)?, register(*source)?);
            match (intrinsic, places.scalar(source.index())) {
                (Intrinsic::CastF64, Some(ScalarType::I64)) => {
                    Instruction::IntegerToFloat(dst, source)
                }
                (Intrinsic::CastF64, Some(ScalarType::F64))
                | (Intrinsic::CastI64, Some(ScalarType::I64)) => Instruction::Copy(dst, source),
                (Intrinsic::CastI64, Some(ScalarType::F64)) => {
                    Instruction::FloatToInteger(dst, source)
                }
                _ => return Err(unsupported()),
            }
        }
        Op::Call(dst, callee, args) => {
            let site = CallSiteId(lower_index(routine, sites.calls.len())?);
            sites.calls.push(CallSite {
                target: call_target(*callee)?,
                arguments: args
                    .iter()
                    .copied()
                    .map(register)
                    .collect::<Result<Box<[_]>, _>>()?,
                destination: dst.map(register).transpose()?,
            });
            Instruction::Call(site)
        }
        Op::Aggregate(dst, fields) => {
            let list = OperandListId(lower_index(routine, sites.lists.len())?);
            let fields = fields
                .iter()
                .map(|&field| register(field))
                .collect::<Result<Box<[_]>, _>>()?;
            sites.lists.push(fields);
            Instruction::Tuple(register(*dst)?, list)
        }
        Op::Extract(dst, base, field) => Instruction::Extract(
            register(*dst)?,
            register(*base)?,
            lower_index(routine, *field)?,
        ),
        Op::EnumConstruct(dst, variant, payload) => {
            let dst_register = register(*dst)?;
            let index =
                variant_index(places, shapes, dst_register, variant).ok_or_else(unsupported)?;
            match payload {
                None => Instruction::EnumUnit(dst_register, index),
                Some(payload) => Instruction::EnumWrap(dst_register, index, register(*payload)?),
            }
        }
        Op::EnumTest(dst, base, variant) => {
            let base = enum_place(places, *base).ok_or_else(unsupported)?;
            let index = variant_index(places, shapes, base, variant).ok_or_else(unsupported)?;
            Instruction::EnumIs(register(*dst)?, base, index)
        }
        Op::EnumPayload(dst, base, variant, projection, _)
            if *projection == Projection::Value || places.is_payload_copy(*dst) =>
        {
            let base = enum_place(places, *base).ok_or_else(unsupported)?;
            let index = variant_index(places, shapes, base, variant).ok_or_else(unsupported)?;
            Instruction::EnumPayload(register(*dst)?, base, index)
        }
        Op::CheckStep(step) => Instruction::CheckStep(register(*step)?),
        Op::Output(registers, style) => {
            let pieces = (0..registers.len()).map(OutputPiece::Argument).collect();
            let arguments = registers
                .iter()
                .map(|&reg| register(reg))
                .collect::<Result<Vec<_>, _>>()?;
            let output = OutputId(lower_index(routine, sites.outputs.len())?);
            sites.outputs.push(OutputTemplate {
                style: *style,
                pieces,
            });
            let site = CallSiteId(lower_index(routine, sites.calls.len())?);
            sites.calls.push(CallSite {
                target: CallTarget::Output(output),
                arguments: arguments.into_boxed_slice(),
                destination: None,
            });
            Instruction::Call(site)
        }
        _ => return Err(unsupported()),
    })
}

/// The register an enum operand reads: the local behind an address, or the
/// register itself.
fn enum_place(places: &Places, operand: usize) -> Option<Register> {
    places
        .address(operand)
        .or_else(|| places.register(operand).ok())
}

fn variant_index(
    places: &Places,
    shapes: &Shapes,
    enum_register: Register,
    variant: &cyallo_ast::EnumVariant,
) -> Option<u32> {
    let shape = match *places.registers.get(enum_register.index())? {
        RegisterType::Composite(shape) => shape,
        RegisterType::Loan {
            pointee: super::ValueType::Composite(shape),
            ..
        } => shape,
        _ => return None,
    };
    shapes.variant_index(shape, variant.declaration)
}
