//! Source-syntax rendering of types and resolved names, for diagnostics.
use crate::{Locality, MemoryKind, NumberKind, ParameterPassing, RoutineKind, StorageKind, Type};
use std::fmt::{self, Display, Formatter};

impl Display for MemoryKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Gc => "gc",
            Self::Rc => "rc",
        })
    }
}

impl Display for RoutineKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Function => "fn",
            Self::Procedure => "proc",
        })
    }
}

impl Display for ParameterPassing {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Value => "by-value",
            Self::Read => "read",
        })
    }
}

/// The name a programmer wrote for a resolved declaration. Name resolution
/// gives declarations internal identities (`$module0$Value$total`,
/// `$binding3$x`, `$generic$map<…>`); diagnostics show the source spelling.
pub fn source_name(name: &str) -> &str {
    let Some(rest) = name.strip_prefix('$') else {
        return name;
    };
    let mut parts = rest.split('$');
    let head = parts.next().unwrap_or_default();
    let spelled = if head.starts_with("module") {
        // `$module{N}${Kind}${name}[$shadow{V}]`
        parts.nth(1)
    } else if head.starts_with("binding") || matches!(head, "type" | "property" | "generic") {
        parts.next()
    } else {
        None
    };
    match spelled {
        Some(spelled) => spelled.split('<').next().unwrap_or(spelled),
        None => name,
    }
}

impl Display for NumberKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::I8 => "i8",
            Self::I16 => "i16",
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::Isize => "isize",
            Self::Usize => "usize",
            Self::F32 => "f32",
            Self::F64 => "f64",
        })
    }
}

/// Writes `items` separated by `, `.
fn list(f: &mut Formatter<'_>, items: &[Type]) -> fmt::Result {
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            f.write_str(", ")?;
        }
        item.fmt(f)?;
    }
    Ok(())
}

impl Display for Type {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::ValueParameter(index) => write!(f, "value parameter {index}"),
            Self::NativeCallable(_) => f.write_str("native callable"),
            Self::Dependent(id, arguments) => {
                write!(f, "type family {}<", id.0)?;
                list(f, arguments)?;
                f.write_str(">")
            }
            Self::Lifetime(name) => write!(f, "'{name}"),
            Self::SelfType => f.write_str("Self"),
            Self::Numeric(kind) => kind.fmt(f),
            Self::Integer => f.write_str("i64"),
            Self::Real => f.write_str("f64"),
            Self::Boolean => f.write_str("bool"),
            Self::Char => f.write_str("char"),
            Self::String => f.write_str("string"),
            Self::Array(bounds, element) => match bounds.as_slice() {
                [(0, upper)] => write!(f, "[{}]{element}", upper + 1),
                bounds => {
                    f.write_str("[")?;
                    for (index, (lower, upper)) in bounds.iter().enumerate() {
                        if index > 0 {
                            f.write_str(", ")?;
                        }
                        write!(f, "{lower}..={upper}")?;
                    }
                    write!(f, "]{element}")
                }
            },
            Self::SparseArray(element) => write!(f, "sparse array of {element}"),
            Self::Named(name) | Self::RecordRef(name) | Self::Record(name, _) => {
                f.write_str(source_name(name))
            }
            Self::Applied(name, arguments) => {
                write!(f, "{}<", source_name(name))?;
                list(f, arguments)?;
                f.write_str(">")
            }
            Self::Dynamic => f.write_str("dynamic"),
            Self::Object(fields) => {
                f.write_str("object { ")?;
                for (name, ty) in fields {
                    write!(f, "{name}: {ty}, ")?;
                }
                f.write_str(".. }")
            }
            Self::Tuple(items) => {
                f.write_str("(")?;
                list(f, items)?;
                if items.len() == 1 {
                    f.write_str(",")?;
                }
                f.write_str(")")
            }
            Self::Function(kind, params, result) => {
                f.write_str(match kind {
                    RoutineKind::Function => "fn(",
                    RoutineKind::Procedure => "proc(",
                })?;
                for (index, (param, by_ref)) in params.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    if *by_ref {
                        f.write_str("ref ")?;
                    }
                    param.fmt(f)?;
                }
                f.write_str(")")?;
                if **result != Self::Void {
                    write!(f, " -> {result}")?;
                }
                Ok(())
            }
            Self::Borrowed(pointee) => write!(f, "&{pointee}"),
            Self::Reference {
                mutable,
                lifetime,
                pointee,
            } => {
                f.write_str("&")?;
                if let Some(lifetime) = lifetime {
                    write!(f, "'{lifetime} ")?;
                }
                if *mutable {
                    f.write_str("mut ")?;
                }
                pointee.fmt(f)
            }
            Self::Managed {
                kind,
                locality,
                lifetime,
                pointee,
            } => {
                if *locality == Locality::Shared {
                    f.write_str("@shared ")?;
                }
                kind.fmt(f)?;
                if let Some(lifetime) = lifetime {
                    write!(f, "<'{lifetime}>")?;
                }
                write!(f, " {pointee}")
            }
            Self::Slice(element) => write!(f, "[]{element}"),
            Self::Context => f.write_str("std::service::Context"),
            Self::Registration => f.write_str("std::service::Registration"),
            Self::ServiceKey(service) => write!(f, "ServiceKey<{service}>"),
            Self::ServiceRef(service) => write!(f, "ServiceRef<{service}>"),
            Self::TraitObject(name) => write!(f, "dyn {}", source_name(name)),
            Self::Storage(kind, payload) => {
                let name = match kind {
                    StorageKind::Cell => "Cell",
                    StorageKind::Location => "Location",
                    StorageKind::StoragePath => "StoragePath",
                    StorageKind::Option => "Option",
                    StorageKind::Result => "Result",
                    StorageKind::Vec => "Vec",
                    StorageKind::VecDeque => "VecDeque",
                    StorageKind::Range => "Range",
                    StorageKind::ParseError => return f.write_str("ParseError"),
                    StorageKind::Weak => "Weak",
                    StorageKind::ArenaCell => "ArenaCell",
                    StorageKind::Layout => return f.write_str("Layout"),
                    StorageKind::Allocation => return f.write_str("Allocation"),
                    StorageKind::AllocError => return f.write_str("AllocError"),
                    StorageKind::Generator => "Generator",
                    StorageKind::PureGenerator => "Generator",
                    StorageKind::GeneratorStep => "GeneratorStep",
                    StorageKind::Sender => "Sender",
                    StorageKind::Receiver => "Receiver",
                    StorageKind::SendError => "SendError",
                    StorageKind::ReceiveError => return f.write_str("ReceiveError"),
                    StorageKind::Clock => return f.write_str("Clock"),
                    StorageKind::Task => "Task",
                    StorageKind::TaskOutcome => "TaskOutcome",
                    StorageKind::TypeDescriptor => return f.write_str("TypeDescriptor"),
                    StorageKind::ViewError => return f.write_str("ViewError"),
                    StorageKind::LoadError => return f.write_str("LoadError"),
                };
                write!(f, "{name}<")?;
                // Multi-parameter constructors store their arguments as a tuple.
                match (kind, payload.as_ref()) {
                    (
                        StorageKind::Result
                        | StorageKind::Generator
                        | StorageKind::PureGenerator
                        | StorageKind::GeneratorStep,
                        Self::Tuple(arguments),
                    ) => list(f, arguments)?,
                    (_, payload) => payload.fmt(f)?,
                }
                f.write_str(">")
            }
            Self::Symbol => f.write_str("Symbol"),
            Self::Any => f.write_str("Any"),
            Self::Void => f.write_str("()"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_identities_render_as_source_names() {
        assert_eq!(source_name("$module0$Value$total"), "total");
        assert_eq!(source_name("$module2$Value$x$shadow3"), "x");
        assert_eq!(source_name("$binding7$count"), "count");
        assert_eq!(source_name("$generic$map<[Integer]>"), "map");
        assert_eq!(source_name("plain"), "plain");
    }

    #[test]
    fn types_render_in_source_syntax() {
        let point = Type::Record("$module0$Type$Point".into(), vec![]);
        let cases = [
            (Type::Tuple(vec![Type::Integer, Type::Real]), "(i64, f64)"),
            (Type::Numeric(NumberKind::U8), "u8"),
            (Type::Array(vec![(0, 3)], Box::new(Type::Char)), "[4]char"),
            (
                Type::Reference {
                    mutable: true,
                    lifetime: None,
                    pointee: Box::new(Type::Slice(Box::new(point.clone()))),
                },
                "&mut []Point",
            ),
            (
                Type::Function(
                    RoutineKind::Function,
                    vec![(Type::Integer, false)],
                    Box::new(Type::Real),
                ),
                "fn(i64) -> f64",
            ),
            (
                Type::Storage(
                    StorageKind::Result,
                    Box::new(Type::Tuple(vec![Type::Integer, Type::String])),
                ),
                "Result<i64, string>",
            ),
            (
                Type::Managed {
                    kind: MemoryKind::Gc,
                    locality: Locality::Local,
                    lifetime: None,
                    pointee: Box::new(point),
                },
                "gc Point",
            ),
        ];
        for (ty, text) in cases {
            assert_eq!(ty.to_string(), text);
        }
    }
}
