//! Optional bounded compiler observation. Workers collect data, never print it.
use std::{
    collections::BTreeMap,
    fmt::Write,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
#[derive(Debug, Clone, Default)]
pub struct InspectionOptions {
    pub full_trace: bool,
    pub notices: bool,
    pub warn_unfulfilled: bool,
    pub emit_ir: bool,
    pub emit_bytecode: bool,
    pub emit_clif: bool,
    pub directory: Option<PathBuf>,
}
#[derive(Debug, Clone)]
pub struct Notice {
    pub category: &'static str,
    pub routine: String,
    pub message: String,
    pub count: u64,
}
#[derive(Debug, Default)]
struct Records {
    events: Vec<Notice>,
    index: BTreeMap<(String, String, String), usize>,
    dropped: u64,
    errors: Vec<String>,
}
#[derive(Debug)]
pub struct Inspection {
    pub options: InspectionOptions,
    records: Mutex<Records>,
    next_snapshot: Arc<AtomicU64>,
}
impl Inspection {
    pub fn new(options: InspectionOptions) -> Self {
        Self {
            options,
            records: Mutex::default(),
            next_snapshot: Arc::new(AtomicU64::new(1)),
        }
    }
    pub(crate) fn next_snapshot(&self) -> u64 {
        self.next_snapshot.fetch_add(1, Ordering::Relaxed)
    }
    pub fn notice(&self, category: &'static str, routine: &str, message: impl Into<String>) {
        if if category == "unfulfilled" {
            !self.options.warn_unfulfilled
        } else {
            !self.options.notices
        } {
            return;
        }
        let message = message.into();
        let key = (category.into(), routine.into(), message.clone());
        let mut records = self.records.lock().unwrap();
        if let Some(&index) = records.index.get(&key) {
            records.events[index].count = records.events[index].count.saturating_add(1);
            return;
        }
        if records.events.len() >= 256 {
            records.dropped = records.dropped.saturating_add(1);
            return;
        }
        let index = records.events.len();
        records.index.insert(key, index);
        records.events.push(Notice {
            category,
            routine: routine.into(),
            message,
            count: 1,
        });
    }
    pub fn notices(&self) -> Vec<Notice> {
        self.records.lock().unwrap().events.clone()
    }
    pub fn render(&self) -> String {
        let records = self.records.lock().unwrap();
        let mut text = String::new();
        if !records.events.is_empty() {
            writeln!(text, "Compiler notices:").unwrap();
        }
        for event in &records.events {
            writeln!(
                text,
                "  [{}] {}: {}{}",
                event.category,
                event.routine,
                event.message,
                if event.count > 1 {
                    format!(" ({} occurrences)", event.count)
                } else {
                    String::new()
                }
            )
            .unwrap();
        }
        if records.dropped != 0 {
            writeln!(text, "  {} additional notices omitted", records.dropped).unwrap();
        }
        for error in &records.errors {
            writeln!(text, "  [emit error] {error}").unwrap();
        }
        text
    }
    pub fn errors(&self) -> Vec<String> {
        self.records.lock().unwrap().errors.clone()
    }
    /// Start an independent reporting window without changing snapshot numbering.
    pub fn window(&self) -> Self {
        Self {
            options: self.options.clone(),
            records: Mutex::default(),
            next_snapshot: self.next_snapshot.clone(),
        }
    }
    pub(crate) fn write(&self, name: &str, text: &str) {
        let Some(directory) = &self.options.directory else {
            return;
        };
        let path = directory.join(name);
        let result = (|| {
            std::fs::create_dir_all(directory)?;
            let temporary = path.with_extension("partial");
            std::fs::write(&temporary, text)?;
            std::fs::rename(temporary, &path)
        })();
        if let Err(error) = result {
            let mut records = self.records.lock().unwrap();
            if records.errors.len() < 16 {
                records.errors.push(format!("{}: {error}", path.display()));
            }
        }
    }
    pub(crate) fn emit_program(&self, program: &crate::ir::Program) {
        if self.options.directory.is_none() {
            return;
        }
        let jobs = program.jobs;
        // Capture readiness once: workers may publish more bodies concurrently,
        // but the manifest must describe this dump's selection consistently.
        let analyzed: Vec<_> = program
            .functions()
            .iter()
            .map(|f| f.is_analyzed())
            .collect();
        // Formatting each checked routine is independent; cold units stay cold.
        jobs.map(program.functions(), 4, |fi, routine| {
            if analyzed[fi] {
                self.emit_routine(program, fi, routine)
            }
        });
        let manifest = program
            .functions()
            .iter()
            .enumerate()
            .map(|(fi, f)| {
                format!(
                    "f{fi} {}: {}\n",
                    f.name,
                    if analyzed[fi] { "analyzed" } else { "deferred" }
                )
            })
            .collect::<String>();
        self.write(&format!("p{}-manifest.txt", program.snapshot_id), &manifest);
    }
    fn emit_routine(&self, program: &crate::ir::Program, fi: usize, routine: &crate::ir::Routine) {
        if !routine.is_analyzed() {
            return;
        }
        let Ok(body) = routine.checked() else {
            return;
        };
        let prefix = format!("p{}-f{fi}", program.snapshot_id);
        if self.options.emit_ir {
            let mut text = format!(
                "function f{fi} {} {:?}\nattributes {:?}\neffects {:?}\nlocals {:?}\nregisters {:?}\n",
                routine.name,
                routine.kind,
                routine.attributes,
                body.effects,
                body.locals,
                body.registers
            );
            for (pc, instruction) in body.code.iter().enumerate() {
                writeln!(
                    text,
                    "  {pc:04} {:?} ; {}..{}",
                    instruction.op, instruction.span.start, instruction.span.end
                )
                .unwrap();
            }
            self.write(&format!("{prefix}.ir"), &text);
        }
        if self.options.emit_bytecode {
            self.write(
                &format!("{prefix}.bytecode"),
                &program.bytecode().disassemble_unit(fi),
            );
        }
    }
}
