//! Utility declaration contracts, keyed by semantic identity.
use crate::{Type, intrinsic::Intrinsic};
use Intrinsic::*;
pub fn pure(id: Intrinsic) -> bool {
    !matches!(
        id,
        Random | Print | Input | FreshSymbol | CellNew | ContextNew | ServiceKeyNew | MappedLookup
    )
}
pub fn standard(name: &str) -> bool {
    canonical(name).is_some_and(standard_id)
}
pub fn standard_id(id: Intrinsic) -> bool {
    matches!(
        id,
        Right | Length | Mid | LowerChar | UpperChar | Truncate | Random
    )
}
/// Compatibility declarations are separate from the modern standard library.
pub fn canonical(name: &str) -> Option<Intrinsic> {
    const DECLARATIONS: &[(&str, Intrinsic)] = &[
        ("LEFT", Left),
        ("RIGHT", Right),
        ("MID", Mid),
        ("LENGTH", Length),
        ("LCASE", LowerChar),
        ("UCASE", UpperChar),
        ("INT", Truncate),
        ("RAND", Random),
        ("ASC", CharCode),
        ("CHR", CharFromCode),
        ("NUM_TO_STR", StringFrom),
        ("STR_TO_NUM", ParseLegacy),
        ("IS_NUM", IsNumeric),
        ("TRIM", Trim),
        ("UPPER", Upper),
        ("LOWER", Lower),
        ("CONTAINS", Contains),
        ("STARTS_WITH", StartsWith),
        ("ENDS_WITH", EndsWith),
        ("REPLACE", Replace),
        ("ABS", Abs),
        ("MIN", Min),
        ("MAX", Max),
        ("CLAMP", Clamp),
        ("ROUND", Round),
        ("FLOOR", Floor),
        ("CEIL", Ceil),
        ("SQRT", Sqrt),
        ("POW", Pow),
        ("KEYS", Keys),
        ("VALUES", Values),
        ("COUNT", Count),
        ("SUM", Sum),
    ];
    DECLARATIONS
        .iter()
        .find(|(declaration, _)| *declaration == name)
        .map(|(_, id)| *id)
}
pub fn result_type(n: Intrinsic, ts: &[Type]) -> Option<Type> {
    use Type::*;
    if let Some(kind) = n.cast_kind() {
        return (ts.len() == 1 && ts[0].number_kind().is_some()).then(|| Type::numeric(kind));
    }
    if let Some(first) = ts.first()
        && matches!(first, Type::Numeric(_))
        && ts.iter().all(|t| t == first)
    {
        let kind = first.number_kind()?;
        return match (n, ts.len()) {
            (Abs, 1) | (Min | Max, 2) | (Clamp, 3) => Some(first.clone()),
            (Round | Floor | Ceil | Sqrt, 1) | (Pow, 2) => Some(if kind.float() {
                first.clone()
            } else {
                Type::Real
            }),
            (StringFrom, 1) => Some(Type::String),
            _ => None,
        };
    }
    Some(match (n, ts) {
        (
            ArraySum,
            [
                Array(_, element)
                | Storage(cyallo_ast::StorageKind::Vec | cyallo_ast::StorageKind::VecDeque, element),
            ],
        ) if element.number_kind().is_some() => *element.clone(),
        (Left | Right, [String, Integer]) | (Mid, [String, Integer, Integer]) => String,
        (StringLength, [String]) => Integer,
        (
            ArrayLength,
            [
                Array(..)
                | Storage(cyallo_ast::StorageKind::Vec | cyallo_ast::StorageKind::VecDeque, _),
            ],
        ) => Integer,
        (ArrayLength, [Reference { pointee, .. }])
            if matches!(
                pointee.as_ref(),
                Slice(_)
                    | Array(..)
                    | Storage(
                        cyallo_ast::StorageKind::Vec | cyallo_ast::StorageKind::VecDeque,
                        _
                    )
            ) =>
        {
            Integer
        }
        (Length, [String | SparseArray(_) | Array(..)]) => Integer,
        (Length, [Reference { pointee, .. }])
            if matches!(pointee.as_ref(), Slice(_) | Array(..)) =>
        {
            Integer
        }
        (LowerChar | UpperChar, [Char]) => Char,
        (Truncate, [Real | Integer]) => Integer,
        (Random, [Integer]) => Real,
        (CharCode, [Char]) => Integer,
        (CharFromCode, [Integer]) => Char,
        (StringFrom, [Integer | Real | Boolean | Char | String]) => String,
        (ParseLegacy, [String]) => Real,
        (IsNumeric, [String]) => Boolean,
        (Trim | Upper | Lower, [String]) | (Replace, [String, String, String]) => String,
        (Contains | StartsWith | EndsWith, [String, String]) => Boolean,
        (Abs, [Integer]) => Integer,
        (Abs | Round | Floor | Ceil | Sqrt, [Integer | Real]) => Real,
        (Pow, [Integer | Real, Integer | Real]) => Real,
        (Min | Max, [Integer, Integer]) | (Clamp, [Integer, Integer, Integer]) => Integer,
        (Min | Max, [Integer | Real, Integer | Real])
        | (Clamp, [Integer | Real, Integer | Real, Integer | Real]) => Real,
        (Keys, [SparseArray(_)]) => SparseArray(Box::new(Integer)),
        (Values, [SparseArray(element)]) => SparseArray(element.clone()),
        (Count, [SparseArray(_)]) => Integer,
        (Sum, [SparseArray(element)]) if matches!(**element, Integer | Real) => (**element).clone(),
        _ => return None,
    })
}
