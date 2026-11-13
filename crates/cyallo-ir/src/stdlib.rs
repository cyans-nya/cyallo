//! Standard symbols resolved before lowering; source spellings are never opcodes.
use crate::intrinsic::Intrinsic;
use cyallo_ast::StorageKind;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinType {
    Storage(StorageKind),
    Any,
    Symbol,
    Context,
    Registration,
    ServiceKey,
    ServiceRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Module(&'static str),
    Type(BuiltinType),
    Trait,
    Function(Intrinsic),
}
pub const PRELUDE: &[(&str, &str)] = &[
    ("array", "std::array"),
    ("collections", "std::collections"),
    ("num", "std::num"),
    ("gc", "std::gc"),
    ("rc", "std::rc"),
    ("io", "std::io"),
    ("cell", "std::cell"),
    ("lookup", "std::lookup"),
    ("string", "std::string"),
    ("mem", "std::mem"),
    ("Cell", "std::cell::Cell"),
    ("Vec", "std::collections::Vec"),
    ("VecDeque", "std::collections::VecDeque"),
    ("Option", "std::option::Option"),
    ("Result", "std::result::Result"),
    ("Generator", "std::generator::Generator"),
];
const MODULES: &[&str] = &[
    "std",
    "std::prelude",
    "std::array",
    "std::collections",
    "std::num",
    "std::gc",
    "std::rc",
    "std::io",
    "std::cell",
    "std::lookup",
    "std::string",
    "std::char",
    "std::random",
    "std::mem",
    "std::mem::manual",
    "std::option",
    "std::result",
    "std::generator",
    "std::any",
    "std::service",
    "std::runtime",
    "std::runtime::caie",
];
const TRAITS: &[&str] = &["std::mem::Allocator"];
const TYPES: &[(&str, BuiltinType)] = &[
    (
        "std::generator::Generator",
        BuiltinType::Storage(StorageKind::Generator),
    ),
    (
        "std::mem::Layout",
        BuiltinType::Storage(StorageKind::Layout),
    ),
    (
        "std::mem::Allocation",
        BuiltinType::Storage(StorageKind::Allocation),
    ),
    (
        "std::mem::AllocError",
        BuiltinType::Storage(StorageKind::AllocError),
    ),
    ("std::rc::Weak", BuiltinType::Storage(StorageKind::Weak)),
    (
        "std::num::ParseError",
        BuiltinType::Storage(StorageKind::ParseError),
    ),
    ("std::cell::Cell", BuiltinType::Storage(StorageKind::Cell)),
    (
        "std::collections::Vec",
        BuiltinType::Storage(StorageKind::Vec),
    ),
    (
        "std::collections::VecDeque",
        BuiltinType::Storage(StorageKind::VecDeque),
    ),
    (
        "std::lookup::StoragePath",
        BuiltinType::Storage(StorageKind::StoragePath),
    ),
    (
        "std::lookup::Location",
        BuiltinType::Storage(StorageKind::Location),
    ),
    (
        "std::option::Option",
        BuiltinType::Storage(StorageKind::Option),
    ),
    (
        "std::result::Result",
        BuiltinType::Storage(StorageKind::Result),
    ),
    ("std::any::Any", BuiltinType::Any),
    ("std::any::Symbol", BuiltinType::Symbol),
    ("std::service::Context", BuiltinType::Context),
    ("std::service::Registration", BuiltinType::Registration),
    ("std::service::ServiceKey", BuiltinType::ServiceKey),
    ("std::service::ServiceRef", BuiltinType::ServiceRef),
    (
        "std::mem::manual::ArenaCell",
        BuiltinType::Storage(StorageKind::ArenaCell),
    ),
];
const FUNCTIONS: &[(&str, Intrinsic)] = &[
    ("std::mem::Layout::new", Intrinsic::LayoutNew),
    ("std::mem::allocate", Intrinsic::Allocate),
    ("std::mem::deallocate", Intrinsic::Deallocate),
    ("std::mem::AllocError::new", Intrinsic::AllocErrorNew),
    ("std::result::ok", Intrinsic::ResultOk),
    ("std::result::err", Intrinsic::ResultErr),
    ("std::rc::try_new_in", Intrinsic::RcTryNewIn),
    ("std::rc::clone", Intrinsic::RcClone),
    ("std::rc::downgrade", Intrinsic::RcDowngrade),
    ("std::rc::upgrade", Intrinsic::RcUpgrade),
    ("std::rc::strong_count", Intrinsic::RcStrongCount),
    ("std::rc::weak_count", Intrinsic::RcWeakCount),
    ("std::gc::clone", Intrinsic::GcClone),
    ("std::mem::drop", Intrinsic::DropValue),
    ("std::mem::clone", Intrinsic::CloneValue),
    ("std::lookup::field", Intrinsic::FieldLocation),
    ("std::collections::Vec::from", Intrinsic::VecFrom),
    ("std::collections::VecDeque::from", Intrinsic::DequeFrom),
    ("std::array::empty", Intrinsic::ArrayEmpty),
    ("std::array::from", Intrinsic::ArrayFrom),
    ("std::array::len", Intrinsic::ArrayLength),
    ("std::array::sum", Intrinsic::ArraySum),
    ("std::num::parse", Intrinsic::Parse),
    ("std::num::abs", Intrinsic::NumAbs),
    ("std::num::min", Intrinsic::NumMin),
    ("std::num::max", Intrinsic::NumMax),
    ("std::num::clamp", Intrinsic::NumClamp),
    ("std::num::round", Intrinsic::FloatRound),
    ("std::num::floor", Intrinsic::FloatFloor),
    ("std::num::ceil", Intrinsic::FloatCeil),
    ("std::num::sqrt", Intrinsic::FloatSqrt),
    ("std::num::pow", Intrinsic::FloatPow),
    ("std::string::len", Intrinsic::StringLength),
    ("std::string::trim", Intrinsic::Trim),
    ("std::string::upper", Intrinsic::Upper),
    ("std::string::lower", Intrinsic::Lower),
    ("std::string::contains", Intrinsic::Contains),
    ("std::string::starts_with", Intrinsic::StartsWith),
    ("std::string::ends_with", Intrinsic::EndsWith),
    ("std::string::replace", Intrinsic::Replace),
    ("std::string::from", Intrinsic::StringFrom),
    ("std::char::code", Intrinsic::CharCode),
    ("std::char::from_code", Intrinsic::CharFromCode),
    ("std::random::uniform", Intrinsic::Random),
    ("std::cell::Cell::new", Intrinsic::CellNew),
    ("std::lookup::path", Intrinsic::LookupPath),
    ("std::lookup::missing", Intrinsic::MissingPath),
    ("std::lookup::mapped", Intrinsic::MappedLookup),
    ("std::option::some", Intrinsic::OptionSome),
    ("std::option::none", Intrinsic::OptionNone),
    ("std::any::erase", Intrinsic::Erase),
    ("std::any::checked", Intrinsic::Checked),
    ("std::any::fresh_symbol", Intrinsic::FreshSymbol),
    ("std::service::Context::new", Intrinsic::ContextNew),
    ("std::service::ServiceKey::new", Intrinsic::ServiceKeyNew),
    ("std::io::print", Intrinsic::Print),
    ("std::runtime::caie::output", Intrinsic::Print),
    ("std::runtime::caie::input", Intrinsic::Input),
    ("std::runtime::caie::left", Intrinsic::Left),
    ("std::runtime::caie::right", Intrinsic::Right),
    ("std::runtime::caie::mid", Intrinsic::Mid),
    ("std::runtime::caie::length", Intrinsic::Length),
    ("std::runtime::caie::lcase", Intrinsic::LowerChar),
    ("std::runtime::caie::ucase", Intrinsic::UpperChar),
    ("std::runtime::caie::int", Intrinsic::Truncate),
    ("std::runtime::caie::rand", Intrinsic::Random),
    ("std::runtime::caie::asc", Intrinsic::CharCode),
    ("std::runtime::caie::chr", Intrinsic::CharFromCode),
    ("std::runtime::caie::num_to_str", Intrinsic::StringFrom),
    ("std::runtime::caie::str_to_num", Intrinsic::ParseLegacy),
    ("std::runtime::caie::is_num", Intrinsic::IsNumeric),
];
pub fn item(path: &str) -> Option<Item> {
    if TRAITS.contains(&path) {
        return Some(Item::Trait);
    }
    if let Some(module) = MODULES.iter().find(|m| **m == path) {
        return Some(Item::Module(module));
    }
    if let Some((_, ty)) = TYPES.iter().find(|(p, _)| *p == path) {
        return Some(Item::Type(*ty));
    }
    FUNCTIONS
        .iter()
        .find(|(p, _)| *p == path)
        .map(|(_, id)| Item::Function(*id))
}
pub fn exports(module: &str) -> Vec<(String, String)> {
    if module == "std::prelude" {
        return PRELUDE
            .iter()
            .map(|(n, p)| (n.to_string(), p.to_string()))
            .collect();
    }
    let prefix = format!("{module}::");
    MODULES
        .iter()
        .copied()
        .chain(TRAITS.iter().copied())
        .chain(TYPES.iter().map(|(p, _)| *p))
        .chain(FUNCTIONS.iter().map(|(p, _)| *p))
        .filter_map(|p| {
            p.strip_prefix(&prefix)
                .filter(|n| !n.contains("::"))
                .map(|n| (n.to_owned(), p.to_owned()))
        })
        .collect()
}
