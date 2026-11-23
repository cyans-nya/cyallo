use super::{Scalar, Variable, mir};
use cyallo_ast::{Diagnostic, Operator, Param, Span, Type};
use mir::{Exit, Located, Value};
use std::fmt;

pub(super) struct Body {
    pub(super) blocks: Vec<Block>,
    pub(super) types: Vec<Type>,
    pub(super) operands: Vec<Value>,
    /// Final types of the locals after the parameters, in slot order.
    pub(super) locals: Vec<Type>,
}

/// A callee contract bound to one reference site: the target's routine
/// identity and its signature. The callee's body is never consulted.
pub(super) struct Callee {
    pub(super) id: usize,
    pub(super) parameters: Vec<Type>,
    pub(super) result: Type,
}

pub(super) struct Block {
    pub(super) operations: Vec<Located<Operation>>,
    pub(super) exit: Located<Exit>,
}

pub(super) enum Operation {
    Statement,
    Constant(Value, Scalar),
    ReadVariable(Value, Variable),
    /// Store into a declared local (its first store is its definition).
    Store(Variable, Value),
    Binary(Value, Operator, Value, Value),
    Unary(Value, Operator, Value),
    Call(Value, usize, mir::Operands),
}

#[derive(Debug)]
enum CheckError {
    TypeMismatch { expected: Type, actual: Type },
    MissingReturn,
    InvalidMir,
}

impl fmt::Display for CheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TypeMismatch { expected, actual } => {
                write!(
                    f,
                    "mismatched types: expected `{expected}`, found `{actual}`"
                )
            }
            Self::MissingReturn => {
                f.write_str("non-void routine must return a value on every path")
            }
            Self::InvalidMir => f.write_str("invalid scalar routine MIR"),
        }
    }
}

impl std::error::Error for CheckError {}

fn routine_diagnostic(error: CheckError, span: Span) -> Diagnostic {
    let code = match &error {
        CheckError::TypeMismatch { .. } => "E109",
        CheckError::MissingReturn => "E103",
        CheckError::InvalidMir => "E100",
    };
    Diagnostic::new(code, error.to_string(), span)
}

/// Check every source operation in source order and propagate reachability
/// separately to decide whether control can fall off the end of the routine.
pub(super) fn analyze(
    body: &mir::Body,
    parameters: &[Param],
    result: &Type,
    callees: &[Callee],
    routine_span: Span,
) -> Result<Body, Diagnostic> {
    let mut types = Vec::with_capacity(body.value_count);
    let mut local_types: Vec<Option<Type>> = vec![None; body.locals.len()];
    let mut reachable = vec![false; body.blocks.len()];
    *reachable
        .first_mut()
        .ok_or_else(|| routine_diagnostic(CheckError::InvalidMir, routine_span))? = true;
    let mut blocks = Vec::with_capacity(body.blocks.len());
    for (index, block) in body.blocks.iter().enumerate() {
        let block_reachable = reachable[index];
        let mut operations = Vec::with_capacity(block.operations.len());
        let local_start = types.len();
        for &Located { kind, span } in &block.operations {
            let checked = match kind {
                mir::Operation::Statement => Operation::Statement,
                mir::Operation::Constant(value, scalar) => {
                    let ty = match scalar {
                        Scalar::Integer(_) => Type::Integer,
                        Scalar::Boolean(_) => Type::Boolean,
                    };
                    define(&mut types, value, ty, span)?;
                    Operation::Constant(value, scalar)
                }
                mir::Operation::Read(value, variable) => {
                    let ty = match variable.0.checked_sub(parameters.len()) {
                        None => parameters[variable.0].ty.clone(),
                        Some(local) => local_types
                            .get(local)
                            .cloned()
                            .flatten()
                            .ok_or_else(|| routine_diagnostic(CheckError::InvalidMir, span))?,
                    };
                    define(&mut types, value, ty, span)?;
                    Operation::ReadVariable(value, variable)
                }
                mir::Operation::Unary(value, operator, operand_value) => {
                    let ty = operand(&types, local_start, operand_value, span)?.clone();
                    let valid = if operator == Operator::Not {
                        ty == Type::Boolean
                    } else {
                        ty.number_kind().is_some()
                    };
                    if !valid {
                        return Err(Diagnostic::new("E105", "invalid unary operand", span));
                    }
                    define(&mut types, value, ty, span)?;
                    Operation::Unary(value, operator, operand_value)
                }
                mir::Operation::Condition(value) => {
                    // `&&` and `||` take booleans. The existing route never checked this
                    // (it moved the operand into a boolean register); this is its
                    // intended `mismatched types` error.
                    require(
                        &Type::Boolean,
                        operand(&types, local_start, value, span)?,
                        span,
                    )?;
                    continue;
                }
                mir::Operation::Define(variable, value, value_span, annotation) => {
                    let actual = operand(&types, local_start, value, span)?.clone();
                    let slot = local_slot(&mut local_types, parameters.len(), variable, span)?;
                    if slot.is_some() {
                        return Err(routine_diagnostic(CheckError::InvalidMir, span));
                    }
                    let ty = match annotation {
                        Some(annotation) => {
                            let expected = annotation.ty();
                            require(&expected, &actual, value_span)?;
                            expected
                        }
                        None => actual,
                    };
                    *slot = Some(ty);
                    Operation::Store(variable, value)
                }
                mir::Operation::Assign(variable, value, value_span) => {
                    let actual = operand(&types, local_start, value, span)?.clone();
                    let slot = local_slot(&mut local_types, parameters.len(), variable, span)?;
                    let expected = slot
                        .as_ref()
                        .ok_or_else(|| routine_diagnostic(CheckError::InvalidMir, span))?;
                    require(expected, &actual, value_span)?;
                    Operation::Store(variable, value)
                }
                mir::Operation::RangeBounds(start, end, iterable_span, end_span) => {
                    let start_type = operand(&types, local_start, start, span)?;
                    let end_type = operand(&types, local_start, end, span)?;
                    require(start_type, end_type, end_span)?;
                    if *start_type != Type::Integer {
                        return Err(Diagnostic::new(
                            "E109",
                            "for-in ranges require integer bounds",
                            iterable_span,
                        ));
                    }
                    // A pure check: nothing is emitted for it.
                    continue;
                }
                mir::Operation::Binary(value, operator, left, right) => {
                    let left_type = operand(&types, local_start, left, span)?;
                    let right_type = operand(&types, local_start, right, span)?;
                    let ty = super::super::binary_type(operator, left_type, right_type, span)?;
                    let operator = super::super::specialized_operator(operator, &ty);
                    define(&mut types, value, ty, span)?;
                    Operation::Binary(value, operator, left, right)
                }
                mir::Operation::Call(value, site, range) => {
                    let callee = callees
                        .get(site.0)
                        .ok_or_else(|| routine_diagnostic(CheckError::InvalidMir, span))?;
                    let arguments = body
                        .operands
                        .get(range.start..range.start + range.len)
                        .filter(|arguments| arguments.len() == callee.parameters.len())
                        .ok_or_else(|| routine_diagnostic(CheckError::InvalidMir, span))?;
                    for (&argument, expected) in arguments.iter().zip(&callee.parameters) {
                        require(
                            expected,
                            operand(&types, local_start, argument, span)?,
                            span,
                        )?;
                    }
                    define(&mut types, value, callee.result.clone(), span)?;
                    Operation::Call(value, callee.id, range)
                }
            };
            operations.push(Located {
                kind: checked,
                span,
            });
        }
        let exit = block.exit;
        let mut edge = |target: mir::BlockId, back: bool| {
            if target.0 <= index {
                // A loop's back edge returns to a block already reached.
                return if back {
                    Ok(())
                } else {
                    Err(routine_diagnostic(CheckError::InvalidMir, exit.span))
                };
            }
            let target = reachable
                .get_mut(target.0)
                .ok_or_else(|| routine_diagnostic(CheckError::InvalidMir, exit.span))?;
            *target |= block_reachable;
            Ok(())
        };
        match exit.kind {
            Exit::Branch {
                condition,
                condition_span,
                yes,
                no,
            } => {
                require(
                    &Type::Boolean,
                    operand(&types, local_start, condition, condition_span)?,
                    condition_span,
                )?;
                edge(yes, false)?;
                edge(no, false)?;
            }
            Exit::Jump(target) => edge(target, true)?,
            Exit::Next(target) => {
                if target.0 != index + 1 {
                    return Err(routine_diagnostic(CheckError::InvalidMir, exit.span));
                }
                edge(target, false)?;
            }
            Exit::Return { value, value_span } => {
                require(
                    result,
                    operand(&types, local_start, value, value_span)?,
                    value_span,
                )?;
            }
            Exit::End if block_reachable => {
                return Err(routine_diagnostic(CheckError::MissingReturn, routine_span));
            }
            Exit::End => {}
        }
        blocks.push(Block { operations, exit });
    }
    let locals = local_types
        .into_iter()
        .map(|ty| ty.ok_or_else(|| routine_diagnostic(CheckError::InvalidMir, routine_span)))
        .collect::<Result<_, _>>()?;
    Ok(Body {
        blocks,
        types,
        operands: body.operands.clone(),
        locals,
    })
}

fn local_slot<'a>(
    locals: &'a mut [Option<Type>],
    parameters: usize,
    variable: Variable,
    span: Span,
) -> Result<&'a mut Option<Type>, Diagnostic> {
    variable
        .0
        .checked_sub(parameters)
        .and_then(|local| locals.get_mut(local))
        .ok_or_else(|| routine_diagnostic(CheckError::InvalidMir, span))
}

fn define(types: &mut Vec<Type>, value: Value, ty: Type, span: Span) -> Result<(), Diagnostic> {
    if value.0 != types.len() {
        return Err(routine_diagnostic(CheckError::InvalidMir, span));
    }
    types.push(ty);
    Ok(())
}

fn operand(
    types: &[Type],
    local_start: usize,
    value: Value,
    span: Span,
) -> Result<&Type, Diagnostic> {
    value
        .0
        .checked_sub(local_start)
        .and_then(|index| types[local_start..].get(index))
        .ok_or_else(|| routine_diagnostic(CheckError::InvalidMir, span))
}

fn require(expected: &Type, actual: &Type, span: Span) -> Result<(), Diagnostic> {
    if expected == actual {
        return Ok(());
    }
    Err(routine_diagnostic(
        CheckError::TypeMismatch {
            expected: expected.clone(),
            actual: actual.clone(),
        },
        span,
    ))
}
