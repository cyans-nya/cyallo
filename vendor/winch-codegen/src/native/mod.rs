//! Small native emission seam. Frontends own control flow, semantics and spill policy.
#![allow(
    missing_docs,
    reason = "private fork interface documented by the first-party frontend"
)]
pub use cranelift_codegen::MachLabel as Label;
#[derive(Clone, Copy, Debug)]
pub enum Condition {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Lo,
    Ls,
    Hi,
    Hs,
    Overflow,
}
#[derive(Clone, Copy, Debug)]
pub enum Arithmetic {
    Add,
    Sub,
    Mul,
    And,
    Or,
    Xor,
}
#[derive(Clone, Copy, Debug)]
pub enum FloatArithmetic {
    Add,
    Sub,
    Mul,
    Div,
}
#[cfg(feature = "arm64")]
pub mod aarch64;
#[cfg(feature = "x64")]
pub mod x64;
#[cfg(target_arch = "aarch64")]
pub use aarch64::Assembler;
#[cfg(target_arch = "x86_64")]
pub use x64::Assembler;
