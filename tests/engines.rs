use cyallo::{
    Context, Session, Status,
    backend::{
        CraneliftBackend, CraneliftSpeedBackend, Executable, NativeBackend, Tier, TierPolicy,
        TieredBackend,
    },
};
use std::sync::Arc;
fn execute(source: &str, input: &[&str]) -> (String, Status) {
    let p = Arc::new(
        cyallo::compile_with_options(
            source,
            cyallo::CompileOptions {
                dialect: cyallo::Dialect::Compatibility,
                convenience_builtins: true,
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let context = Context {
        input: input.iter().map(|s| s.to_string()).collect(),
        ..Context::default()
    };
    let mut a = Session::with_context(p.clone(), context.clone()).unwrap();
    a.run(100_000);
    let baseline = TieredBackend {
        policy: TierPolicy {
            small_limit: usize::MAX - 1,
            heavy_limit: usize::MAX,
            hot_instructions: u64::MAX,
            warm_instructions: u64::MAX,
            ..Default::default()
        },
    }
    .compile(p.clone())
    .unwrap();
    for fi in 0..p.functions().len() {
        baseline.precompile(fi, Tier::Winch).unwrap();
    }
    assert!(baseline.wait_for_compilation(std::time::Duration::from_secs(30)));
    let artifacts: Vec<Box<dyn Executable>> = vec![
        Box::new(CraneliftBackend.compile(p.clone()).unwrap()),
        Box::new(CraneliftSpeedBackend.compile(p.clone()).unwrap()),
        Box::new(baseline),
    ];
    #[cfg(feature = "wasm-adapter")]
    let artifacts = {
        let mut artifacts = artifacts;
        artifacts.push(Box::new(
            cyallo::backend::WasmBackend.compile(p.clone()).unwrap(),
        ));
        artifacts
    };
    for native in artifacts {
        let mut b = Session::with_context(p.clone(), context.clone()).unwrap();
        native.run(&mut b, 100_000).unwrap();
        assert_eq!(a.context.output, b.context.output, "output differs");
        assert_eq!(a.status(), b.status(), "status differs");
        assert_eq!(
            a.instruction_count(),
            b.instruction_count(),
            "instruction count differs"
        );
    }
    (a.context.output.clone(), a.status().clone())
}
fn output(source: &str, expected: &str) {
    let (out, status) = execute(source, &[]);
    assert_eq!(out, expected);
    assert_eq!(status, Status::Finished);
}
#[test]
fn examples() {
    for (source, expected) in [
        (include_str!("../examples/loops.psc"), "5050\n"),
        (include_str!("../examples/functions.psc"), "3628800\n"),
        (include_str!("../examples/arrays.psc"), "4,4\n"),
        (include_str!("../examples/strings.psc"), "Cambridge9\n"),
    ] {
        output(source, expected);
    }
}
#[test]
fn control_flow() {
    output(
        "DECLARE i : INTEGER\ni <- 0\nWHILE i < 3\n i <- i + 1\nENDWHILE\nREPEAT\n i <- i - 1\nUNTIL i = 0\nCASE OF i\n 0 : OUTPUT \"zero\"\n 1 : OUTPUT \"one\"\n OTHERWISE OUTPUT \"other\"\nENDCASE\nFOR i <- 3 TO 1 STEP -1\n OUTPUT i\nNEXT i\n",
        "zero\n3\n2\n1\n",
    );
}
#[test]
fn byref_alias_and_byval_copy() {
    output(
        "DECLARE a : ARRAY[1:2] OF INTEGER\na[1] <- 10\na[2] <- 20\nPROCEDURE Change(BYREF x : INTEGER, BYREF y : INTEGER, BYVAL b : ARRAY[1:2] OF INTEGER)\n x <- x + 1\n y <- y + 2\n b[1] <- 999\nENDPROCEDURE\nCALL Change(a[1], a[1], a)\nOUTPUT a[1], \":\", a[2]\n",
        "13:20\n",
    );
}
#[test]
fn multidimensional_and_array_assignment() {
    output(
        "DECLARE a : ARRAY[-1:0,2:3] OF INTEGER\nDECLARE b : ARRAY[-1:0,2:3] OF INTEGER\na[-1,2] <- 7\nb <- a\na[-1,2] <- 8\nOUTPUT b[-1,2], a[-1,2]\n",
        "78\n",
    );
}
#[test]
fn short_circuit() {
    output(
        "OUTPUT FALSE AND (1 DIV 0 = 0), TRUE OR (1 DIV 0 = 0)\n",
        "FALSETRUE\n",
    );
}
#[test]
fn scalar_math() {
    output(
        "OUTPUT -7 DIV 3, \",\", -7 MOD 3, \",\", 7 DIV -3, \",\", 7 MOD -3\nOUTPUT 1.5 + 2.5, 2.0 * 3.0, 8.0 - 1.0, 3 / 2\n",
        "-2,-1,-2,1\n4671.5\n",
    );
}
#[test]
fn unicode_and_builtins() {
    output(
        "OUTPUT LENGTH(\"é水🐈\"), MID(\"é水🐈\", 2, 1), CHR(ASC('🐈')), UCASE('a'), LCASE('Z'), INT(-1.2), IS_NUM(\"3.5\"), STR_TO_NUM(\"3.5\"), NUM_TO_STR(12)\n",
        "3水🐈Az-1TRUE3.512\n",
    );
}
#[test]
fn input() {
    let (out, status) = execute(
        "DECLARE n : INTEGER\nDECLARE s : STRING\nDECLARE c : CHAR\nDECLARE b : BOOLEAN\nINPUT n\nINPUT s\nINPUT c\nINPUT b\nOUTPUT n, s, c, b\n",
        &["42", " hello ", "🐈", "TRUE"],
    );
    assert_eq!(out, "42 hello 🐈TRUE\n");
    assert_eq!(status, Status::Finished);
}
#[test]
fn random_is_reproducible() {
    let (out, status) = execute("OUTPUT RAND(10)\nOUTPUT RAND(10)\n", &[]);
    assert_eq!(status, Status::Finished);
    for n in out.lines() {
        assert!((0.0..10.0).contains(&n.parse::<f64>().unwrap()));
    }
}
#[test]
fn failures_match() {
    for (source, message) in [
        ("OUTPUT 9223372036854775807 + 1\n", "overflow"),
        ("OUTPUT 9223372036854775807 * 2\n", "overflow"),
        ("OUTPUT (-9223372036854775807 - 1) - 1\n", "overflow"),
        ("OUTPUT 1 DIV 0\n", "division by zero"),
        ("OUTPUT 1 / 0\n", "division by zero"),
        ("DECLARE x : INTEGER\nOUTPUT x\n", "uninitialized"),
        (
            "FUNCTION F() RETURNS INTEGER\n DECLARE x : INTEGER\n RETURN x\nENDFUNCTION\nOUTPUT F()\n",
            "uninitialized",
        ),
        (
            "DECLARE a : ARRAY[1:2] OF INTEGER\nOUTPUT a[3]\n",
            "outside",
        ),
        ("OUTPUT MID(\"abc\",0,1)\n", "bounds"),
        (
            "DECLARE i : INTEGER\nFOR i <- 1 TO 2 STEP 0\nNEXT i\n",
            "zero",
        ),
    ] {
        let (_, status) = execute(source, &[]);
        let Status::Failed(d) = status else {
            panic!("expected failure: {source}")
        };
        assert!(d.message.contains(message), "{}", d.message);
        assert!(d.span.end > d.span.start);
    }
}
#[test]
fn invalid_programs_are_rejected() {
    for source in [
        "OUTPUT missing\n",
        "DECLARE x : INTEGER\nx <- \"wrong\"\n",
        "DECLARE x : INTEGER\nDECLARE X : INTEGER\n",
        "CONSTANT x = 1\nx <- 2\n",
        "FUNCTION F(BYREF x : INTEGER) RETURNS INTEGER\nRETURN x\nENDFUNCTION\n",
        "FUNCTION F() RETURNS INTEGER\nIF TRUE THEN\nRETURN 1\nENDIF\nENDFUNCTION\n",
        "DECLARE a : ARRAY[1:2] OF INTEGER\nOUTPUT a[1,2]\n",
        "DECLARE d : DATE\n",
        "CLASS X\nENDCLASS\n",
        "OUTPUT SUBSTRING(\"abc\",1,1)\n",
        "OUTPUT UCASE(\"abc\")\n",
    ] {
        assert!(compile(source).is_err(), "accepted {source}");
    }
}
#[test]
fn resumes_input_and_budget() {
    let p = Arc::new(compile("DECLARE n : INTEGER\nINPUT n\nOUTPUT n\n").unwrap());
    let native = CraneliftBackend.compile(p.clone()).unwrap();
    for jit in [false, true] {
        let mut s = Session::new(p.clone()).unwrap();
        if jit {
            native.run(&mut s, 100).unwrap();
        } else {
            s.run(100);
        }
        assert_eq!(s.status(), &Status::WaitingForInput);
        s.context.input.push_back("123".into());
        if jit {
            native.run(&mut s, 100).unwrap();
        } else {
            s.run(100);
        }
        assert_eq!(s.status(), &Status::Finished);
        assert_eq!(s.context.output, "123\n");
    }
}
#[test]
fn bounded_infinite_loop() {
    let p = Arc::new(compile("WHILE TRUE\nENDWHILE\n").unwrap());
    let native = CraneliftBackend.compile(p.clone()).unwrap();
    let mut s = Session::new(p).unwrap();
    s.run(100);
    assert_eq!(s.status(), &Status::Paused);
    native.run(&mut s, 100).unwrap();
    assert_eq!(s.status(), &Status::Paused);
}
#[test]
fn artifact_rejects_different_program() {
    let p = Arc::new(compile("OUTPUT 1\n").unwrap());
    let native = CraneliftBackend.compile(p).unwrap();
    let mut s = Session::new(Arc::new(compile("OUTPUT 2\n").unwrap())).unwrap();
    assert!(native.run(&mut s, 10).is_err());
}
#[test]
fn evaluation_order_snapshots_byval_arrays() {
    output(
        "DECLARE a : ARRAY[1:1] OF INTEGER\na[1] <- 10\nFUNCTION Mutate() RETURNS INTEGER\n a[1] <- 20\n RETURN 1\nENDFUNCTION\nPROCEDURE Show(b : ARRAY[1:1] OF INTEGER, unused : INTEGER)\n OUTPUT b[1]\nENDPROCEDURE\nCALL Show(a, Mutate())\nOUTPUT a[1]\n",
        "10\n20\n",
    );
}

#[test]
fn for_loop_can_finish_at_the_largest_integer() {
    output(
        "DECLARE i : INTEGER\nFOR i <- 9223372036854775806 TO 9223372036854775807\n OUTPUT i\nNEXT i\n",
        "9223372036854775806\n9223372036854775807\n",
    );
}

#[test]
fn returned_arrays_survive_their_frame() {
    output(
        "FUNCTION Values() RETURNS ARRAY[1:1] OF INTEGER\n DECLARE a : ARRAY[1:1] OF INTEGER\n a[1] <- 123\n RETURN a\nENDFUNCTION\nDECLARE b : ARRAY[1:1] OF INTEGER\nb <- Values()\nOUTPUT b[1]\n",
        "123\n",
    );
}
#[test]
fn integer_int_is_exact() {
    output("OUTPUT INT(9223372036854775807)\n", "9223372036854775807\n");
}
#[test]
fn case_body_can_contain_declarations() {
    output(
        "CASE OF 1\n 1 :\n  DECLARE x : INTEGER\n  x <- 10\n  OUTPUT x\nENDCASE\n",
        "10\n",
    );
}
#[test]
fn constants_require_literals() {
    for source in ["CONSTANT a = 1 + 2\n", "CONSTANT a = 1\nCONSTANT b = a\n"] {
        assert!(compile(source).is_err());
    }
    output(
        "CONSTANT a = -1\nCONSTANT b = -1.5\nOUTPUT a, b\n",
        "-1-1.5\n",
    );
}

fn compile(source: &str) -> Result<cyallo::ir::Program, cyallo::Diagnostic> {
    cyallo::compile_with_options(
        source,
        cyallo::CompileOptions {
            dialect: cyallo::Dialect::Compatibility,
            ..Default::default()
        },
    )
}
