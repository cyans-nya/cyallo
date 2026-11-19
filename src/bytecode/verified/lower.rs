use super::{
    BooleanOp, CallSite, CallSiteId, CallTarget, Extremum, FORMAT_VERSION, FloatOp, FunctionId,
    FunctionImage, HostBindingId, Implementation, ImplementationId, InputKind, Instruction,
    InstructionId, IntegerOp, LoanAccess, ModuleImage, OperandListId, OutputId, OutputPiece,
    OutputTemplate, Register, RegisterType, Scalar, ScalarHostBinding, ScalarType, TextId,
    TextMapOp, TextOp, TextTestOp, TraitId, ValueType, VerifiedModule, VerifyError,
    cfg::{self, Block, Exit, Located, Statement},
    encode, layout, lifetimes,
    loans::Places,
    shapes::Shapes,
};
use crate::{
    Diagnostic, Span, Type,
    ir::{Function, Op, Program, Routine},
};
use cyallo_ast::{ConstantValue, Literal, MemoryKind, NumberKind, Operator};
use cyallo_ir::{
    enums::Projection,
    intrinsic::Intrinsic,
    lookup::{Operation, ResumeInput},
    services::Metadata,
};
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
        /// What has no verified representation, for the person reading it.
        detail: String,
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
        /// The checked operation that has no verified form.
        operation: String,
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
            Self::UnsupportedStorage { routine, detail } => {
                write!(
                    f,
                    "unsupported frame storage in routine {routine}: {detail}"
                )
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
                operation,
                ..
            } => write!(
                f,
                "unsupported operation {instruction} in routine {routine}: {operation}"
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
        Self::lower_checked(program.functions(), routine, Some(program.services.clone()))
    }

    pub(super) fn lower_checked(
        checked: &[Routine],
        routine: usize,
        services: Option<Arc<Metadata>>,
    ) -> Result<Self, LowerError> {
        Self::lower_with_hosts(checked, routine, &HashMap::new(), Vec::new(), services)
    }

    pub(super) fn lower_with_hosts(
        checked: &[Routine],
        routine: usize,
        host_ids: &HashMap<usize, HostBindingId>,
        host_bindings: Vec<ScalarHostBinding>,
        services: Option<Arc<Metadata>>,
    ) -> Result<Self, LowerError> {
        let entry = FunctionId(0);
        let mut ids = HashMap::from([(routine, entry)]);
        let mut routines = vec![routine];
        let mut functions = Vec::new();
        // How a routine receives its parameter at a position: shared or
        // exclusive access, when it is a reference.
        let param_access = |callee: usize, position: usize| -> Option<LoanAccess> {
            let function = checked.get(callee)?.checked().ok()?;
            match function.locals.get(position)?.ty {
                Type::Reference { mutable, .. } => Some(if mutable {
                    LoanAccess::Exclusive
                } else {
                    LoanAccess::Shared
                }),
                _ => None,
            }
        };
        let mut shapes = Shapes::new(services.clone());
        let globals = Globals::collect(checked, &mut shapes)?;
        let implementations = Implementations::collect(checked, services.as_deref(), &mut shapes)?;
        // Every method of an implementation can be called through an object.
        for draft in &implementations.drafts {
            for &method in &draft.routines {
                if let std::collections::hash_map::Entry::Vacant(slot) = ids.entry(method) {
                    let next = lower_index(method, routines.len()).map(FunctionId)?;
                    routines.push(method);
                    slot.insert(next);
                }
            }
        }
        while let Some(&id) = routines.get(functions.len()) {
            let function = checked
                .get(id)
                .ok_or(LowerError::UnknownRoutine(id))?
                .checked()
                .map_err(|diagnostic| LowerError::Analysis {
                    routine: id,
                    diagnostic: Box::new(diagnostic.clone()),
                })?;
            functions.push(lower_function(
                function,
                id,
                &mut shapes,
                &globals,
                &implementations,
                &param_access,
                |target| {
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
                },
            )?);
        }
        let table = implementations
            .drafts
            .iter()
            .map(|draft| {
                Ok(Implementation {
                    trait_id: draft.trait_id,
                    receiver: draft.receiver,
                    methods: draft
                        .routines
                        .iter()
                        .map(|method| {
                            ids.get(method)
                                .copied()
                                .ok_or(LowerError::UnknownRoutine(*method))
                        })
                        .collect::<Result<Box<[_]>, _>>()?,
                })
            })
            .collect::<Result<Box<[_]>, LowerError>>()?;
        let super::shapes::Tables {
            shapes,
            signatures,
            managed,
            traits,
        } = shapes.into_parts();
        ModuleImage {
            version: FORMAT_VERSION,
            entry,
            shapes,
            signatures,
            globals: globals.types.clone().into_boxed_slice(),
            managed,
            traits,
            implementations: table,
            functions: functions.into_boxed_slice(),
        }
        .verify_with_host_bindings(host_bindings)
        .map_err(LowerError::Verification)
    }
}

pub(super) fn storage(routine: usize, detail: impl Into<String>) -> LowerError {
    LowerError::UnsupportedStorage {
        routine,
        detail: detail.into(),
    }
}

/// A checked type or operation as text, short enough for a message.
pub(super) fn describe(value: &impl fmt::Debug) -> String {
    let text = format!("{value:?}");
    match text.char_indices().nth(160) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text,
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

fn lower_input(ty: &Type) -> Option<InputKind> {
    match ty {
        Type::String => Some(InputKind::Text),
        Type::Integer | Type::Numeric(NumberKind::I64) => Some(InputKind::Integer),
        Type::Real => Some(InputKind::Float),
        Type::Boolean => Some(InputKind::Bool),
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

/// An implementation a trait box names, before its routines are numbered.
struct ImplementationDraft {
    trait_id: TraitId,
    receiver: ValueType,
    routines: Vec<usize>,
}

/// The implementations that some routine boxes a value for, numbered in the
/// order of the declared implementations.
#[derive(Default)]
pub(super) struct Implementations {
    ids: HashMap<usize, ImplementationId>,
    drafts: Vec<ImplementationDraft>,
}

impl Implementations {
    fn collect(
        checked: &[Routine],
        services: Option<&Metadata>,
        shapes: &mut Shapes,
    ) -> Result<Self, LowerError> {
        let mut used = std::collections::BTreeSet::new();
        for routine in checked {
            let Ok(function) = routine.checked() else {
                continue;
            };
            for record in &function.code {
                if let Op::TraitBox(_, _, implementation) = record.op {
                    used.insert(implementation);
                }
            }
        }
        let mut found = Self::default();
        let Some(services) = services else {
            return Ok(found);
        };
        for declared in used {
            let implementation = services
                .implementations
                .get(declared)
                .ok_or_else(|| storage(0, format!("implementation {declared} is not declared")))?;
            let trait_id = shapes.trait_of(implementation.trait_id).ok_or_else(|| {
                storage(
                    0,
                    format!("the trait of implementation {declared} has no verified form"),
                )
            })?;
            let receiver = shapes.value(&implementation.receiver).ok_or_else(|| {
                storage(
                    0,
                    format!(
                        "the receiver of implementation {declared} is {}",
                        describe(&implementation.receiver)
                    ),
                )
            })?;
            let id = ImplementationId(lower_index(0, found.drafts.len())?);
            found.ids.insert(declared, id);
            found.drafts.push(ImplementationDraft {
                trait_id,
                receiver,
                routines: implementation.methods.clone(),
            });
        }
        Ok(found)
    }

    fn id(&self, declared: usize) -> Option<ImplementationId> {
        self.ids.get(&declared).copied()
    }
}

/// Module-level variables: the cell locals of `<main>` that other routines
/// reach through global slots. A variable keeps the index it has among
/// `<main>`'s locals.
#[derive(Default)]
pub(super) struct Globals {
    types: Vec<ValueType>,
    present: Vec<bool>,
}

impl Globals {
    fn collect(checked: &[Routine], shapes: &mut Shapes) -> Result<Self, LowerError> {
        let mut globals = Self::default();
        let Some(Ok(main)) = checked.first().map(Routine::checked) else {
            return Ok(globals);
        };
        if &*main.name != "<main>" {
            return Ok(globals);
        }
        let mut used = std::collections::BTreeSet::new();
        for routine in checked {
            let Ok(function) = routine.checked() else {
                continue;
            };
            for record in &function.code {
                if let Op::Address(_, slot) = record.op
                    && slot.global
                {
                    used.insert(slot.index);
                }
            }
        }
        let Some(&last) = used.last() else {
            return Ok(globals);
        };
        // Unreferenced indices keep a placeholder type that nothing borrows.
        globals.types = vec![ValueType::Scalar(ScalarType::I64); last + 1];
        globals.present = vec![false; last + 1];
        for index in used {
            let local = main
                .locals
                .get(index)
                .ok_or_else(|| storage(0, format!("module variable {index} does not exist")))?;
            globals.types[index] = shapes.value(&local.ty).ok_or_else(|| {
                storage(
                    0,
                    format!("module variable {index} has type {}", describe(&local.ty)),
                )
            })?;
            globals.present[index] = true;
        }
        Ok(globals)
    }

    /// The declared type of the variable at this index of `<main>`'s locals.
    pub(super) fn pointee(&self, index: usize) -> Option<ValueType> {
        self.present
            .get(index)
            .copied()
            .unwrap_or(false)
            .then(|| self.types[index])
    }
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

/// The number of a string constant of the function, adding it when new.
fn intern_text(routine: usize, sites: &mut Sites, text: &Arc<str>) -> Result<TextId, LowerError> {
    let index = match sites.texts.iter().position(|known| known == text) {
        Some(index) => index,
        None => {
            sites.texts.push(text.clone());
            sites.texts.len() - 1
        }
    };
    Ok(TextId(lower_index(routine, index)?))
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
    globals: &Globals,
    implementations: &Implementations,
    param_access: &dyn Fn(usize, usize) -> Option<LoanAccess>,
    mut call_target: impl FnMut(usize) -> Result<CallTarget, LowerError>,
) -> Result<FunctionImage, LowerError> {
    let signature_error = || LowerError::UnsupportedSignature { routine };
    if function.params.iter().any(|&by_ref| by_ref) {
        return Err(signature_error());
    }
    let generator = match &function.generator {
        None => None,
        Some(ty) => match shapes.value(ty) {
            Some(ValueType::Generator(description)) => Some(description),
            _ => return Err(signature_error()),
        },
    };
    let result = match function.result {
        Type::Void => None,
        ref ty => Some(shapes.value(ty).ok_or_else(signature_error)?),
    };
    let reachable = cfg::reachable(&function.code, routine, result.is_some())?;
    let places = Places::collect(function, routine, &reachable, shapes, globals, param_access)?;
    let context = Context {
        routine,
        places: &places,
        shapes,
        implementations,
    };
    let register = |index| places.register(index);
    let parameters: Box<[_]> = places
        .locals
        .get(..function.params.len())
        .ok_or_else(|| storage(routine, "the routine has fewer locals than parameters"))?
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
                // A value with no counted owners has nothing to release.
                Op::Drop(addr)
                    if places
                        .address(addr)
                        .is_some_and(|place| !context.needs_drop(place)) => {}
                Op::Lookup(_, Operation::DropValue, ref args)
                    if args.len() == 1
                        && places
                            .registers
                            .get(args[0])
                            .is_some_and(|kind| !context.register_needs_drop(*kind)) => {}
                Op::Address(dst, _)
                    if places.address(dst).is_some() && places.borrowed_address(dst).is_none() => {}
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
    release_owned_at_returns(&mut blocks, &parameters, &places, &context, &sites);
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
        generator,
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

/// Release the owners a routine still holds when it returns. The checked IR
/// releases a local at the end of its scope, which an explicit `return` skips,
/// and releases a parameter nowhere, so both leaked until the frame vanished.
/// A drop of a value that was moved out releases nothing, so only definite
/// initialization matters: a register is released at a return if every path
/// to it has written the register. Owners written on only some paths stay
/// unreleased there.
fn release_owned_at_returns(
    blocks: &mut [Block],
    parameters: &[Register],
    places: &Places,
    context: &Context<'_>,
    sites: &Sites,
) {
    use std::collections::HashSet;
    let owned: Vec<Register> = places
        .locals
        .iter()
        .copied()
        .filter(|register| {
            places
                .registers
                .get(register.index())
                .is_some_and(|kind| context.register_needs_drop(*kind))
        })
        .collect();
    if owned.is_empty() {
        return;
    }
    let mut written: Vec<Option<HashSet<Register>>> = vec![None; blocks.len()];
    written[0] = Some(parameters.iter().copied().collect());
    let mut pending = vec![0usize];
    while let Some(index) = pending.pop() {
        let Some(mut state) = written[index].clone() else {
            continue;
        };
        for operation in &blocks[index].operations {
            state.extend(
                operation
                    .value
                    .register_access(&sites.calls, &sites.lists)
                    .write,
            );
        }
        for successor in blocks[index].exit.value.successors().into_iter().flatten() {
            let merged = match &written[successor.0] {
                None => state.clone(),
                Some(known) => known.intersection(&state).copied().collect(),
            };
            if written[successor.0].as_ref() != Some(&merged) {
                written[successor.0] = Some(merged);
                pending.push(successor.0);
            }
        }
    }
    for (index, block) in blocks.iter_mut().enumerate() {
        let (Exit::Return(returned), Some(mut state)) = (block.exit.value, written[index].clone())
        else {
            continue;
        };
        for operation in &block.operations {
            state.extend(
                operation
                    .value
                    .register_access(&sites.calls, &sites.lists)
                    .write,
            );
        }
        for &register in owned.iter().rev() {
            if state.contains(&register) && returned != Some(register) {
                block.operations.push(Located {
                    value: Instruction::Drop(register),
                    span: block.exit.span,
                });
            }
        }
    }
}

/// Call sites and output templates built while lowering one function.
#[derive(Default)]
struct Sites {
    texts: Vec<Arc<str>>,
    calls: Vec<CallSite>,
    lists: Vec<Box<[Register]>>,
    outputs: Vec<OutputTemplate>,
}

impl Context<'_> {
    /// Whether the value in this register changes counts when released.
    fn needs_drop(&self, register: Register) -> bool {
        self.places
            .registers
            .get(register.index())
            .is_some_and(|kind| self.register_needs_drop(*kind))
    }

    fn register_needs_drop(&self, kind: RegisterType) -> bool {
        kind.value().is_some_and(|ty| self.shapes.needs_drop(ty))
    }

    /// The `Some` and `None` variants of an option held by this register.
    fn option_variants(&self, register: Register) -> Option<(u32, u32)> {
        let RegisterType::Composite(shape) = *self.places.registers.get(register.index())? else {
            return None;
        };
        self.shapes.option_variants(shape)
    }

    /// The `Ok` and `Err` variants of a result held by this register.
    fn result_variants(&self, register: Register) -> Option<(u32, u32)> {
        let RegisterType::Composite(shape) = *self.places.registers.get(register.index())? else {
            return None;
        };
        self.shapes.result_variants(shape)
    }

    /// Whether the payload of this variant changes counts when released.
    fn payload_needs_drop(&self, register: Register, variant: u32) -> bool {
        let Some(RegisterType::Composite(shape)) = self.places.registers.get(register.index())
        else {
            return false;
        };
        self.shapes
            .variant_payload(*shape, variant)
            .is_some_and(|payload| self.shapes.needs_drop(payload))
    }
}

/// Read-only facts about the function being lowered.
#[derive(Clone, Copy)]
struct Context<'a> {
    routine: usize,
    places: &'a Places,
    shapes: &'a Shapes,
    implementations: &'a Implementations,
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
        implementations,
    } = *context;
    let register = |index| places.register(index);
    // A generator operand is a local's address or a loan of one.
    let generator = |index| match places.address(index) {
        Some(place) => Ok(place),
        None => places.register(index),
    };
    let unsupported = || LowerError::UnsupportedOperation {
        routine,
        instruction,
        span,
        operation: describe(op),
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
        Op::Lookup(dst, Operation::Range(inclusive), args) if args.len() == 2 => {
            let (dst, start, end) = (register(*dst)?, register(args[0])?, register(args[1])?);
            if *inclusive {
                Instruction::RangeInclusive(dst, start, end)
            } else {
                Instruction::RangeNew(dst, start, end)
            }
        }
        Op::Lookup(dst, Operation::CollectionFrom(false), args) if args.len() == 1 => {
            Instruction::VecFrom(register(*dst)?, register(args[0])?)
        }
        Op::Lookup(dst, Operation::CollectionLiteral(false), args) => {
            let list = new_list(routine, sites, args.iter().map(|&item| register(item)))?;
            Instruction::VecNew(register(*dst)?, list)
        }
        Op::Lookup(_, Operation::CollectionPush(false), args) if args.len() == 2 => {
            let target = enum_place(places, args[0]).ok_or_else(unsupported)?;
            Instruction::VecPush(target, register(args[1])?)
        }
        Op::Lookup(dst, Operation::CollectionLength, args) if args.len() == 1 => {
            let vector = enum_place(places, args[0]).ok_or_else(unsupported)?;
            Instruction::VecLength(register(*dst)?, vector)
        }
        Op::Lookup(dst, Operation::CellNew, args) if args.len() == 1 => {
            Instruction::CellNew(register(*dst)?, register(args[0])?)
        }
        Op::Lookup(dst, Operation::CellRead, args) if args.len() == 1 => {
            let cell = enum_place(places, args[0]).ok_or_else(unsupported)?;
            Instruction::CellRead(register(*dst)?, cell)
        }
        Op::Lookup(_, Operation::CellWrite, args) if args.len() == 2 => {
            let cell = enum_place(places, args[0]).ok_or_else(unsupported)?;
            Instruction::CellWrite(cell, register(args[1])?)
        }
        Op::Lookup(dst, Operation::CellReplace, args) if args.len() == 2 => {
            let cell = enum_place(places, args[0]).ok_or_else(unsupported)?;
            Instruction::CellReplace(register(*dst)?, cell, register(args[1])?)
        }
        Op::Lookup(dst, Operation::Slice(_), args) if args.len() == 1 => {
            Instruction::SliceOf(register(*dst)?, register(args[0])?)
        }
        Op::Builtin(dst, Intrinsic::ArraySum, args) if args.len() == 1 => {
            Instruction::Sum(register(*dst)?, register(args[0])?)
        }
        Op::Builtin(dst, Intrinsic::Length | Intrinsic::ArrayLength, args)
            if args.len() == 1
                && matches!(
                    places.registers.get(args[0]),
                    Some(RegisterType::Loan { .. })
                ) =>
        {
            Instruction::SliceLength(register(*dst)?, register(args[0])?)
        }
        Op::Lookup(dst, Operation::ManagedNew(MemoryKind::Gc, false), args) if args.len() == 1 => {
            Instruction::GcNew(register(*dst)?, register(args[0])?)
        }
        Op::ManagedAddress(dst, handle) => {
            Instruction::GcBorrow(register(*dst)?, register(*handle)?)
        }
        Op::Address(dst, _) if let Some(owner) = places.borrowed_address(*dst) => {
            Instruction::Borrow(register(*dst)?, owner)
        }
        Op::Address(dst, _) if let Some(global) = places.global_address(*dst) => {
            Instruction::BorrowGlobal(register(*dst)?, global)
        }
        Op::TraitBox(dst, value, implementation) => Instruction::TraitBox(
            register(*dst)?,
            register(*value)?,
            implementations
                .id(*implementation)
                .ok_or_else(unsupported)?,
        ),
        Op::TraitCall(dst, object, method, args) => {
            let site = CallSiteId(lower_index(routine, sites.calls.len())?);
            sites.calls.push(CallSite {
                target: CallTarget::Method {
                    object: register(*object)?,
                    method: lower_index(routine, *method)?,
                },
                arguments: args
                    .iter()
                    .map(|&arg| register(arg))
                    .collect::<Result<Box<[_]>, _>>()?,
                destination: dst.map(register).transpose()?,
            });
            Instruction::Call(site)
        }
        Op::LiteralString(dst, text) => {
            let text = intern_text(routine, sites, text)?;
            Instruction::TextConstant(register(*dst)?, text)
        }
        Op::Frozen(dst, value) => match value.as_ref() {
            ConstantValue::String(text) => {
                let text = intern_text(routine, sites, text)?;
                Instruction::TextConstant(register(*dst)?, text)
            }
            ConstantValue::Integer(value) => {
                Instruction::Constant(register(*dst)?, Scalar::I64(*value))
            }
            ConstantValue::Real(value) => {
                Instruction::Constant(register(*dst)?, Scalar::from_f64(*value))
            }
            ConstantValue::Boolean(value) => {
                Instruction::Constant(register(*dst)?, Scalar::Bool(*value))
            }
            _ => return Err(unsupported()),
        },
        Op::Lookup(dst, Operation::CloneValue, args) if args.len() == 1 => {
            let source = enum_place(places, args[0]).ok_or_else(unsupported)?;
            Instruction::Clone(register(*dst)?, source)
        }
        Op::Builtin(
            dst,
            intrinsic @ (Intrinsic::Trim | Intrinsic::Upper | Intrinsic::Lower),
            args,
        ) if args.len() == 1 => {
            let op = match intrinsic {
                Intrinsic::Trim => TextMapOp::Trim,
                Intrinsic::Upper => TextMapOp::Upper,
                _ => TextMapOp::Lower,
            };
            Instruction::TextMap(op, register(*dst)?, register(args[0])?)
        }
        Op::Builtin(
            dst,
            intrinsic @ (Intrinsic::Contains | Intrinsic::StartsWith | Intrinsic::EndsWith),
            args,
        ) if args.len() == 2 => {
            let op = match intrinsic {
                Intrinsic::Contains => TextTestOp::Contains,
                Intrinsic::StartsWith => TextTestOp::StartsWith,
                _ => TextTestOp::EndsWith,
            };
            Instruction::TextTest(op, register(*dst)?, register(args[0])?, register(args[1])?)
        }
        Op::Builtin(dst, Intrinsic::Replace, args) if args.len() == 3 => {
            let list = new_list(routine, sites, args[1..].iter().map(|&arg| register(arg)))?;
            Instruction::TextReplace(register(*dst)?, register(args[0])?, list)
        }
        Op::Lookup(dst, Operation::Field(field), args)
            if args.len() == 2 && places.is_location(*dst) =>
        {
            Instruction::LocationField(
                register(*dst)?,
                register(args[0])?,
                lower_index(routine, *field)?,
            )
        }
        Op::Lookup(dst, Operation::LocationLoad, args) if args.len() == 1 => {
            Instruction::LocationLoad(register(*dst)?, register(args[0])?)
        }
        Op::Lookup(_, Operation::LocationStore, args) if args.len() == 2 => {
            Instruction::LocationStore(register(args[0])?, register(args[1])?)
        }
        Op::Yield(dst, value) => Instruction::Yield(register(*dst)?, register(*value)?),
        Op::Lookup(dst, Operation::GeneratorStart, args) if args.len() == 1 => {
            Instruction::GeneratorStart(register(*dst)?, generator(args[0])?)
        }
        Op::Lookup(dst, Operation::GeneratorNext, args) if args.len() == 1 => {
            Instruction::GeneratorNext(register(*dst)?, generator(args[0])?)
        }
        Op::Lookup(dst, Operation::GeneratorResume(ResumeInput::Unit), args) if args.len() == 1 => {
            Instruction::GeneratorResumeUnit(register(*dst)?, generator(args[0])?)
        }
        Op::Lookup(dst, Operation::GeneratorResume(ResumeInput::Value), args)
            if args.len() == 2 =>
        {
            Instruction::GeneratorResume(register(*dst)?, generator(args[0])?, register(args[1])?)
        }
        Op::Lookup(_, Operation::GeneratorClose, args) if args.len() == 1 => {
            Instruction::GeneratorClose(generator(args[0])?)
        }
        Op::Lookup(
            dst,
            operation @ (Operation::GeneratorIsYielded | Operation::GeneratorIsComplete),
            args,
        ) if args.len() == 1 => Instruction::EnumIs(
            register(*dst)?,
            register(args[0])?,
            u32::from(matches!(operation, Operation::GeneratorIsComplete)),
        ),
        Op::Lookup(
            dst,
            operation @ (Operation::GeneratorYielded | Operation::GeneratorCompleted),
            args,
        ) if args.len() == 1 => {
            let step = register(args[0])?;
            let variant = u32::from(matches!(operation, Operation::GeneratorCompleted));
            if context.payload_needs_drop(step, variant) {
                Instruction::EnumTake(register(*dst)?, step, variant)
            } else {
                Instruction::EnumPayload(register(*dst)?, step, variant)
            }
        }
        Op::Lookup(dst, Operation::LayoutForRc, args) if args.is_empty() => {
            Instruction::LayoutForRc(register(*dst)?)
        }
        Op::Lookup(dst, Operation::Allocate, args) if args.len() == 1 => {
            Instruction::Allocate(register(*dst)?, register(args[0])?)
        }
        Op::Lookup(_, Operation::Deallocate, args) if args.len() == 1 => {
            Instruction::Deallocate(register(args[0])?)
        }
        Op::Lookup(
            dst,
            Operation::RcNewIn {
                deallocate,
                borrowed: false,
                fallible: false,
            },
            args,
        ) if args.len() == 3 => {
            let CallTarget::Direct(function) = call_target(*deallocate)? else {
                return Err(unsupported());
            };
            let list = new_list(routine, sites, args.iter().map(|&arg| register(arg)))?;
            Instruction::RcNewIn(register(*dst)?, list, function)
        }
        Op::Lookup(dst, Operation::StoragePathNew, args) if args.len() == 1 => {
            Instruction::PathNew(register(*dst)?, register(args[0])?)
        }
        Op::Lookup(dst, Operation::Missing, args) if args.is_empty() => {
            Instruction::PathMissing(register(*dst)?)
        }
        Op::Lookup(dst, Operation::Target, args) if args.len() == 1 && places.is_path(args[0]) => {
            Instruction::PathTarget(register(*dst)?, register(args[0])?)
        }
        Op::Lookup(dst, Operation::Field(field), args)
            if args.len() == 2 && places.is_path(args[0]) =>
        {
            Instruction::PathField(
                register(*dst)?,
                register(args[0])?,
                lower_index(routine, *field)?,
            )
        }
        Op::Lookup(dst, Operation::Element, args) if args.len() == 2 && places.is_path(args[0]) => {
            Instruction::PathElement(register(*dst)?, register(args[0])?, register(args[1])?)
        }
        Op::Lookup(dst, Operation::Mapped, args) if args.len() == 3 => {
            let site = CallSiteId(lower_index(routine, sites.calls.len())?);
            sites.calls.push(CallSite {
                target: CallTarget::Mapped(register(args[2])?),
                arguments: Box::new([register(args[0])?, register(args[1])?]),
                destination: Some(register(*dst)?),
            });
            Instruction::Call(site)
        }
        Op::Lookup(dst, Operation::Parse(NumberKind::I64), args) if args.len() == 1 => {
            Instruction::ParseInteger(register(*dst)?, register(args[0])?)
        }
        Op::Lookup(
            dst,
            operation @ (Operation::IsOk
            | Operation::IsErr
            | Operation::UnwrapOk
            | Operation::UnwrapErr),
            args,
        ) if args.len() == 1 => {
            let result = register(args[0])?;
            let (ok, err) = context.result_variants(result).ok_or_else(unsupported)?;
            let variant = match operation {
                Operation::IsOk | Operation::UnwrapOk => ok,
                _ => err,
            };
            match operation {
                Operation::IsOk | Operation::IsErr => {
                    Instruction::EnumIs(register(*dst)?, result, variant)
                }
                _ if context.payload_needs_drop(result, variant) => {
                    Instruction::EnumTake(register(*dst)?, result, variant)
                }
                _ => Instruction::EnumPayload(register(*dst)?, result, variant),
            }
        }
        Op::Builtin(dst, Intrinsic::StringFrom, args) if args.len() == 1 => {
            Instruction::ToText(register(*dst)?, register(args[0])?)
        }
        Op::Builtin(dst, Intrinsic::Abs | Intrinsic::NumAbs, args)
            if args.len() == 1 && places.scalar(args[0]) == Some(ScalarType::I64) =>
        {
            Instruction::Abs(register(*dst)?, register(args[0])?)
        }
        Op::Builtin(
            dst,
            intrinsic @ (Intrinsic::Min | Intrinsic::Max | Intrinsic::NumMin | Intrinsic::NumMax),
            args,
        ) if args.len() == 2
            && args
                .iter()
                .all(|&arg| places.scalar(arg) == Some(ScalarType::I64)) =>
        {
            let op = if matches!(intrinsic, Intrinsic::Min | Intrinsic::NumMin) {
                Extremum::Min
            } else {
                Extremum::Max
            };
            Instruction::Extremum(op, register(*dst)?, register(args[0])?, register(args[1])?)
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
            let place = register(*addr)?;
            let owner = matches!(
                places.registers[place.index()],
                RegisterType::Loan { pointee, .. } if shapes.needs_drop(pointee)
            );
            if owner {
                Instruction::MoveOut(register(*dst)?, place)
            } else {
                Instruction::ReadLoan(register(*dst)?, place)
            }
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
            // Taking an owner empties its local.
            if context.needs_drop(place) {
                Instruction::Move(register(*dst)?, place)
            } else {
                Instruction::Copy(register(*dst)?, place)
            }
        }
        Op::Drop(addr) if places.address(*addr).is_some() => {
            Instruction::Drop(places.address(*addr).ok_or_else(unsupported)?)
        }
        Op::Lookup(dst, Operation::ManagedNew(MemoryKind::Rc, false), args) if args.len() == 1 => {
            Instruction::RcNew(register(*dst)?, register(args[0])?)
        }
        Op::Lookup(dst, Operation::RcClone, args) if args.len() == 1 => {
            Instruction::RcClone(register(*dst)?, register(args[0])?)
        }
        Op::Lookup(dst, Operation::RcDowngrade, args) if args.len() == 1 => {
            Instruction::RcDowngrade(register(*dst)?, register(args[0])?)
        }
        Op::Lookup(dst, Operation::RcUpgrade, args) if args.len() == 1 => {
            Instruction::RcUpgrade(register(*dst)?, register(args[0])?)
        }
        Op::Lookup(dst, Operation::RcStrongCount, args) if args.len() == 1 => {
            Instruction::RcStrongCount(register(*dst)?, register(args[0])?)
        }
        Op::Lookup(_, Operation::DropValue, args) if args.len() == 1 => {
            Instruction::Drop(register(args[0])?)
        }
        Op::Lookup(
            dst,
            operation @ (Operation::Unwrap | Operation::IsNone | Operation::IsSome),
            args,
        ) if args.len() == 1 => {
            let option = register(args[0])?;
            let (some, none) = context.option_variants(option).ok_or_else(unsupported)?;
            match operation {
                Operation::IsNone => Instruction::EnumIs(register(*dst)?, option, none),
                Operation::IsSome => Instruction::EnumIs(register(*dst)?, option, some),
                _ if context.payload_needs_drop(option, some) => {
                    Instruction::EnumTake(register(*dst)?, option, some)
                }
                _ => Instruction::EnumPayload(register(*dst)?, option, some),
            }
        }
        Op::Input(addr, ty) => {
            let kind = lower_input(ty).ok_or_else(unsupported)?;
            match places.address(*addr) {
                Some(place) => Instruction::Input(place, kind),
                None => Instruction::InputLoan(register(*addr)?, kind),
            }
        }
        Op::FrameInput(dst, ty) => {
            Instruction::Input(register(*dst)?, lower_input(ty).ok_or_else(unsupported)?)
        }
        Op::Load(dst, addr) => match places.address(*addr) {
            Some(place) => Instruction::Copy(register(*dst)?, place),
            None => Instruction::ReadLoan(register(*dst)?, register(*addr)?),
        },
        Op::Store(addr, src) => match places.address(*addr) {
            Some(place) if context.needs_drop(place) => Instruction::Move(place, register(*src)?),
            Some(place) => Instruction::Copy(place, register(*src)?),
            None => Instruction::WriteLoan(register(*addr)?, register(*src)?),
        },
        Op::Move(dst, src) | Op::FrameStore(dst, src)
            if places
                .registers
                .get(*dst)
                .is_some_and(|kind| context.register_needs_drop(*kind)) =>
        {
            Instruction::Move(register(*dst)?, register(*src)?)
        }
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
                (Intrinsic::CastI64, Some(ScalarType::U64)) => {
                    Instruction::CountToInteger(dst, source)
                }
                (Intrinsic::CastUsize | Intrinsic::CastU64, Some(ScalarType::I64)) => {
                    Instruction::IntegerToCount(dst, source)
                }
                (Intrinsic::CastUsize | Intrinsic::CastU64, Some(ScalarType::U64)) => {
                    Instruction::Copy(dst, source)
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
