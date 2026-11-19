//! AArch64 native C ABI emission; independent of Wasmtime VM contexts.
use super::*;
use crate::{
    isa::{
        aarch64::asm,
        reg::{Reg, writable},
    },
    masm::{Imm, OperandSize as S, ShiftKind},
};
use cranelift_codegen::{
    ir::{MemFlagsData, types},
    isa::aarch64::{
        inst::{AMode, Cond, ImmShift, PairAMode, SImm7Scaled, SImm9},
        settings as isa,
    },
    settings,
};
fn r(id: u8) -> Reg {
    Reg::int(match id {
        0 => 19,
        1 => 20,
        2 => 21,
        3 => 22,
        13 => 23,
        4..=12 => 9 + (id - 4) as usize,
        _ => panic!("invalid native register"),
    })
}
fn f(id: u8) -> Reg {
    Reg::float(16 + id as usize)
}
fn cc(cond: Condition) -> Cond {
    match cond {
        Condition::Eq => Cond::Eq,
        Condition::Ne => Cond::Ne,
        Condition::Lt => Cond::Lt,
        Condition::Le => Cond::Le,
        Condition::Gt => Cond::Gt,
        Condition::Ge => Cond::Ge,
        Condition::Lo => Cond::Lo,
        Condition::Ls => Cond::Ls,
        Condition::Hi => Cond::Hi,
        Condition::Hs => Cond::Hs,
        Condition::Overflow => Cond::Vs,
    }
}
pub struct Assembler {
    asm: asm::Assembler,
}
impl Assembler {
    pub fn new() -> Self {
        let flags = settings::Flags::new(settings::builder());
        Self {
            asm: asm::Assembler::new(flags.clone(), isa::Flags::new(&flags, &isa::builder())),
        }
    }
    pub fn label(&mut self) -> Label {
        self.asm.get_label()
    }
    pub fn bind(&mut self, label: Label) {
        self.asm
            .buffer_mut()
            .bind_label(label, &mut Default::default());
    }
    pub fn start_site(&mut self, site: u32) {
        self.asm
            .buffer_mut()
            .start_srcloc(cranelift_codegen::ir::RelSourceLoc::new(site));
    }
    pub fn end_site(&mut self) {
        self.asm.buffer_mut().end_srcloc();
    }
    pub fn finish_image(self) -> Option<(Vec<u8>, Vec<SourceRange>)> {
        let base = cranelift_codegen::ir::SourceLoc::new(SCALAR_SOURCE_BASE);
        super::finish_image(self.asm.finalize(Some(base)))
    }
    pub fn jump(&mut self, label: Label) {
        self.asm.jmp(label);
    }
    pub fn branch(&mut self, cond: Condition, label: Label) {
        self.asm.jmp_if(cc(cond), label);
    }
    pub fn mov(&mut self, dst: u8, src: u8) {
        self.asm.mov_rr(r(src), writable!(r(dst)), S::S64);
    }
    pub fn imm(&mut self, dst: u8, value: u64) {
        self.asm.mov_ir(writable!(r(dst)), Imm::I64(value), S::S64);
    }
    pub fn load(&mut self, dst: u8, base: u8, offset: u32) {
        self.asm.uload(
            AMode::Unscaled {
                rn: r(base).into(),
                simm9: SImm9::maybe_from_i64(offset as i64).expect("native field offset"),
            },
            writable!(r(dst)),
            S::S64,
            MemFlagsData::trusted(),
        );
    }
    pub fn store(&mut self, src: u8, base: u8, offset: u32) {
        self.asm.str(
            r(src),
            AMode::Unscaled {
                rn: r(base).into(),
                simm9: SImm9::maybe_from_i64(offset as i64).expect("native field offset"),
            },
            S::S64,
            MemFlagsData::trusted(),
        );
    }
    pub fn binary(&mut self, op: Arithmetic, dst: u8, a: u8, b: u8) {
        match op {
            Arithmetic::Add => self.asm.add_rrr(r(b), r(a), writable!(r(dst)), S::S64),
            Arithmetic::Sub => self.asm.sub_rrr(r(b), r(a), writable!(r(dst)), S::S64),
            Arithmetic::Mul => self.asm.mul_rrr(r(b), r(a), writable!(r(dst)), S::S64),
            Arithmetic::And => self.asm.and_rrr(r(b), r(a), writable!(r(dst)), S::S64),
            Arithmetic::Or => self.asm.or_rrr(r(b), r(a), writable!(r(dst)), S::S64),
            Arithmetic::Xor => self.asm.xor_rrr(r(b), r(a), writable!(r(dst)), S::S64),
        }
    }
    pub fn checked(&mut self, op: Arithmetic, dst: u8, a: u8, b: u8, overflow: Label) {
        match op {
            Arithmetic::Add => self.asm.adds_rrr(r(b), r(a), writable!(r(dst)), S::S64),
            Arithmetic::Sub => self.asm.subs_rrr(r(b), r(a), writable!(r(dst)), S::S64),
            Arithmetic::Mul => {
                // Frontend reserves logical register 12 for the overflow sequence.
                self.asm.smulh_rrr(r(b), r(a), writable!(r(12)));
                self.asm.mul_rrr(r(b), r(a), writable!(r(dst)), S::S64);
                self.asm.shift_ir(
                    ImmShift::maybe_from_u64(63).unwrap(),
                    r(dst),
                    writable!(r(11)),
                    ShiftKind::ShrS,
                    S::S64,
                );
                self.compare(12, 11);
                self.branch(Condition::Ne, overflow);
                return;
            }
            _ => unreachable!(),
        }
        self.branch(Condition::Overflow, overflow);
    }
    pub fn divide(&mut self, dst: u8, a: u8, b: u8) {
        self.asm.native_div(r(b), r(a), writable!(r(dst)));
    }
    pub fn compare(&mut self, a: u8, b: u8) {
        self.asm
            .subs_rrr(r(b), r(a), writable!(Reg::int(31)), S::S64);
    }
    pub fn shift(&mut self, dst: u8, src: u8, amount: u8) {
        self.asm.shift_ir(
            ImmShift::maybe_from_u64(amount as u64).unwrap(),
            r(src),
            writable!(r(dst)),
            ShiftKind::Shl,
            S::S64,
        );
    }
    pub fn bits_to_float(&mut self, dst: u8, src: u8) {
        self.asm.mov_to_fpu(r(src), writable!(f(dst)), S::S64);
    }
    pub fn float_to_bits(&mut self, dst: u8, src: u8) {
        self.asm.mov_from_vec(f(src), writable!(r(dst)), 0, S::S64);
    }
    pub fn int_to_float(&mut self, dst: u8, src: u8) {
        self.asm
            .cvt_sint_to_float(r(src), writable!(f(dst)), S::S64, S::S64);
    }
    pub fn float_binary(&mut self, op: FloatArithmetic, dst: u8, a: u8, b: u8) {
        match op {
            FloatArithmetic::Add => self.asm.fadd_rrr(f(b), f(a), writable!(f(dst)), S::S64),
            FloatArithmetic::Sub => self.asm.fsub_rrr(f(b), f(a), writable!(f(dst)), S::S64),
            FloatArithmetic::Mul => self.asm.fmul_rrr(f(b), f(a), writable!(f(dst)), S::S64),
            FloatArithmetic::Div => self.asm.fdiv_rrr(f(b), f(a), writable!(f(dst)), S::S64),
        }
    }
    pub fn float_branch(&mut self, a: u8, b: u8, cond: Condition, label: Label) {
        self.asm.fcmp(f(a), f(b), S::S64);
        self.asm.jmp_if(
            match cond {
                Condition::Lt => Cond::Mi,
                Condition::Le => Cond::Ls,
                _ => cc(cond),
            },
            label,
        );
    }
    fn push(&mut self, a: usize, b: usize) {
        self.asm.stp(
            Reg::int(a),
            Reg::int(b),
            PairAMode::SPPreIndexed {
                simm7: SImm7Scaled::maybe_from_i64(-16, types::I64).unwrap(),
            },
        );
    }
    fn pop(&mut self, a: usize, b: usize) {
        self.asm.ldp(
            Reg::int(a),
            Reg::int(b),
            PairAMode::SPPostIndexed {
                simm7: SImm7Scaled::maybe_from_i64(16, types::I64).unwrap(),
            },
        );
    }
    pub fn prologue(&mut self) {
        self.push(29, 30);
        self.asm
            .mov_rr(Reg::int(63), writable!(Reg::int(29)), S::S64);
        self.push(19, 20);
        self.push(21, 22);
        self.push(23, 24);
        for (id, arg) in [(0, 0), (1, 1), (2, 2)] {
            self.asm.mov_rr(Reg::int(arg), writable!(r(id)), S::S64);
        }
        self.imm(3, 0);
    }
    pub fn epilogue(&mut self) {
        self.asm.mov_rr(r(2), writable!(Reg::int(0)), S::S64);
        self.pop(23, 24);
        self.pop(21, 22);
        self.pop(19, 20);
        self.pop(29, 30);
        self.asm.ret();
    }
    pub fn call(&mut self, address: u64) {
        self.asm.mov_rr(r(0), writable!(Reg::int(0)), S::S64);
        self.imm(12, address);
        self.asm.call_with_reg(
            r(12),
            if cfg!(target_vendor = "apple") {
                crate::CallingConvention::AppleAarch64
            } else {
                crate::CallingConvention::SystemV
            },
        );
    }
    pub fn call_entry(&mut self, address: u8) {
        for (arg, src) in [(0, 0), (1, 1), (2, 2)] {
            self.asm.mov_rr(r(src), writable!(Reg::int(arg)), S::S64);
        }
        self.asm.call_with_reg(
            r(address),
            if cfg!(target_vendor = "apple") {
                crate::CallingConvention::AppleAarch64
            } else {
                crate::CallingConvention::SystemV
            },
        );
        self.asm.mov_rr(Reg::int(0), writable!(r(2)), S::S64);
    }
    pub fn finish(self) -> Vec<u8> {
        self.asm.finalize(None).data().to_vec()
    }
}
impl Default for Assembler {
    fn default() -> Self {
        Self::new()
    }
}
