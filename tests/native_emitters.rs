//! Both ISA seams are type checked and emit forward fixups on every host.
//! Actual execution is covered by engines/native_storage on native CI runners.
use winch_codegen::native::{Arithmetic, Condition};
macro_rules! smoke {
    ($name:ident,$module:ident) => {
        #[test]
        fn $name() {
            let mut a = winch_codegen::native::$module::Assembler::new();
            let fail = a.label();
            let end = a.label();
            a.prologue();
            a.imm(7, 13);
            a.imm(8, 17);
            a.checked(Arithmetic::Mul, 7, 7, 8, fail);
            a.compare(7, 8);
            a.branch(Condition::Gt, end);
            a.bind(fail);
            a.imm(7, 0);
            a.bind(end);
            a.epilogue();
            assert!(!a.finish().is_empty());
        }
    };
}
smoke!(aarch64_fixups, aarch64);
smoke!(x64_fixups, x64);
