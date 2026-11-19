//! Program output through the verified interpreter. Expected lines are written
//! by hand from the language contract, not taken from another engine.

use cyallo::{
    CompileOptions, Dialect,
    bytecode::verified::{
        CallTarget, ExecutionLimits, ExecutionStatus, LowerError, OutputSink, RunError,
        VerifiedModule,
    },
    compile, compile_with_options,
};
use std::sync::{Arc, Mutex};

const LIMITS: ExecutionLimits = ExecutionLimits {
    fuel: 10_000,
    frames: 64,
};

fn capture() -> (OutputSink, Arc<Mutex<Vec<String>>>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let lines = lines.clone();
        OutputSink::new(move |line| lines.lock().unwrap().push(line.to_owned()))
    };
    (sink, lines)
}

fn caie(source: &str) -> cyallo::ir::Program {
    compile_with_options(
        source,
        CompileOptions {
            dialect: Dialect::Compatibility,
            ..Default::default()
        },
    )
    .unwrap()
}

#[test]
fn caie_program_prints_scalars_text_and_booleans() {
    let program = caie(concat!(
        "DECLARE total : INTEGER\n",
        "DECLARE i : INTEGER\n",
        "total <- 0\n",
        "FOR i <- 1 TO 10\n",
        "    total <- total + i\n",
        "NEXT i\n",
        "OUTPUT \"total = \", total\n",
        "OUTPUT total > 50\n",
        "OUTPUT total < 50\n",
    ));
    let module = VerifiedModule::lower(&program, 0).unwrap();
    let (sink, lines) = capture();
    assert_eq!(module.run_to(&[], LIMITS, sink), Ok(None));
    assert_eq!(*lines.lock().unwrap(), ["total = 55", "TRUE", "FALSE"]);
}

#[test]
fn caie_recursive_function_output() {
    let program = caie(include_str!("../examples/functions.psc"));
    let module = VerifiedModule::lower(&program, 0).unwrap();
    let (sink, lines) = capture();
    assert_eq!(module.run_to(&[], LIMITS, sink), Ok(None));
    assert_eq!(*lines.lock().unwrap(), ["3628800"]);
}

#[test]
fn modern_print_spells_booleans_in_lowercase() {
    let program = compile("std::io::print(1 + 2)\nstd::io::print(2 > 1)\n").unwrap();
    let module = VerifiedModule::lower(&program, 0).unwrap();
    let (sink, lines) = capture();
    assert_eq!(module.run_to(&[], LIMITS, sink), Ok(None));
    assert_eq!(*lines.lock().unwrap(), ["3", "true"]);
}

#[test]
fn output_is_delivered_once_across_single_instruction_slices() {
    let program = caie("OUTPUT \"a\"\nOUTPUT 7\n");
    let module = VerifiedModule::lower(&program, 0).unwrap();
    let (sink, lines) = capture();
    let mut activation = module.start(&[], LIMITS).unwrap();
    activation.set_output(sink);
    while matches!(activation.resume(1), ExecutionStatus::Paused(_)) {}
    assert_eq!(*lines.lock().unwrap(), ["a", "7"]);
}

#[test]
fn output_stops_at_a_failure_after_earlier_lines() {
    let program = caie("OUTPUT 1\nOUTPUT 1 DIV 0\nOUTPUT 3\n");
    let module = VerifiedModule::lower(&program, 0).unwrap();
    let (sink, lines) = capture();
    assert!(matches!(
        module.run_to(&[], LIMITS, sink),
        Err(RunError::DivisionByZero { .. })
    ));
    assert_eq!(*lines.lock().unwrap(), ["1"]);
}

#[test]
fn output_call_site_is_visible_in_the_disassembly() {
    let program = caie("OUTPUT \"x\", 1\n");
    let module = VerifiedModule::lower(&program, 0).unwrap();
    let body = module.function(module.entry()).unwrap();
    assert!(
        body.calls()
            .iter()
            .any(|call| matches!(call.target, CallTarget::Output(_)))
    );
    assert!(module.disassemble().contains("output.caie"));
}

#[test]
fn text_stored_in_a_variable_is_printed() {
    let program = caie("DECLARE s : STRING\ns <- \"hi\"\nOUTPUT s, \"!\"\n");
    let module = VerifiedModule::lower(&program, 0).unwrap();
    let (sink, lines) = capture();
    assert_eq!(module.run_to(&[], LIMITS, sink), Ok(None));
    assert_eq!(*lines.lock().unwrap(), ["hi!"]);
}

#[test]
fn zero_loop_step_fails_with_its_source_span() {
    let source = "DECLARE i : INTEGER\nFOR i <- 1 TO 3 STEP 0\nOUTPUT i\nNEXT i\n";
    let program = caie(source);
    let module = VerifiedModule::lower(&program, 0).unwrap();
    let (sink, lines) = capture();
    assert!(matches!(
        module.run_to(&[], LIMITS, sink),
        Err(RunError::ZeroStep { .. })
    ));
    assert!(lines.lock().unwrap().is_empty());
}

mod malformed {
    use cyallo::{
        Span,
        bytecode::verified::{
            CallSite, CallSiteId, CallTarget, FORMAT_VERSION, FunctionId, FunctionImage,
            Instruction, InvalidBytecode, ModuleImage, OutputId, OutputPiece, OutputTemplate,
            Register, RegisterType, ScalarType, TextStyle, encode,
        },
    };

    fn image(
        outputs: Vec<OutputTemplate>,
        call: CallSite,
        registers: Vec<RegisterType>,
    ) -> ModuleImage {
        let instructions = [Instruction::Call(CallSiteId(0)), Instruction::Return(None)];
        ModuleImage {
            version: FORMAT_VERSION,
            entry: FunctionId(0),
            shapes: Box::new([]),
            signatures: Box::new([]),
            globals: Box::new([]),
            managed: Box::new([]),
            traits: Box::new([]),
            implementations: Box::new([]),
            functions: Box::new([FunctionImage {
                locals: Box::new([]),
                frozen: Box::new([]),
                contracts: Box::new([]),
                generator: None,
                registers: registers.into(),
                parameters: Box::new([]),
                result: None,
                calls: Box::new([call]),
                lists: Box::new([]),
                texts: Box::new([]),
                outputs: outputs.into(),
                words: encode(instructions),
                spans: Box::new([Span::new(0, 1); 2]),
                statements: Box::new([]),
            }]),
        }
    }

    fn text(pieces: Vec<OutputPiece>) -> OutputTemplate {
        OutputTemplate {
            style: TextStyle::Plain,
            pieces: pieces.into(),
        }
    }

    fn call(output: u32, arguments: Vec<Register>, destination: Option<Register>) -> CallSite {
        CallSite {
            target: CallTarget::Output(OutputId(output)),
            arguments: arguments.into(),
            destination,
        }
    }

    fn kind(image: ModuleImage) -> InvalidBytecode {
        image.verify().unwrap_err().kind
    }

    #[test]
    fn rejects_an_unknown_template() {
        assert_eq!(
            kind(image(vec![], call(0, vec![], None), vec![])),
            InvalidBytecode::UnknownOutput(OutputId(0))
        );
    }

    #[test]
    fn rejects_a_piece_without_an_argument() {
        let template = text(vec![OutputPiece::Argument(0)]);
        assert_eq!(
            kind(image(vec![template], call(0, vec![], None), vec![])),
            InvalidBytecode::OutputArgument {
                output: OutputId(0),
                index: 0
            }
        );
    }

    #[test]
    fn rejects_a_result_destination() {
        let registers = vec![RegisterType::Scalar(ScalarType::I64)];
        assert_eq!(
            kind(image(
                vec![text(vec![])],
                call(0, vec![], Some(Register(0))),
                registers
            )),
            InvalidBytecode::OutputDestination
        );
    }
}
