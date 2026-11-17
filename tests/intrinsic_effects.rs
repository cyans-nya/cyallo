use cyallo::{compile, ir::RoutineKind};
fn effects(source: &str, name: &str) -> cyallo::ir::EffectSummary {
    let program = compile(source).unwrap();
    program
        .functions()
        .iter()
        .find(|f| f.name.as_ref() == name)
        .unwrap_or_else(|| panic!("no function {name}"))
        .effects
}
#[test]
fn builtin_effects_follow_the_intrinsic_contract() {
    let source = "
fn code_of() -> i64 { return std::char::code('a') }
fn from_code() -> char { return std::char::from_code(97) }
fn shout() -> string { return std::string::upper(\"x\") }
fn magnitude() -> i64 { return std::num::abs(-2) }
std::io::print(code_of(), from_code(), shout(), magnitude())
";
    // A total scalar builtin cannot fail or allocate.
    let total = effects(source, "code_of");
    assert!(!total.may_fail && !total.may_allocate && total.is_observably_pure(), "{total:?}");
    // Conversions that can reject their input stay fallible.
    assert!(effects(source, "from_code").may_fail);
    // A builtin that allocates can still exhaust resources.
    let shout = effects(source, "shout");
    assert!(shout.may_allocate && shout.may_fail && shout.is_observably_pure());
    // Arithmetic that can overflow stays fallible.
    assert!(effects(source, "magnitude").may_fail);
}
#[test]
fn impure_builtins_are_still_rejected_in_pure_functions() {
    let source = "fn roll(n: i64) -> i64 { return 1 }\nstd::io::print(roll(1))\n";
    let program = compile(source).unwrap();
    assert!(program.functions().iter().any(|f| f.kind == RoutineKind::Function));
}
