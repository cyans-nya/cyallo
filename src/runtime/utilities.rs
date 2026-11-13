use super::*;
use cyallo_ast::Operator;
fn dense(values: impl IntoIterator<Item = Value>) -> Value {
    let entries: BTreeMap<_, _> = values
        .into_iter()
        .enumerate()
        .map(|(i, v)| (i as u32, v))
        .collect();
    let length = entries.len() as u64;
    Value::SparseSnapshot(entries, length)
}
pub(super) fn call(
    n: cyallo_ir::intrinsic::Intrinsic,
    args: Vec<Value>,
    _limit: usize,
) -> RunResult<Value> {
    use cyallo_ast::{Number, NumberKind};
    if matches!(n, Intrinsic::Sum | Intrinsic::ArraySum) {
        let values: Vec<_> = match &args[0] {
            Value::ArraySnapshot(values) => values.iter().collect(),
            Value::SparseSnapshot(values, _) => values.values().collect(),
            _ => return Err("sum requires numeric array elements".into()),
        };
        let kind = values
            .first()
            .and_then(|value| value.as_number())
            .map_or(NumberKind::I64, |number| number.kind);
        let mut total = Value::from_number(Number::integer(kind, 0).map_err(|e| e.to_string())?);
        for value in values {
            total = binary(Operator::Add, total, value.clone())?;
        }
        return Ok(total);
    }
    use cyallo_ir::intrinsic::Intrinsic;
    if let Some(Value::Number(first)) = args.first() {
        let first = *first;
        let result = match n {
            Intrinsic::Abs => Some(if first.kind.float() {
                Number::floating(first.kind, first.real().abs()).map_err(|e| e.to_string())?
            } else {
                Number::integer(first.kind, first.int().abs()).map_err(|e| e.to_string())?
            }),
            Intrinsic::Min | Intrinsic::Max | Intrinsic::Clamp => {
                let second = args[1].as_number().ok_or("expected number")?;
                let choose = |a: Number, b: Number, smaller: bool| -> RunResult<Number> {
                    let less = a.compare(b, Operator::Less).map_err(|e| e.to_string())?;
                    Ok(if less == smaller { a } else { b })
                };
                Some(if n == Intrinsic::Min {
                    choose(first, second, true)?
                } else if n == Intrinsic::Max {
                    choose(first, second, false)?
                } else {
                    let third = args[2].as_number().ok_or("expected number")?;
                    if second
                        .compare(third, Operator::Greater)
                        .map_err(|e| e.to_string())?
                    {
                        return Err("clamp lower bound exceeds upper bound".into());
                    }
                    choose(choose(first, second, false)?, third, true)?
                })
            }
            Intrinsic::Round
            | Intrinsic::Floor
            | Intrinsic::Ceil
            | Intrinsic::Sqrt
            | Intrinsic::Pow => {
                let value = if first.kind == NumberKind::F32 {
                    let value = first.real() as f32;
                    (match n {
                        Intrinsic::Round => value.round(),
                        Intrinsic::Floor => value.floor(),
                        Intrinsic::Ceil => value.ceil(),
                        Intrinsic::Sqrt => value.sqrt(),
                        _ => value.powf(real(args[1].clone())? as f32),
                    }) as f64
                } else {
                    let value = first.real();
                    match n {
                        Intrinsic::Round => value.round(),
                        Intrinsic::Floor => value.floor(),
                        Intrinsic::Ceil => value.ceil(),
                        Intrinsic::Sqrt => value.sqrt(),
                        _ => value.powf(real(args[1].clone())?),
                    }
                };
                Some(
                    Number::floating(
                        if first.kind.float() {
                            first.kind
                        } else {
                            NumberKind::F64
                        },
                        value,
                    )
                    .map_err(|e| e.to_string())?,
                )
            }
            _ => None,
        };
        if let Some(result) = result {
            return Ok(Value::from_number(result));
        }
    }
    Ok(match n {
        cyallo_ir::intrinsic::Intrinsic::Trim => Value::String(text(&args[0])?.trim().into()),
        cyallo_ir::intrinsic::Intrinsic::Upper => {
            Value::String(text(&args[0])?.to_uppercase().into())
        }
        cyallo_ir::intrinsic::Intrinsic::Lower => {
            Value::String(text(&args[0])?.to_lowercase().into())
        }
        cyallo_ir::intrinsic::Intrinsic::Contains => {
            Value::Boolean(text(&args[0])?.contains(text(&args[1])?))
        }
        cyallo_ir::intrinsic::Intrinsic::StartsWith => {
            Value::Boolean(text(&args[0])?.starts_with(text(&args[1])?))
        }
        cyallo_ir::intrinsic::Intrinsic::EndsWith => {
            Value::Boolean(text(&args[0])?.ends_with(text(&args[1])?))
        }
        cyallo_ir::intrinsic::Intrinsic::Replace => Value::String(
            text(&args[0])?
                .replace(text(&args[1])?, text(&args[2])?)
                .into(),
        ),
        cyallo_ir::intrinsic::Intrinsic::Abs => match &args[0] {
            Value::Integer(n) => Value::Integer(n.checked_abs().ok_or("INTEGER overflow")?),
            v => Value::Real(real(v.clone())?.abs()),
        },
        cyallo_ir::intrinsic::Intrinsic::Min
        | cyallo_ir::intrinsic::Intrinsic::Max
        | cyallo_ir::intrinsic::Intrinsic::Clamp => {
            if args.iter().all(|v| matches!(v, Value::Integer(_)))
                && let [Value::Integer(x), Value::Integer(y), rest @ ..] = args.as_slice()
            {
                let value = if n == cyallo_ir::intrinsic::Intrinsic::Min {
                    (*x).min(*y)
                } else if n == cyallo_ir::intrinsic::Intrinsic::Max {
                    (*x).max(*y)
                } else {
                    let Value::Integer(z) = rest[0] else {
                        unreachable!()
                    };
                    if *y > z {
                        return Err("clamp lower bound exceeds upper bound".into());
                    }
                    (*x).clamp(*y, z)
                };
                Value::Integer(value)
            } else {
                let x = real(args[0].clone())?;
                let y = real(args[1].clone())?;
                let value = if n == cyallo_ir::intrinsic::Intrinsic::Min {
                    x.min(y)
                } else if n == cyallo_ir::intrinsic::Intrinsic::Max {
                    x.max(y)
                } else {
                    let z = real(args[2].clone())?;
                    if y.is_nan() || z.is_nan() || y > z {
                        return Err("invalid clamp bounds".into());
                    }
                    x.clamp(y, z)
                };
                Value::Real(value)
            }
        }
        cyallo_ir::intrinsic::Intrinsic::Round
        | cyallo_ir::intrinsic::Intrinsic::Floor
        | cyallo_ir::intrinsic::Intrinsic::Ceil
        | cyallo_ir::intrinsic::Intrinsic::Sqrt
        | cyallo_ir::intrinsic::Intrinsic::Pow => {
            let x = real(args[0].clone())?;
            let value = match n {
                cyallo_ir::intrinsic::Intrinsic::Round => x.round(),
                cyallo_ir::intrinsic::Intrinsic::Floor => x.floor(),
                cyallo_ir::intrinsic::Intrinsic::Ceil => x.ceil(),
                cyallo_ir::intrinsic::Intrinsic::Sqrt => x.sqrt(),
                _ => x.powf(real(args[1].clone())?),
            };
            if !value.is_finite() {
                return Err(format!("{n} result is outside its finite numeric domain"));
            }
            Value::Real(value)
        }
        cyallo_ir::intrinsic::Intrinsic::Keys
        | cyallo_ir::intrinsic::Intrinsic::Values
        | cyallo_ir::intrinsic::Intrinsic::Count
        | cyallo_ir::intrinsic::Intrinsic::Sum => {
            let Value::SparseSnapshot(entries, _) = &args[0] else {
                return Err("expected sparse array".into());
            };
            match n {
                cyallo_ir::intrinsic::Intrinsic::Keys => {
                    dense(entries.keys().map(|i| Value::Integer(*i as i64)))
                }
                cyallo_ir::intrinsic::Intrinsic::Values => dense(entries.values().cloned()),
                cyallo_ir::intrinsic::Intrinsic::Count => Value::Integer(entries.len() as i64),
                _ => {
                    let mut total = Value::Integer(0);
                    for value in entries.values() {
                        if *value == Value::Uninitialized {
                            return Err("read of uninitialized array element".into());
                        }
                        total = binary(Operator::Add, total, value.clone())?;
                    }
                    total
                }
            }
        }
        _ => return Err(format!("unsupported builtin {n}")),
    })
}
