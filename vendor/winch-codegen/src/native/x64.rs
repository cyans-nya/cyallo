//! x64 system C ABI emission. Saved registers are shared with the native frontend.
use super::*;
use crate::{
    isa::{
        reg::{Reg, writable},
        x64::asm,
    },
    masm::{DivKind, OperandSize as S, ShiftKind},
};
use cranelift_codegen::{
    isa::x64::{args::CC, settings as isa},
    settings,
};
// The address type remains private to x64; use an ISA-local constructor seam.
fn r(id: u8) -> Reg {
    Reg::int(match id {
        0 => 12,
        1 => 13,
        2 => 14,
        3 => 15,
        4 => 0,
        5 => 1,
        6 => 2,
        7 => 8,
        8 => 9,
        9 => 10,
        10 => 11,
        11 => 6,
        12 => 7,
        13 => 3,
        _ => panic!("invalid native register"),
    })
}
fn f(id: u8) -> Reg {
    Reg::float(id as usize)
}
fn cc(cond: Condition) -> CC {
    match cond {
        Condition::Eq => CC::Z,
        Condition::Ne => CC::NZ,
        Condition::Lt => CC::L,
        Condition::Le => CC::LE,
        Condition::Gt => CC::NLE,
        Condition::Ge => CC::NL,
        Condition::Lo => CC::B,
        Condition::Ls => CC::BE,
        Condition::Hi => CC::NBE,
        Condition::Hs => CC::NB,
        Condition::Overflow => CC::O,
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
        self.asm.buffer_mut().get_label()
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
        self.asm.mov_ir(value, writable!(r(dst)), S::S64);
    }
    pub fn load(&mut self, dst: u8, base: u8, offset: u32) {
        self.asm.native_load(r(base), offset, writable!(r(dst)));
    }
    pub fn store(&mut self, src: u8, base: u8, offset: u32) {
        self.asm.native_store(r(src), r(base), offset);
    }
    pub fn binary(&mut self, op: Arithmetic, dst: u8, a: u8, mut b: u8) {
        if dst != a {
            if dst == b {
                self.mov(12, b);
                b = 12;
            }
            self.mov(dst, a);
        }
        match op {
            Arithmetic::Add => self.asm.add_rr(r(b), writable!(r(dst)), S::S64),
            Arithmetic::Sub => self.asm.sub_rr(r(b), writable!(r(dst)), S::S64),
            Arithmetic::Mul => self.asm.mul_rr(r(b), writable!(r(dst)), S::S64),
            Arithmetic::And => self.asm.and_rr(r(b), writable!(r(dst)), S::S64),
            Arithmetic::Or => self.asm.or_rr(r(b), writable!(r(dst)), S::S64),
            Arithmetic::Xor => self.asm.xor_rr(r(b), writable!(r(dst)), S::S64),
        }
    }
    pub fn checked(&mut self, op: Arithmetic, dst: u8, a: u8, b: u8, overflow: Label) {
        self.binary(op, dst, a, b);
        self.branch(Condition::Overflow, overflow);
    }
    pub fn divide(&mut self, dst: u8, a: u8, b: u8) {
        self.asm.mov_rr(r(b), writable!(Reg::int(11)), S::S64);
        self.asm.mov_rr(r(a), writable!(Reg::int(0)), S::S64);
        self.asm.div(
            Reg::int(11),
            (Reg::int(0), Reg::int(2)),
            DivKind::Signed,
            S::S64,
        );
        self.asm.mov_rr(Reg::int(0), writable!(r(dst)), S::S64);
    }
    pub fn compare(&mut self, a: u8, b: u8) {
        self.asm.cmp_rr(r(a), r(b), S::S64);
    }
    pub fn shift(&mut self, dst: u8, src: u8, amount: u8) {
        if dst != src {
            self.mov(dst, src);
        }
        self.asm
            .shift_ir(amount, writable!(r(dst)), ShiftKind::Shl, S::S64);
    }
    pub fn bits_to_float(&mut self, dst: u8, src: u8) {
        self.asm.gpr_to_xmm(r(src), writable!(f(dst)), S::S64);
    }
    pub fn float_to_bits(&mut self, dst: u8, src: u8) {
        self.asm.xmm_to_gpr(f(src), writable!(r(dst)), S::S64);
    }
    pub fn int_to_float(&mut self, dst: u8, src: u8) {
        self.asm
            .cvt_sint_to_float(r(src), writable!(f(dst)), S::S64, S::S64);
    }
    pub fn float_binary(&mut self, op: FloatArithmetic, dst: u8, a: u8, b: u8) {
        if dst != a {
            self.asm.xmm_mov_rr(f(a), writable!(f(dst)), S::S64);
        }
        match op {
            FloatArithmetic::Add => self.asm.xmm_add_rr(f(b), writable!(f(dst)), S::S64),
            FloatArithmetic::Sub => self.asm.xmm_sub_rr(f(b), writable!(f(dst)), S::S64),
            FloatArithmetic::Mul => self.asm.xmm_mul_rr(f(b), writable!(f(dst)), S::S64),
            FloatArithmetic::Div => self.asm.xmm_div_rr(f(b), writable!(f(dst)), S::S64),
        }
    }
    pub fn float_branch(&mut self, a: u8, b: u8, cond: Condition, label: Label) {
        self.asm.ucomis(f(a), f(b), S::S64);
        if matches!(cond, Condition::Ne) {
            self.asm.jmp_if(CC::P, label);
            self.asm.jmp_if(CC::NZ, label);
            return;
        }
        let skip = self.label();
        if matches!(cond, Condition::Eq | Condition::Lt | Condition::Le) {
            self.asm.jmp_if(CC::P, skip);
        }
        self.asm.jmp_if(
            match cond {
                Condition::Eq => CC::Z,
                Condition::Lt => CC::B,
                Condition::Le => CC::BE,
                Condition::Gt => CC::NBE,
                Condition::Ge => CC::NB,
                _ => unreachable!(),
            },
            label,
        );
        self.bind(skip);
    }
    pub fn prologue(&mut self) {
        self.asm.push_r(Reg::int(5));
        self.asm.mov_rr(Reg::int(4), writable!(Reg::int(5)), S::S64);
        for reg in [12, 13, 14, 15, 3] {
            self.asm.push_r(Reg::int(reg));
        }
        if cfg!(target_os = "windows") {
            for reg in [6, 7] {
                self.asm.push_r(Reg::int(reg));
            }
        }
        self.asm.sub_ir(
            if cfg!(target_os = "windows") { 40 } else { 8 },
            writable!(Reg::int(4)),
            S::S64,
        );
        let args = if cfg!(target_os = "windows") {
            [1, 2, 8]
        } else {
            [7, 6, 2]
        };
        for (id, arg) in args.into_iter().enumerate() {
            self.asm
                .mov_rr(Reg::int(arg), writable!(r(id as u8)), S::S64);
        }
        self.imm(3, 0);
    }
    pub fn epilogue(&mut self) {
        self.asm.mov_rr(r(2), writable!(Reg::int(0)), S::S64);
        self.asm.add_ir(
            if cfg!(target_os = "windows") { 40 } else { 8 },
            writable!(Reg::int(4)),
            S::S64,
        );
        if cfg!(target_os = "windows") {
            for reg in [7, 6] {
                self.asm.pop_r(writable!(Reg::int(reg)));
            }
        }
        for reg in [3, 15, 14, 13, 12, 5] {
            self.asm.pop_r(writable!(Reg::int(reg)));
        }
        self.asm.ret(0);
    }
    fn args(&mut self) {
        let args = if cfg!(target_os = "windows") {
            [1, 2, 8]
        } else {
            [7, 6, 2]
        };
        for (id, arg) in args.into_iter().enumerate() {
            self.asm
                .mov_rr(r(id as u8), writable!(Reg::int(arg)), S::S64);
        }
    }
    pub fn call(&mut self, address: u64) {
        self.args();
        self.asm.mov_ir(address, writable!(Reg::int(11)), S::S64);
        self.asm.call_with_reg(
            if cfg!(target_os = "windows") {
                crate::CallingConvention::WindowsFastcall
            } else {
                crate::CallingConvention::SystemV
            },
            Reg::int(11),
        );
    }
    pub fn call_entry(&mut self, address: u8) {
        self.args();
        self.asm.call_with_reg(
            if cfg!(target_os = "windows") {
                crate::CallingConvention::WindowsFastcall
            } else {
                crate::CallingConvention::SystemV
            },
            r(address),
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
