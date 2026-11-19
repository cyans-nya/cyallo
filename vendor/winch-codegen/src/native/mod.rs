//! Small native emission seam. Frontends own control flow, semantics and spill policy.
#![allow(
    missing_docs,
    reason = "private fork interface documented by the first-party frontend"
)]
use cranelift_codegen::MachBufferFinalized;
pub use cranelift_codegen::MachLabel as Label;

#[derive(Debug, Clone, Copy)]
pub struct SourceRange {
    pub site: u32,
    pub start: u32,
    pub end: u32,
}

const SCALAR_SOURCE_BASE: u32 = 0;

fn finish_image(buffer: MachBufferFinalized) -> Option<(Vec<u8>, Vec<SourceRange>)> {
    if !buffer.relocs().is_empty() {
        return None;
    }
    let ranges = buffer
        .get_srclocs_sorted()
        .iter()
        .map(|range| SourceRange {
            site: range.loc.as_abs().bits(),
            start: range.start,
            end: range.end,
        })
        .collect();
    Some((buffer.data().to_vec(), ranges))
}
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
