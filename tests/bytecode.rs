use cyallo::{
    bytecode::{Instruction, Opcode},
    compile,
};
#[test]
fn bytecode_is_compact_and_preserves_control_flow() {
    let p = compile("let mut n = 0\nwhile n < 3 { n += 1 }\nstd::io::print(n)\n").unwrap();
    assert_eq!(size_of::<Instruction>(), 16);
    let f = &p.bytecode().functions[0];
    assert_eq!(f.instructions.len(), f.spans.len());
    assert!(f.instructions.iter().any(|i| i.opcode == Opcode::Branch));
    for (pc, source) in p.functions()[0].code.iter().enumerate() {
        assert_eq!(format!("{:?}", f.decode(pc)), format!("{:?}", source.op));
        assert_eq!(f.spans[pc], source.span);
    }
    assert!(p.bytecode().disassemble().contains("Branch"));
}
#[test]
fn constants_roundtrip_exactly() {
    let p = compile(
        "std::io::print(-9223372036854775808,9223372036854775807,1.25,true,'水',\"hello\")\n",
    )
    .unwrap();
    for (f, code) in p.functions().iter().zip(&p.bytecode().functions) {
        for (pc, inst) in f.code.iter().enumerate() {
            assert_eq!(format!("{:?}", code.decode(pc)), format!("{:?}", inst.op));
        }
    }
}
