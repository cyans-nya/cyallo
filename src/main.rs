mod report;
mod tracer;
#[cfg(not(feature = "startup-profile"))]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;
#[cfg(feature = "startup-profile")]
#[global_allocator]
static ALLOCATOR: cyallo_startup_profile::Counting<mimalloc::MiMalloc> =
    cyallo_startup_profile::Counting(mimalloc::MiMalloc);
use clap::{Parser, Subcommand, ValueEnum};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use cyallo::{
    Diagnostic, Session, Status,
    backend::{
        CraneliftConfig, Executable, NativeBackend, TierPolicy, TieredArtifact, TieredBackend,
    },
    debugger::{Debugger, Mode},
};
use ratatui::{
    prelude::*,
    widgets::{Block, Paragraph, Wrap},
};
use std::{
    io::{self, BufRead, IsTerminal, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
#[derive(Parser)]
#[command(
    version,
    about = "Typed language, CAIE compatibility, bytecode and background tiered JIT"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    #[arg(long, global = true)]
    convenience_builtins: bool,
    #[arg(
        long,
        global = true,
        default_value_t = 0,
        help = "Compiler workers (0: available cores, 1: serial)"
    )]
    jobs: usize,
    #[arg(
        long,
        global = true,
        value_enum,
        help = "Override source dialect (.ya: modern, .psc: CAIE pseudo code, .pss: simplified CAIE pseudo code; otherwise modern)"
    )]
    dialect: Option<Dialect>,
    #[arg(long, global = true, value_enum, default_value = "auto")]
    color: ColorMode,
    #[arg(
        long,
        global = true,
        require_equals = true,
        num_args = 0..=1,
        default_missing_value = "live",
        value_enum,
        help = "Report JIT execution: bare --jit-trace traces with a live view; --jit-trace=summary prints counters once"
    )]
    jit_trace: Option<TraceMode>,
    #[arg(
        long,
        global = true,
        value_enum,
        default_value = "human",
        help = "Format of the final JIT report (json goes to stderr)"
    )]
    jit_trace_format: ReportFormat,
    #[arg(
        long,
        global = true,
        value_enum,
        help = "JIT report style: plain (ASCII), color, or boxed (colors and frames); default follows the terminal"
    )]
    jit_trace_style: Option<ReportStyle>,
    #[arg(
        long,
        global = true,
        help = "Disable routine inlining in native code; compare against the default"
    )]
    jit_no_inline: bool,
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "Write the final JIT report to PATH instead of stderr (needs --jit-trace or --jit-trace=summary)"
    )]
    jit_trace_out: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        help = "Print bounded compiler notices at execution safepoints"
    )]
    jit_notices: bool,
    #[arg(
        long,
        global = true,
        help = "Warn when a forced routine attribute cannot be honored"
    )]
    warn_unfulfilled_attributes: bool,
    #[arg(
        long,
        global = true,
        value_enum,
        value_delimiter = ',',
        requires = "emit_dir"
    )]
    emit: Vec<EmitKind>,
    #[arg(
        long,
        global = true,
        requires = "emit",
        help = "Directory for emitted IR and bytecode"
    )]
    emit_dir: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        help = "Enable the experimental moving cell collector"
    )]
    gc: bool,
    #[arg(
        long,
        global = true,
        help = "Collect before every cell allocation (implies --gc)"
    )]
    gc_stress: bool,
}
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum TraceMode {
    /// Full trace with a live status block while the program runs.
    Live,
    /// Counter-based summary printed once at exit.
    Summary,
}
#[derive(Clone, Copy, ValueEnum)]
enum ReportFormat {
    Human,
    Json,
}
#[derive(Clone, Copy, ValueEnum)]
enum ReportStyle {
    Plain,
    Color,
    Boxed,
}
#[derive(Clone, Copy, ValueEnum)]
enum Dialect {
    Modern,
    Compatibility,
    #[value(alias = "caie")]
    Strict,
    Simplified,
}
#[derive(Subcommand)]
enum Command {
    /// Inspect bytecode, verified scalar bytecode, or the Winch Wasm adapter.
    Bytecode {
        file: PathBuf,
        #[arg(long)]
        wasm: Option<usize>,
        /// Verify and disassemble the linked routine and its direct callees.
        #[arg(long, value_name = "ROUTINE_ID", conflicts_with = "wasm")]
        verified: Option<usize>,
    },
    /// Start a persistent interactive session (modern syntax by default).
    Repl {
        #[arg(long, value_enum, default_value = "jit")]
        engine: Engine,
        #[arg(long, default_value_t = 1_000_000, value_parser = clap::value_parser!(u64).range(1..))]
        max_instructions: u64,
    },
    /// Print source with syntax colors, preserving its original formatting.
    Pretty {
        file: PathBuf,
    },
    Check {
        file: PathBuf,
    },
    Run {
        file: PathBuf,
        #[arg(long, value_enum, default_value = "jit")]
        engine: Engine,
    },
    Debug {
        file: PathBuf,
    },
    /// Browse a JIT trace written with --jit-trace --jit-trace-format=json.
    Trace {
        file: PathBuf,
    },
}
impl Command {
    fn file(&self) -> Option<&Path> {
        match self {
            Self::Bytecode { file, .. }
            | Self::Pretty { file }
            | Self::Check { file }
            | Self::Run { file, .. }
            | Self::Debug { file }
            | Self::Trace { file } => Some(file),
            Self::Repl { .. } => None,
        }
    }
}
#[derive(Clone, Copy, ValueEnum)]
enum Engine {
    #[value(alias = "tiered")]
    Jit,
    Interpreter,
    /// The verified bytecode interpreter. Programs it cannot lower are refused.
    Verified,
    /// Eager per-routine Cranelift compilation with low-latency settings.
    Cranelift,
    /// Eager per-routine Cranelift compilation optimized for execution speed.
    #[value(name = "cranelift_speed", alias = "cranelift-speed")]
    CraneliftSpeed,
}
#[derive(Clone, Copy, ValueEnum)]
enum ColorMode {
    Auto,
    Always,
    Never,
}
#[derive(Clone, Copy, ValueEnum)]
enum EmitKind {
    Ir,
    Bytecode,
    Clif,
}
fn use_color(mode: ColorMode, terminal: bool) -> bool {
    match mode {
        ColorMode::Always => true,
        ColorMode::Never => false,
        ColorMode::Auto => {
            terminal
                && std::env::var_os("NO_COLOR").is_none()
                && std::env::var_os("TERM").is_none_or(|term| term != "dumb")
        }
    }
}
enum Compiled {
    Direct(Box<dyn Executable>),
    Tiered(Box<TieredArtifact>),
}
impl Compiled {
    fn run(
        &self,
        session: &mut Session,
        budget: usize,
    ) -> Result<(), cyallo::runtime::EngineError> {
        match self {
            Self::Direct(unit) => unit.run(session, budget),
            Self::Tiered(unit) => unit.run(session, budget),
        }
    }
}
fn compile_engine(
    engine: Engine,
    program: Arc<cyallo::ir::Program>,
    reporting: Option<Instant>,
    inlining: bool,
) -> color_eyre::Result<Option<Compiled>> {
    Ok(match engine {
        Engine::Interpreter | Engine::Verified => None,
        Engine::Cranelift => Some(Compiled::Direct(
            CraneliftConfig {
                inlining,
                ..CraneliftConfig::fast()
            }
            .compile(program)
            .map(Box::new)
            .map_err(|e| color_eyre::eyre::eyre!(e))?,
        )),
        Engine::CraneliftSpeed => Some(Compiled::Direct(
            CraneliftConfig {
                inlining,
                ..CraneliftConfig::speed()
            }
            .compile(program)
            .map(Box::new)
            .map_err(|e| color_eyre::eyre::eyre!(e))?,
        )),
        Engine::Jit => {
            let backend = TieredBackend::new(TierPolicy {
                inlining,
                ..TierPolicy::default()
            });
            let artifact = reporting
                .map_or_else(
                    || backend.compile(program.clone()),
                    |origin| backend.compile_with_reporting(program.clone(), origin),
                )
                .map_err(|e| color_eyre::eyre::eyre!(e))?;
            Some(Compiled::Tiered(Box::new(artifact)))
        }
    })
}
fn main() -> color_eyre::Result<()> {
    cyallo_startup_profile::init();
    color_eyre::install()?;
    let cli = Cli::parse();
    let stderr_tty = io::stderr().is_terminal();
    let color = use_color(cli.color, stderr_tty);
    let look = match cli.jit_trace_style {
        Some(ReportStyle::Plain) => report::Look::plain(),
        Some(ReportStyle::Color) => report::Look::color(color),
        Some(ReportStyle::Boxed) => report::Look::boxed(color),
        None if color => report::Look::color(true),
        None => report::Look::plain(),
    };
    let jit_report = cli.jit_trace.is_some();
    if cli.jit_trace_out.is_some() && !jit_report {
        return Err(color_eyre::eyre::eyre!(
            "--jit-trace-out needs --jit-trace or --jit-trace=summary"
        ));
    }
    let jit_trace = cli.jit_trace == Some(TraceMode::Live);
    let runtime = RuntimeOptions {
        jit_report,
        jit_trace,
        // The live block is human output; JSON keeps stderr for the document.
        jit_live: jit_trace
            && stderr_tty
            && matches!(cli.jit_trace_format, ReportFormat::Human)
            && std::env::var_os("TERM").is_none_or(|term| term != "dumb"),
        jit_no_inline: cli.jit_no_inline,
        jit_trace_out: cli.jit_trace_out.clone(),
        report: report::Output {
            format: match cli.jit_trace_format {
                ReportFormat::Human => report::Format::Human,
                ReportFormat::Json => report::Format::Json,
            },
            look,
            inlining: !cli.jit_no_inline,
        },
        jit_notices: cli.jit_notices,
        inspection: {
            let options = cyallo::inspection::InspectionOptions {
                full_trace: jit_trace,
                notices: jit_report || cli.jit_notices,
                warn_unfulfilled: cli.warn_unfulfilled_attributes,
                emit_ir: cli.emit.iter().any(|v| matches!(v, EmitKind::Ir)),
                emit_bytecode: cli.emit.iter().any(|v| matches!(v, EmitKind::Bytecode)),
                emit_clif: cli.emit.iter().any(|v| matches!(v, EmitKind::Clif)),
                directory: cli.emit_dir.clone(),
            };
            (options.notices || options.warn_unfulfilled || !cli.emit.is_empty())
                .then(|| Arc::new(cyallo::inspection::Inspection::new(options)))
        },
        gc: cyallo::runtime::gc::GcConfig {
            enabled: cli.gc || cli.gc_stress,
            stress: cli.gc_stress,
            ..Default::default()
        },
    };
    let diagnostic_color = use_color(cli.color, io::stderr().is_terminal());
    let dialect =
        cli.dialect
            .unwrap_or_else(|| match cli.command.file().and_then(Path::extension) {
                Some(extension) if extension == "psc" => Dialect::Compatibility,
                _ => Dialect::Modern,
            });
    let options = cyallo::CompileOptions {
        convenience_builtins: cli.convenience_builtins,
        jobs: cli.jobs,
        dialect: match dialect {
            Dialect::Modern => cyallo::Dialect::Modern,
            Dialect::Compatibility => cyallo::Dialect::Compatibility,
            Dialect::Strict => cyallo::Dialect::Strict,
            Dialect::Simplified => cyallo::Dialect::Simplified,
        },
    };
    match &cli.command {
        Command::Repl {
            engine,
            max_instructions,
        } => {
            return repl_cli(options, *engine, *max_instructions, cli.color, runtime);
        }
        Command::Pretty { file } => {
            let source = std::fs::read_to_string(file)?;
            match cyallo::highlight::highlight_with_dialect(
                &source,
                use_color(cli.color, io::stdout().is_terminal()),
                options.dialect,
            ) {
                Ok(text) => io::stdout().write_all(text.as_bytes())?,
                Err(d) => {
                    eprintln!(
                        "{}",
                        cyallo::diagnostics::render_with_color_and_dialect(
                            &file.to_string_lossy(),
                            &source,
                            &d,
                            diagnostic_color,
                            options.dialect,
                        )
                    );
                    std::process::exit(1);
                }
            }
            return Ok(());
        }
        Command::Trace { file } => return tracer::run(file),
        _ => {}
    }
    let file = cli.command.file().expect("non-file commands handled above");
    let source_name = file.to_string_lossy().into_owned();
    let frontend_started = Instant::now();
    let mut text = std::fs::read_to_string(file)?;
    let mut source_ranges = vec![];
    let deferred = matches!(
        cli.command,
        Command::Run {
            engine: Engine::Jit | Engine::Interpreter,
            ..
        } | Command::Bytecode {
            verified: Some(_),
            ..
        }
    );
    let frontend_result = if options.dialect == cyallo::Dialect::Modern {
        let loaded = cyallo::module_loader::ModuleLoader::for_entry(file)
            .and_then(|loader| {
                loader
                    .load_parsed(file, cyallo::Jobs(options.jobs))
                    .map(|sources| (loader, sources))
            })
            .map_err(cyallo::module_loader::module_load_diagnostic);
        match loaded {
            Ok((loader, sources)) => {
                (text, source_ranges) = cyallo::modules::parsed_source_context(&sources);
                cyallo::modules::compile_parsed_modules(
                    sources.len() - 1,
                    &sources,
                    options,
                    deferred,
                )
                .map(|program| program.with_module_loader(loader))
            }
            Err(error) => Err(error),
        }
    } else if deferred {
        cyallo::compile_for_execution(&text, options)
    } else {
        cyallo::compile_with_options(&text, options)
    };
    let program = match frontend_result {
        Ok(p) => Arc::new(if let Some(observer) = &runtime.inspection {
            p.with_inspection(observer.clone())
        } else {
            p
        }),
        Err(d) => {
            let located = cyallo::modules::diagnostic_context(&text, &source_ranges, &d);
            let (error_name, error_source, error_diagnostic) = located
                .as_ref()
                .map(|(n, s, d)| (*n, *s, d))
                .unwrap_or((&source_name, &text, &d));
            eprintln!(
                "{}",
                cyallo::diagnostics::render_with_color_and_dialect(
                    error_name,
                    error_source,
                    error_diagnostic,
                    diagnostic_color,
                    options.dialect,
                )
            );
            std::process::exit(1);
        }
    };
    let frontend = frontend_started.elapsed();
    if !matches!(cli.command, Command::Run { .. }) {
        program.emit_inspection();
        check_emission(&program)?;
    }
    match cli.command {
        Command::Bytecode { wasm, verified, .. } => {
            if let Some(routine) = verified {
                let bytecode =
                    cyallo::bytecode::verified::VerifiedModule::lower(&program, routine)?;
                print!("{}", bytecode.disassemble());
            } else if let Some(function) = wasm {
                let bytecode = program
                    .bytecode()
                    .functions
                    .get(function)
                    .ok_or_else(|| color_eyre::eyre::eyre!("unknown function"))?;
                print!(
                    "{}",
                    bytecode.wasm_text(function, &program.functions()[function].registers)
                );
            } else {
                print!("{}", program.bytecode().disassemble());
            }
        }
        Command::Check { .. } => println!(
            "OK: {}",
            match dialect {
                Dialect::Modern => "modern",
                Dialect::Compatibility => "CAIE 9618 compatibility (2027–2029)",
                Dialect::Strict => "CAIE compatibility (strict alias)",
                Dialect::Simplified => "simplified (fixed inferred types)",
            }
        ),
        Command::Run {
            engine: Engine::Verified,
            ..
        } => run_verified(&program, runtime.gc.stress)?,
        Command::Run { engine, .. } => {
            if !run(
                program,
                engine,
                &source_name,
                diagnostic_color,
                options.dialect,
                runtime,
                frontend,
            )? {
                std::process::exit(1);
            }
        }
        Command::Debug { .. } => {
            let session = match Session::with_context(program.clone(), runtime.context()) {
                Ok(session) => session,
                Err(d) => {
                    eprintln!(
                        "{}",
                        cyallo::diagnostics::render_program(
                            &source_name,
                            &program,
                            &d,
                            diagnostic_color,
                            options.dialect,
                        )
                    );
                    std::process::exit(1);
                }
            };
            tui(Debugger::new(session))?;
        }
        _ => unreachable!("non-file commands handled above"),
    }

    Ok(())
}
/// Instructions per slice of a verified run.
const VERIFIED_SLICE: usize = 1_000_000;
/// Entry routine of a linked program.
const ENTRY_ROUTINE: usize = 0;
/// Fuel and call depth of a verified run.
const VERIFIED_LIMITS: cyallo::bytecode::verified::ExecutionLimits =
    cyallo::bytecode::verified::ExecutionLimits {
        fuel: usize::MAX,
        frames: 10_000,
    };
/// Run the entry routine in the verified interpreter. A program that uses an
/// operation it cannot execute stops before running, with the reason.
fn run_verified(program: &cyallo::ir::Program, gc_stress: bool) -> color_eyre::Result<()> {
    use cyallo::bytecode::verified::ExecutionStatus;
    let module = cyallo::bytecode::verified::VerifiedModule::lower(program, ENTRY_ROUTINE)?;
    let mut activation = module
        .start(&[], VERIFIED_LIMITS)
        .map_err(|error| color_eyre::eyre::eyre!(error))?;
    activation.set_gc_stress(gc_stress);
    loop {
        match activation.resume(VERIFIED_SLICE) {
            ExecutionStatus::Paused(_) => {}
            ExecutionStatus::Returned(_) => return Ok(()),
            ExecutionStatus::Failed(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    }
}
/// Instruction budget per run slice while the live view is on.
const LIVE_SLICE: usize = 50_000;
#[derive(Clone)]
struct RuntimeOptions {
    /// Any JIT report (`--jit-trace`, `--jit-trace=summary`).
    jit_report: bool,
    /// Full observation with per-invocation events (`--jit-trace`).
    jit_trace: bool,
    /// Redraw the live status block on stderr (trace mode on a terminal).
    jit_live: bool,
    jit_no_inline: bool,
    /// Where the final report goes; stderr when absent.
    jit_trace_out: Option<PathBuf>,
    report: report::Output,
    jit_notices: bool,
    inspection: Option<Arc<cyallo::inspection::Inspection>>,
    gc: cyallo::runtime::gc::GcConfig,
}
impl RuntimeOptions {
    fn context(&self) -> cyallo::Context {
        cyallo::Context {
            gc: self.gc,
            ..Default::default()
        }
    }
}
fn run(
    program: Arc<cyallo::ir::Program>,
    engine: Engine,
    source_name: &str,
    diagnostic_color: bool,
    dialect: cyallo::Dialect,
    runtime: RuntimeOptions,
    frontend: Duration,
) -> color_eyre::Result<bool> {
    let jit_report = runtime.jit_report;
    let origin = Instant::now();
    let artifact = compile_engine(
        engine,
        program.clone(),
        jit_report.then_some(origin),
        !runtime.jit_no_inline,
    )?;
    let mut timings = report::Timings {
        frontend: Some(frontend),
        backend: origin.elapsed(),
        ..Default::default()
    };
    let setup = Instant::now();
    let mut session = match Session::with_context(program.clone(), runtime.context()) {
        Ok(session) => session,
        Err(d) => {
            eprintln!(
                "{}",
                cyallo::diagnostics::render_program(
                    source_name,
                    &program,
                    &d,
                    diagnostic_color,
                    dialect,
                )
            );
            return Ok(false);
        }
    };
    timings.session = setup.elapsed();
    if jit_report {
        timings.counters = report::Counters::capture(&session);
    }
    if runtime.jit_trace {
        session.enable_reporting(origin);
    }
    let mut written = 0;
    let mut notice_cursor = 0;
    let style = runtime.report.look;
    let mut live = report::Live::new(runtime.jit_live);
    // Live refreshes happen between slices, so slices are kept short; the
    // budget only changes where the scheduler pauses, not what runs.
    let slice = if runtime.jit_live {
        LIVE_SLICE
    } else {
        1_000_000
    };
    let success = loop {
        let execution = Instant::now();
        if let Some(artifact) = &artifact {
            artifact
                .run(&mut session, slice)
                .map_err(|e| color_eyre::eyre::eyre!(e))?;
        } else {
            session.run(slice);
        }
        timings.execution += execution.elapsed();
        let output_pending = session.context.output.len() > written;
        let notices_pending = runtime.jit_notices && has_new_notices(&program, notice_cursor);
        if output_pending || notices_pending {
            live.suspend();
        }
        if runtime.jit_notices {
            print_live_notices(&program, &mut notice_cursor);
        }
        if output_pending {
            print!("{}", &session.context.output[written..]);
            written = session.context.output.len();
        }
        io::stdout().flush()?;
        if output_pending {
            cyallo_startup_profile::mark_first_output();
        }
        live.refresh(output_pending, || {
            report::live_frame(&artifact, &program, &session, origin, &timings, style)
        });
        match session.status() {
            Status::Paused => {}
            Status::Waiting => session.wait_for_wakeup(Duration::from_millis(50)),
            Status::WaitingForInput => {
                live.suspend();
                let mut line = String::new();
                let waiting = Instant::now();
                let bytes = io::stdin().read_line(&mut line)?;
                timings.input_wait += waiting.elapsed();
                if bytes == 0 {
                    let d = cyallo::runtime::RuntimeFailure::new(
                        cyallo::runtime::InputError::EndOfInput.into(),
                        session.current_span().unwrap_or_default(),
                    )
                    .diagnostic();
                    eprintln!(
                        "{}",
                        cyallo::diagnostics::render_program(
                            source_name,
                            session.program(),
                            &d,
                            diagnostic_color,
                            dialect,
                        )
                    );
                    break false;
                }
                if line.ends_with('\n') {
                    line.pop();
                    if line.ends_with('\r') {
                        line.pop();
                    }
                }
                session.context.input.push_back(line);
            }
            Status::Finished => break true,
            Status::Failed(d) => {
                eprintln!(
                    "{}",
                    cyallo::diagnostics::render_program(
                        source_name,
                        session.program(),
                        &d.diagnostic(),
                        diagnostic_color,
                        dialect,
                    )
                );
                break false;
            }
        }
    };
    live.finish();
    cyallo_startup_profile::finish();
    let program = session.program().clone();
    finish_inspection(&artifact, &program)?;
    if runtime.jit_notices {
        print_live_notices(&program, &mut notice_cursor);
    }
    if jit_report {
        timings.wall = origin.elapsed();
        emit_report(
            &report::render(
                &artifact,
                &program,
                &session,
                engine,
                &timings,
                runtime.report,
            ),
            runtime.jit_trace_out.as_deref(),
        )?;
    }
    if !jit_report
        && !runtime.jit_notices
        && let Some(observer) = program.inspection()
    {
        eprint!("{}", observer.render());
    }
    check_emission(&program)?;
    Ok(success)
}
fn check_emission(program: &cyallo::ir::Program) -> color_eyre::Result<()> {
    if let Some(observer) = program.inspection() {
        let errors = observer.errors();
        if !errors.is_empty() {
            return Err(color_eyre::eyre::eyre!(
                "failed to emit compiler inspection files: {}",
                errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
    }
    Ok(())
}
fn finish_inspection(
    artifact: &Option<Compiled>,
    program: &cyallo::ir::Program,
) -> color_eyre::Result<()> {
    // A requested CLIF pair must survive process exit even when compilation
    // completed after the last VM instruction. This only drains existing jobs.
    if program.inspection().is_some_and(|o| o.options.emit_clif)
        && let Some(Compiled::Tiered(artifact)) = artifact
        && !artifact.wait_for_compilation(Duration::from_secs(30))
    {
        return Err(color_eyre::eyre::eyre!(
            "timed out finalizing compiler inspection"
        ));
    }
    program.emit_inspection();
    check_emission(program)
}
/// Write the final report to the requested file, or to stderr.
fn emit_report(text: &str, file: Option<&Path>) -> color_eyre::Result<()> {
    match file {
        Some(path) => std::fs::write(path, text)
            .map_err(|error| color_eyre::eyre::eyre!("cannot write {}: {error}", path.display()))?,
        None => eprint!("{text}"),
    }
    Ok(())
}
fn has_new_notices(program: &cyallo::ir::Program, cursor: usize) -> bool {
    program
        .inspection()
        .is_some_and(|observer| observer.notices().len() > cursor)
}
fn print_live_notices(program: &cyallo::ir::Program, cursor: &mut usize) {
    if let Some(observer) = program.inspection() {
        let notices = observer.notices();
        for event in notices.iter().skip(*cursor) {
            eprintln!("[{}] {}: {}", event.category, event.routine, event.message);
        }
        *cursor = notices.len();
    }
}
struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        ratatui::restore();
    }
}
fn tui(mut debugger: Debugger) -> color_eyre::Result<()> {
    let _guard = TerminalGuard;
    let mut terminal = ratatui::try_init()?;
    let mut input = String::new();
    let mut cursor = 1usize;
    let mut follow = true;
    let mut dirty = true;
    loop {
        if debugger.mode != Mode::Paused {
            dirty = true;
        }
        debugger.tick(1000);
        if follow {
            cursor = debugger.line().unwrap_or(cursor);
        }
        if dirty {
            terminal.draw(|frame| draw(frame, &debugger, &input, cursor))?;
            dirty = false;
        }
        if !event::poll(Duration::from_millis(16))? {
            continue;
        }
        let event = event::read()?;
        dirty = true;
        if let Event::Key(key) = event {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if *debugger.session.status() == Status::WaitingForInput {
                match key.code {
                    KeyCode::Enter => {
                        debugger
                            .session
                            .context
                            .input
                            .push_back(std::mem::take(&mut input));
                        debugger.command(Mode::Continue);
                    }
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Esc => break,
                    KeyCode::Char(c) => input.push(c),
                    _ => {}
                }
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('c') => {
                    follow = true;
                    debugger.command(Mode::Continue);
                }
                KeyCode::Char('p') => debugger.command(Mode::Paused),
                KeyCode::Char('s') => {
                    follow = true;
                    debugger.command(Mode::Into);
                }
                KeyCode::Char('n') => {
                    follow = true;
                    debugger.command(Mode::Over(debugger.session.depth()));
                }
                KeyCode::Char('o') => {
                    follow = true;
                    debugger.command(Mode::Out(debugger.session.depth()));
                }
                KeyCode::Char('r') => {
                    debugger.restart()?;
                    follow = true;
                }
                KeyCode::Up => {
                    cursor = cursor.saturating_sub(1).max(1);
                    follow = false;
                }
                KeyCode::Down => {
                    cursor = (cursor + 1)
                        .min(debugger.session.program().source().lines().count().max(1));
                    follow = false;
                }
                KeyCode::Char('b') => {
                    if !debugger.breakpoints.remove(&cursor) {
                        debugger.breakpoints.insert(cursor);
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}
fn draw(frame: &mut Frame, debugger: &Debugger, input: &str, cursor: usize) {
    let [body, output, footer] = Layout::vertical([
        Constraint::Min(5),
        Constraint::Length(7),
        Constraint::Length(4),
    ])
    .areas(frame.area());
    let [source, inspect] =
        Layout::horizontal([Constraint::Percentage(65), Constraint::Percentage(35)]).areas(body);
    let current = debugger.line();
    let height = source.height.saturating_sub(2) as usize;
    let first = cursor.saturating_sub(height / 2 + 1);
    let lines: Vec<Line> = debugger
        .session
        .program()
        .source()
        .lines()
        .enumerate()
        .skip(first)
        .take(height)
        .map(|(i, text)| {
            let number = i + 1;
            let marker = if current == Some(number) { ">" } else { " " };
            let breakpoint = if debugger.breakpoints.contains(&number) {
                "●"
            } else {
                " "
            };
            let style = if number == cursor {
                Style::default().bg(Color::DarkGray)
            } else {
                Style::default()
            };
            Line::styled(format!("{marker}{breakpoint}{number:4} {text}"), style)
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title("Source")),
        source,
    );
    let mut state = debugger
        .session
        .stack()
        .into_iter()
        .map(|s| format!("  {s}"))
        .collect::<Vec<_>>();
    state.push(String::new());
    state.extend(
        debugger
            .session
            .variables()
            .into_iter()
            .map(|(n, v)| format!("{n} = {v}")),
    );
    frame.render_widget(
        Paragraph::new(state.join("\n"))
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title("Stack / variables")),
        inspect,
    );
    let mut text = debugger
        .session
        .context
        .output
        .lines()
        .rev()
        .take(4)
        .collect::<Vec<_>>();
    text.reverse();
    let mut text = text.join("\n");
    if *debugger.session.status() == Status::WaitingForInput {
        text.push_str(&format!("\nINPUT > {input}▏"));
    }
    frame.render_widget(
        Paragraph::new(text).block(Block::bordered().title("Program I/O")),
        output,
    );
    let status = match debugger.session.status() {
        Status::Waiting => "Waiting for runtime".into(),
        Status::WaitingForInput => "Waiting for input".into(),
        Status::Finished => "Finished".into(),
        Status::Failed(failure) => format!("{}: {}", failure.error.code(), failure.error),
        Status::Paused => match debugger.mode {
            Mode::Paused => "Paused",
            Mode::Continue => "Running",
            _ => "Stepping",
        }
        .into(),
    };
    frame.render_widget(
        Paragraph::new(format!(
            "{status}\nc continue · p pause · s/n/o step · b break · r restart · q quit · ↑↓"
        ))
        .block(Block::bordered()),
        footer,
    );
}

fn repl_cli(
    options: cyallo::CompileOptions,
    engine: Engine,
    max_instructions: u64,
    color_mode: ColorMode,
    runtime: RuntimeOptions,
) -> color_eyre::Result<()> {
    use cyallo::repl::{Repl, Submission};
    let jit_report = runtime.jit_report;
    let dialect = options.dialect;
    let mut repl = Repl::with_context(options, runtime.context())?;
    repl.set_inspection(runtime.inspection.clone());
    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    let color = use_color(color_mode, io::stdout().is_terminal());
    let diagnostic_color = use_color(color_mode, io::stderr().is_terminal());
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut output = io::stdout().lock();
    let mut written = 0;
    if interactive {
        writeln!(output, "CAIE REPL — :help for commands; :quit to exit")?;
    }
    loop {
        if interactive {
            let prompt = if repl.is_pending() { "... " } else { ">>> " };
            if color {
                write!(output, "\x1b[36m{prompt}\x1b[0m")?;
            } else {
                write!(output, "{prompt}")?;
            }
            output.flush()?;
        }
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            if repl.is_pending() {
                let d = Diagnostic::new(
                    "E203",
                    "incomplete REPL entry",
                    cyallo::Span::empty(repl.source().len() as u32),
                );
                eprintln!(
                    "{}",
                    cyallo::diagnostics::render_with_color_and_dialect(
                        "<repl>",
                        repl.source(),
                        &d,
                        diagnostic_color,
                        dialect,
                    )
                );
            }
            break;
        }
        let command = line.trim();
        if command.starts_with(':') {
            match command {
                ":quit" | ":q" => break,
                ":help" => writeln!(
                    output,
                    ":help  :vars  :reset  :cancel  :quit\n{}",
                    if options.dialect == cyallo::Dialect::Modern {
                        "Enter modern code or an expression; close blocks with }. Use std::runtime::caie::input(&mut target) for input."
                    } else {
                        "Enter CAIE pseudocode or an expression; close blocks with END keywords."
                    }
                )?,
                ":vars" => {
                    for (name, value) in repl.session().globals() {
                        writeln!(output, "{name} = {value}")?;
                    }
                }
                ":reset" => {
                    repl.reset()?;
                    written = 0;
                }
                ":cancel" => repl.discard_pending(),
                _ => writeln!(output, "Unknown command. Use :help.")?,
            }
            output.flush()?;
            continue;
        }
        let frontend_started = Instant::now();
        match repl.push_line(&line) {
            Ok(Submission::Incomplete) => continue,
            Err(d) => {
                eprintln!(
                    "{}",
                    cyallo::diagnostics::render_with_color_and_dialect(
                        "<repl>",
                        repl.source(),
                        &d,
                        diagnostic_color,
                        dialect,
                    )
                );
                continue;
            }
            Ok(Submission::Ready) => {}
        }
        let frontend = frontend_started.elapsed();
        let origin = Instant::now();
        let artifact = compile_engine(
            engine,
            repl.session().program().clone(),
            jit_report.then_some(origin),
            !runtime.jit_no_inline,
        )?;
        let mut timings = report::Timings {
            frontend: Some(frontend),
            backend: origin.elapsed(),
            counters: if jit_report {
                report::Counters::capture(repl.session())
            } else {
                Default::default()
            },
            ..Default::default()
        };
        if runtime.jit_trace {
            repl.session_mut().enable_reporting(origin);
        }
        let initial_count = repl.session().instruction_count();
        let mut notice_cursor = 0;
        let style = runtime.report.look;
        let mut live = report::Live::new(runtime.jit_live);
        loop {
            let execution = Instant::now();
            let remaining =
                max_instructions.saturating_sub(repl.session().instruction_count() - initial_count);
            if remaining == 0 {
                repl.interrupt();
            } else if let Some(artifact) = &artifact {
                artifact
                    .run(repl.session_mut(), remaining.min(10_000) as usize)
                    .map_err(|e| color_eyre::eyre::eyre!(e))?;
            } else {
                repl.run(remaining.min(10_000) as usize);
            }
            timings.execution += execution.elapsed();
            let output_pending = repl.session().context.output.len() > written;
            let notices_pending =
                runtime.jit_notices && has_new_notices(repl.session().program(), notice_cursor);
            if output_pending || notices_pending {
                live.suspend();
            }
            if runtime.jit_notices {
                print_live_notices(repl.session().program(), &mut notice_cursor);
            }
            if output_pending {
                output.write_all(&repl.session().context.output.as_bytes()[written..])?;
                written = repl.session().context.output.len();
            }
            output.flush()?;
            live.refresh(output_pending, || {
                report::live_frame(
                    &artifact,
                    repl.session().program(),
                    repl.session(),
                    origin,
                    &timings,
                    style,
                )
            });
            match repl.session().status() {
                Status::Waiting => repl.session().wait_for_wakeup(Duration::from_millis(50)),
                Status::Paused => continue,
                Status::Finished => break,
                Status::Failed(d) => {
                    eprintln!(
                        "{}",
                        cyallo::diagnostics::render_with_color_and_dialect(
                            "<repl>",
                            repl.source(),
                            &d.diagnostic(),
                            diagnostic_color,
                            dialect,
                        )
                    );
                    break;
                }
                Status::WaitingForInput => {
                    live.suspend();
                    if interactive {
                        write!(output, "input> ")?;
                        output.flush()?;
                    }
                    let mut value = String::new();
                    let waiting = Instant::now();
                    let bytes = input.read_line(&mut value)?;
                    timings.input_wait += waiting.elapsed();
                    if bytes == 0 {
                        let d = cyallo::runtime::RuntimeFailure::new(
                            cyallo::runtime::InputError::EndOfInput.into(),
                            repl.session().current_span().unwrap_or_default(),
                        )
                        .diagnostic();
                        eprintln!(
                            "{}",
                            cyallo::diagnostics::render_with_color_and_dialect(
                                "<repl>",
                                repl.source(),
                                &d,
                                diagnostic_color,
                                dialect,
                            )
                        );
                        live.finish();
                        finish_inspection(&artifact, repl.session().program())?;
                        if jit_report {
                            timings.wall = origin.elapsed();
                            emit_report(
                                &report::render(
                                    &artifact,
                                    repl.session().program(),
                                    repl.session(),
                                    engine,
                                    &timings,
                                    runtime.report,
                                ),
                                runtime.jit_trace_out.as_deref(),
                            )?;
                        }
                        return Ok(());
                    }
                    if value.ends_with('\n') {
                        value.pop();
                        if value.ends_with('\r') {
                            value.pop();
                        }
                    }
                    repl.session_mut().context.input.push_back(value);
                }
            }
        }
        live.finish();
        finish_inspection(&artifact, repl.session().program())?;
        if jit_report {
            timings.wall = origin.elapsed();
            emit_report(
                &report::render(
                    &artifact,
                    repl.session().program(),
                    repl.session(),
                    engine,
                    &timings,
                    runtime.report,
                ),
                runtime.jit_trace_out.as_deref(),
            )?;
        }
        check_emission(repl.session().program())?;
        if runtime.jit_notices {
            print_live_notices(repl.session().program(), &mut notice_cursor);
        } else if !jit_report && let Some(observer) = repl.session().program().inspection() {
            eprint!("{}", observer.render());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn debugger_renders_source_and_variables() {
        let p = Arc::new(cyallo::compile("let mut x = 0\nx = 1\nstd::io::print(x)\n").unwrap());
        let d = Debugger::new(Session::new(p).unwrap());
        let mut t = Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        t.draw(|f| draw(f, &d, "", 1)).unwrap();
        let b = t.backend().buffer();
        let text = (0..b.area.height)
            .map(|y| {
                (0..b.area.width)
                    .map(|x| b[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<String>();
        assert!(text.contains("let mut x"));
        assert!(text.contains("uninitialized"));
        assert!(text.contains("c continue"));
    }
}
