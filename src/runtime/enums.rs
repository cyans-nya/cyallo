//! Native enum encodings at the VM boundary. Source declarations own the schema.
use super::*;
use cyallo_ast::{EnumEncoding, EnumVariant};
pub(super) fn enum_value(variant: EnumVariant, payload: Option<Box<Value>>) -> Value {
    Value::Enum(variant.declaration, payload)
}
/// The role of a `core` enum value, recognised by its reserved variant id.
pub(crate) fn native_enum_parts(value: &Value) -> Option<(EnumEncoding, Option<&Value>)> {
    use EnumEncoding::*;
    let Value::Enum(variant, payload) = value else {
        return None;
    };
    let encoding = [
        OptionSome,
        OptionNone,
        ResultOk,
        ResultErr,
        StepYielded,
        StepComplete,
        SendClosed,
        SendFull,
        SendLimit,
        ReceiveClosed,
        ReceiveEmpty,
    ]
    .into_iter()
    .find(|encoding| encoding.reserved() == Some(*variant))?;
    Some((encoding, payload.as_deref()))
}
pub(super) fn enum_test(value: &Value, variant: EnumVariant) -> RunResult<bool> {
    match value {
        Value::Enum(actual, _) | Value::EnumStorage(actual, _) => {
            Ok(*actual == variant.declaration)
        }
        Value::NativeEnumStorage(actual, _) => {
            // Native snapshots already normalize to their validated ABI kind.
            // Published module products may carry relocated declaration IDs.
            if cyallo_ir::enums::encoding_storage(actual.encoding)
                != cyallo_ir::enums::encoding_storage(variant.encoding)
            {
                return Err(Invariant::EnumShapeMismatch.into());
            }
            Ok(actual.encoding == variant.encoding)
        }
        value => {
            let (actual, _) = native_enum_parts(value).ok_or(Invariant::EnumShapeMismatch)?;
            if cyallo_ir::enums::encoding_storage(actual)
                != cyallo_ir::enums::encoding_storage(variant.encoding)
            {
                return Err(Invariant::EnumShapeMismatch.into());
            }
            Ok(actual == variant.encoding)
        }
    }
}
pub(super) fn enum_payload(value: Value) -> RunResult<Value> {
    match value {
        Value::Enum(_, Some(value)) => Ok(*value),
        _ => Err(Invariant::EnumShapeMismatch.into()),
    }
}
