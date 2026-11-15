//! Reproducible grammar-based differential fuzzing. Failures include the exact
//! seed and Cyallo program; no ambient random state or external services.
use cyallo::{
    CompileOptions, Context, Session, Status,
    backend::{
        CraneliftBackend, CraneliftSpeedBackend, Executable, NativeBackend, Tier, TieredBackend,
    },
    runtime::{ObjectIcMode, ObjectStats},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

struct Rng(u64);
fn native_engines(program: &Arc<cyallo::ir::Program>) -> Vec<Box<dyn Executable>> {
    let winch = TieredBackend::default().compile(program.clone()).unwrap();
    for function in 0..program.functions().len() {
        winch.precompile(function, Tier::Winch).unwrap();
    }
    assert!(winch.wait_for_compilation(Duration::from_secs(30)));
    vec![
        Box::new(winch),
        Box::new(CraneliftBackend.compile(program.clone()).unwrap()),
        Box::new(CraneliftSpeedBackend.compile(program.clone()).unwrap()),
    ]
}
impl Rng {
    fn next(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}
fn program(seed: u64) -> String {
    let mut rng = Rng(seed);
    let mut source = String::from(
        r#"
type Bag=object {value:i64,..}
proc get(b:Bag)->i64 {return b.value}
proc put(b:Bag,value:i64) {b.value=value}
proc keyed(b:Bag,key:string)->dynamic {return b[key]}
proc write_key(b:Bag,value:dynamic) {b["value"]=value}
fn twice(value:i64)->i64 {return value*2}
let raw=dynamic {prefix:1,value:7}
let b0=std::object::view<Bag>(raw).unwrap()
let b1=object {value:11}
let alias=b0
for i in 0..8 {put(b0,i); std::io::print(get(b0)); std::io::print(keyed(b0,"value")); write_key(b1,i)}
delete raw.prefix
std::io::print(get(alias))
"#,
    );
    // One call site sees more than four shapes, including offset shifts after
    // deletion and a configurable tail whose representation changes.
    for index in 0..7 {
        source.push_str(&format!("b0.tag{index}={index}\nstd::io::print(get(b0))\n"));
    }
    for step in 0..24 {
        let bag = rng.next(2);
        let field = rng.next(5);
        let value = rng.next(200) as i64 - 100;
        source.push_str(&match rng.next(10) {
            0 => format!("put(b{bag},{value})\n"),
            1 => format!("b{bag}.extra{field}={value}\n"),
            2 => format!("b{bag}.extra{field}=\"text-{step}\"\n"),
            3 => format!("delete b{bag}.extra{field}\n"),
            4 => format!("b{bag}.owner=rc {value}\n"),
            5 => format!("b{bag}.nested=gc Cell::new({value})\n"),
            6 => {
                let key =
                    ["雪", "", "__proto__", "constructor", "Score", "score"][rng.next(6) as usize];
                format!("b{bag}[\"{key}\"]={value}\nstd::io::print(keyed(b{bag},\"{key}\"))\n")
            }
            7 => format!("b{bag}.callback=twice\nstd::io::print(b{bag}.callback({value}))\n"),
            8 => format!("b{bag}.value+={value}\n"),
            _ => format!("std::io::print(get(b{bag}))\n"),
        });
    }
    source.push_str("std::io::print(get(b0),\":\",get(b1),\":\",get(alias))\n");
    match seed % 4 {
        0 => source.push_str("std::object::freeze(b0)\nput(b0,42)\n"),
        1 => source.push_str("write_key(b0,true)\n"),
        2 => source.push_str("std::io::print(keyed(b0,\"absent\"))\n"),
        _ => {}
    }
    source
}
#[derive(Debug, PartialEq)]
struct Observation {
    output: String,
    status: Status,
    instructions: u64,
}
fn run(
    program: Arc<cyallo::ir::Program>,
    engine: Option<&dyn Executable>,
    mode: ObjectIcMode,
    budget: usize,
) -> (Observation, ObjectStats) {
    let mut session = Session::with_context(
        program,
        Context {
            gc: cyallo::runtime::gc::GcConfig::stress(),
            ..Default::default()
        },
    )
    .unwrap();
    session.set_object_ic_mode(mode);
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(engine) = engine {
            engine.run(&mut session, budget).unwrap();
        } else {
            session.run(budget);
        }
        match session.status() {
            Status::Paused => {}
            Status::Waiting => session.wait_for_wakeup(Duration::from_millis(1)),
            _ => break,
        }
        assert!(Instant::now() < deadline, "{:?}", session.status());
    }
    (
        Observation {
            output: session.context.output.clone(),
            status: session.status().clone(),
            instructions: session.instruction_count(),
        },
        session.object_stats(),
    )
}

#[test]
fn seeded_objects_match_interpreter_with_native_ics_enabled_and_disabled() {
    for seed in 1..=32 {
        let source = program(seed);
        for deferred in [false, true] {
            let program = Arc::new(
                if deferred {
                    cyallo::compile_for_execution(&source, CompileOptions::default())
                } else {
                    cyallo::compile(&source)
                }
                .unwrap_or_else(|error| panic!("seed {seed}: {error}\n{source}")),
            );
            let (expected, _) = run(program.clone(), None, ObjectIcMode::Disabled, 1);
            let engines = native_engines(&program);
            for (index, engine) in engines.iter().enumerate() {
                for mode in [ObjectIcMode::Enabled, ObjectIcMode::Disabled] {
                    let (actual, stats) = run(
                        program.clone(),
                        Some(engine.as_ref()),
                        mode,
                        if seed % 2 == 0 { 1 } else { 137 },
                    );
                    assert_eq!(
                        actual, expected,
                        "seed={seed} deferred={deferred} engine={index} mode={mode:?}\n{source}"
                    );
                    match mode {
                        ObjectIcMode::Enabled => {
                            assert!(stats.native_reads > 0, "{stats:?}");
                            assert!(stats.native_writes > 0, "{stats:?}");
                            assert!(stats.native_misses > 0, "{stats:?}");
                            assert!(stats.megamorphic_misses > 0, "{stats:?}");
                        }
                        ObjectIcMode::Disabled => {
                            assert_eq!(stats.native_reads, 0);
                            assert_eq!(stats.native_writes, 0);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn native_scalar_tags_and_suspended_object_roots_match() {
    let source = r#"
type Scalars=object {n:i64,b:bool,r:f64,c:char,..}
proc show(s:Scalars) {std::io::print(s.n,":",s.b,":",s.r,":",s.c)}
proc change(s:Scalars,n:i64,b:bool,r:f64,c:char) {s.n=n; s.b=b; s.r=r; s.c=c}
proc erased(s:dynamic)->dynamic {return s.n}
proc erased_bool(s:dynamic)->dynamic {return s.b}
proc erased_real(s:dynamic)->dynamic {return s.r}
proc erased_char(s:dynamic)->dynamic {return s.c}
let data=object {n:1,b:true,r:1.5,c:'雪'}
let raw:dynamic=data
let create=gen proc()->Generator<i64,(),()> {let saved=object {n:7,b:false,r:2.5,c:'x'}; for i in 0..4 {yield saved.n}}
let mut g=create()
for i in 0..6 {
    change(data,i,i%2==0,2.5,'λ')
    show(data)
    std::io::print(erased(std::mem::clone(&raw)))
    std::io::print(erased_bool(std::mem::clone(&raw)),":",erased_real(std::mem::clone(&raw)),":",erased_char(std::mem::clone(&raw)))
    if i<4 {std::io::print(g.next().yielded())}
    let garbage=gc Cell::new(i)
}
change(data,-9223372036854775808,false,0.0/0.0,'\0')
for i in 0..3 {show(data); std::io::print(data.r==data.r)}
change(data,9223372036854775807,true,-0.0,'雪')
for i in 0..3 {show(data)}
let small=object {value:3 as i8}
std::io::print(small.value)
"#;
    let program = Arc::new(cyallo::compile(source).unwrap());
    let (expected, _) = run(program.clone(), None, ObjectIcMode::Disabled, 1);
    assert_eq!(expected.status, Status::Finished);
    for engine in native_engines(&program) {
        let (actual, stats) = run(
            program.clone(),
            Some(engine.as_ref()),
            ObjectIcMode::Enabled,
            1,
        );
        assert_eq!(actual, expected);
        assert!(stats.native_reads >= 20, "{stats:?}");
        assert!(stats.native_writes >= 20, "{stats:?}");
    }
}

#[test]
fn object_ics_survive_publication_and_recompiled_snapshots() {
    use std::{
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };
    struct Files(PathBuf);
    impl Drop for Files {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let files = Files(std::env::temp_dir().join(format!(
            "cyallo-ic-publication-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )));
    std::fs::create_dir_all(&files.0).unwrap();
    std::fs::write(files.0.join("main.ya"),r#"
type Bag=object {value:i64,..}
proc read(b:Bag)->i64 {return b.value}
let bag=object {value:1}
let make=gen proc()->Generator<i64,(),()> {let saved=object {value:7}; yield saved.value; yield saved.value}
let mut g=make()
std::io::print(g.next().yielded())
let mut sum=0
for i in 0..8 {sum+=read(bag)}
let module=std::module::load("plugin.ya").unwrap()
for i in 0..8 {sum+=module.number as i64}
std::io::print(module.id(sum))
for i in 0..8 {sum+=module.number as i64}
std::io::print(g.next().yielded(),":",sum)
"#).unwrap();
    let program = Arc::new(
        cyallo::modules::compile_path(&files.0.join("main.ya"), CompileOptions::default(), false)
            .unwrap(),
    );
    std::fs::write(
        files.0.join("plugin.ya"),
        "pub const number=5\npub fn id<T>(value:T)->T {return value}",
    )
    .unwrap();
    let (expected, _) = run(program.clone(), None, ObjectIcMode::Disabled, 1);
    assert_eq!(expected.status, Status::Finished);
    for speed in [false, true] {
        let mut session = Session::with_context(
            program.clone(),
            Context {
                gc: cyallo::runtime::gc::GcConfig::stress(),
                ..Default::default()
            },
        )
        .unwrap();
        let mut snapshot = program.clone();
        let mut engine = if speed {
            CraneliftSpeedBackend.compile(snapshot.clone()).unwrap()
        } else {
            CraneliftBackend.compile(snapshot.clone()).unwrap()
        };
        let mut publications = 0;
        let mut last_reads = 0;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            engine.run(&mut session, 1).unwrap();
            if !Arc::ptr_eq(&snapshot, session.program()) {
                publications += 1;
                last_reads = session.object_stats().native_reads;
                snapshot = session.program().clone();
                engine = if speed {
                    CraneliftSpeedBackend.compile(snapshot.clone()).unwrap()
                } else {
                    CraneliftBackend.compile(snapshot.clone()).unwrap()
                };
            }
            match session.status() {
                Status::Paused => {}
                Status::Waiting => session.wait_for_wakeup(Duration::from_millis(1)),
                _ => break,
            }
            assert!(Instant::now() < deadline);
        }
        assert_eq!(session.status(), &expected.status);
        assert_eq!(session.context.output, expected.output);
        assert_eq!(session.instruction_count(), expected.instructions);
        assert!(publications >= 2);
        assert!(session.object_stats().native_reads > last_reads);
    }
}

#[test]
fn shared_native_artifacts_do_not_share_shape_domains_or_feedback() {
    let source = r#"
type Bag=object {value:i64,..}
proc get(b:Bag)->i64 {return b.value}
let mut choice=""
std::runtime::caie::input(&mut choice)
let mut raw=dynamic {}
if choice=="left" {raw=dynamic {prefix:0,value:1}} else {raw=dynamic {value:2,prefix:0}}
let bag=std::object::view<Bag>(raw).unwrap()
let mut sum=0
for i in 0..8 {sum+=get(bag)}
std::io::print(sum)
"#;
    let program = Arc::new(cyallo::compile(source).unwrap());
    for engine in native_engines(&program) {
        for (input, expected) in [
            ("left", "8\n"),
            ("right", "16\n"),
            ("left", "8\n"),
            ("right", "16\n"),
        ] {
            let mut session = Session::with_context(
                program.clone(),
                Context {
                    input: [input.to_string()].into(),
                    gc: cyallo::runtime::gc::GcConfig::stress(),
                    ..Default::default()
                },
            )
            .unwrap();
            while session.status() == &Status::Paused {
                engine.run(&mut session, 97).unwrap();
            }
            assert_eq!(session.status(), &Status::Finished);
            assert_eq!(session.context.output, expected);
            assert!(session.object_stats().native_reads >= 7);
            assert!(session.object_stats().native_misses > 0);
        }
    }
}
