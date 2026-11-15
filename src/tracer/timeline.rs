//! The trace model the viewer works from. It is read from the `trace` member
//! of a `--jit-trace-format=json` report and then turned into one lane per
//! routine: which tier is installed over time, when compiles run, and where
//! transitions and inline decisions happened.
use super::json::{self, Json};
use std::fmt;

/// Version of the `trace` member this viewer understands.
const SUPPORTED_VERSION: u64 = 1;

#[derive(Debug)]
pub(crate) enum TraceError {
    /// The file is not JSON.
    Json(json::ParseError),
    /// The document has no `trace` member; the report was written without
    /// `--jit-trace`, or with `--jit-trace-format=human`.
    NoTrace,
    /// The `trace` member has a different version than this viewer reads.
    Version(u64),
    /// A required member is missing or has the wrong type.
    Field(&'static str),
}

impl fmt::Display for TraceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(f, "{error}"),
            Self::NoTrace => write!(
                f,
                "no trace member; write the report with --jit-trace --jit-trace-format=json"
            ),
            Self::Version(version) => write!(
                f,
                "trace version {version} is not supported (this viewer reads version {SUPPORTED_VERSION})"
            ),
            Self::Field(name) => {
                write!(f, "trace member `{name}` is missing or has the wrong type")
            }
        }
    }
}

impl std::error::Error for TraceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}

impl From<json::ParseError> for TraceError {
    fn from(error: json::ParseError) -> Self {
        Self::Json(error)
    }
}

/// A code level a routine can run at. `Interpreted` is the VM before any
/// native code is installed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Level {
    Interpreted,
    Winch,
    Cranelift,
    Speed,
}

impl Level {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "interpreter" => Some(Self::Interpreted),
            "winch" => Some(Self::Winch),
            "cranelift" => Some(Self::Cranelift),
            "cranelift_speed" => Some(Self::Speed),
            _ => None,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Interpreted => "interpreter",
            Self::Winch => "winch",
            Self::Cranelift => "cranelift",
            Self::Speed => "cranelift_speed",
        }
    }
}

/// What happened. Lifecycle kinds concern compiled code; transitions are
/// VM-to-native entries and OSR.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Request,
    Supersede,
    CompileStart,
    CompileEnd,
    Install,
    Discard,
    Failed,
    Admit,
    Enter,
    Handoff,
    Osr,
}

impl Kind {
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "request" => Self::Request,
            "supersede" => Self::Supersede,
            "compile_start" => Self::CompileStart,
            "compile_end" => Self::CompileEnd,
            "install" => Self::Install,
            "discard" => Self::Discard,
            "failed" => Self::Failed,
            "admit" => Self::Admit,
            "enter" => Self::Enter,
            "handoff" => Self::Handoff,
            "osr" => Self::Osr,
            _ => return None,
        })
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Supersede => "supersede",
            Self::CompileStart => "compile",
            Self::CompileEnd => "compiled",
            Self::Install => "install",
            Self::Discard => "discard",
            Self::Failed => "failed",
            Self::Admit => "admit",
            Self::Enter => "enter",
            Self::Handoff => "handoff",
            Self::Osr => "osr",
        }
    }

    pub(crate) fn is_lifecycle(self) -> bool {
        matches!(
            self,
            Self::Request
                | Self::Supersede
                | Self::CompileStart
                | Self::CompileEnd
                | Self::Install
                | Self::Discard
                | Self::Failed
                | Self::Admit
        )
    }

    pub(crate) fn is_transition(self) -> bool {
        matches!(self, Self::Enter | Self::Handoff | Self::Osr)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Event {
    pub at_ms: f64,
    pub kind: Kind,
    pub routine: usize,
    pub name: String,
    pub detail: String,
    /// Installed or compiled tier (install, compile, request, discard).
    pub tier: Option<Level>,
    /// Source level of an install or an entry.
    pub from: Option<Level>,
    /// Target level of an entry, handoff or OSR.
    pub to: Option<Level>,
    pub duration_ms: Option<f64>,
    pub queued_ms: Option<f64>,
    pub ready_ms: Option<f64>,
    pub invocation: Option<u64>,
    pub pc: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Routine {
    pub index: usize,
    pub name: String,
    pub calls: u64,
    pub instructions: u64,
    pub osr: Option<u64>,
    pub exec_ms: Option<f64>,
    /// Instructions per level: interpreter, winch, cranelift, speed.
    pub engines: Option<[u64; 4]>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Inline {
    pub routine: usize,
    pub routine_name: String,
    pub pc: usize,
    pub callee: usize,
    pub callee_name: String,
    /// `frameless`, `embedded` or `call`.
    pub decision: String,
    pub cost: usize,
    pub depth: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Trace {
    pub mode: String,
    pub engine: String,
    pub status: String,
    pub wall_ms: f64,
    pub dropped_events: u64,
    pub routines: Vec<Routine>,
    /// Sorted by time; ties keep the order they were written in.
    pub events: Vec<Event>,
    pub inlines: Vec<Inline>,
    /// The time the viewer can show: the run wall time or the last event,
    /// whichever is later.
    pub span_ms: f64,
}

impl Trace {
    /// Parse a whole report file and take its `trace` member.
    pub(crate) fn parse(text: &str) -> Result<Self, TraceError> {
        let root = json::parse(text)?;
        let trace = root.get("trace").ok_or(TraceError::NoTrace)?;
        let version = uint(trace, "version")?;
        if version != SUPPORTED_VERSION {
            return Err(TraceError::Version(version));
        }
        let routines = array(trace, "routines")?
            .iter()
            .map(routine)
            .collect::<Result<Vec<_>, _>>()?;
        let events = array(trace, "events")?
            .iter()
            .map(event)
            .collect::<Result<Vec<_>, _>>()?;
        let mut events = events;
        events.sort_by(|a, b| a.at_ms.total_cmp(&b.at_ms));
        let inlines = array(trace, "inlines")?
            .iter()
            .map(inline)
            .collect::<Result<Vec<_>, _>>()?;
        let wall_ms = number(trace, "wall_ms")?;
        let last = events.last().map_or(0.0, |event| event.at_ms);
        Ok(Self {
            mode: text_of(trace, "mode")?,
            engine: text_of(trace, "engine")?,
            status: text_of(trace, "status")?,
            wall_ms,
            dropped_events: uint(trace, "dropped_events")?,
            routines,
            events,
            inlines,
            span_ms: wall_ms.max(last).max(1e-6),
        })
    }

    /// The name of a routine, from the routine table or the first event.
    pub(crate) fn routine_name(&self, index: usize) -> String {
        self.routines
            .iter()
            .find(|routine| routine.index == index)
            .map(|routine| routine.name.clone())
            .or_else(|| {
                self.events
                    .iter()
                    .find(|event| event.routine == index)
                    .map(|event| event.name.clone())
            })
            .unwrap_or_else(|| format!("routine {index}"))
    }

    /// Routines worth a lane: those that ran or had any event.
    pub(crate) fn visible_routines(&self) -> Vec<usize> {
        let mut indexes: Vec<usize> = self
            .routines
            .iter()
            .filter(|routine| routine.instructions != 0 || routine.calls != 0)
            .map(|routine| routine.index)
            .chain(self.events.iter().map(|event| event.routine))
            .collect();
        indexes.sort_unstable();
        indexes.dedup();
        indexes
    }
}

/// A closed time interval on one lane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Span {
    pub start: f64,
    pub end: f64,
    pub level: Level,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MarkerKind {
    Install(Level),
    Osr,
    Failed,
    /// A compile-time inline decision, placed where the routine first ran
    /// native code. The compiler does not time inline decisions.
    Inline,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Marker {
    pub at: f64,
    pub kind: MarkerKind,
}

/// One routine's view of the trace.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Lane {
    pub routine: usize,
    pub name: String,
    /// The installed level from each time onwards, starting at the beginning
    /// of the trace with `Interpreted`.
    pub bands: Vec<(f64, Level)>,
    /// Time spent compiling this routine.
    pub compiles: Vec<Span>,
    /// Time a compile waited for a worker.
    pub queued: Vec<Span>,
    pub markers: Vec<Marker>,
    /// Indexes into `Trace::events` for this routine, in time order.
    pub events: Vec<usize>,
    pub inline_count: usize,
}

impl Lane {
    /// The level installed at `at`.
    pub(crate) fn level_at(&self, at: f64) -> Level {
        self.bands
            .iter()
            .rev()
            .find(|(start, _)| *start <= at)
            .map_or(Level::Interpreted, |(_, level)| *level)
    }
}

/// Build one lane per visible routine, in routine index order.
pub(crate) fn lanes(trace: &Trace) -> Vec<Lane> {
    trace
        .visible_routines()
        .into_iter()
        .map(|index| build_lane(trace, index))
        .collect()
}

fn build_lane(trace: &Trace, index: usize) -> Lane {
    let mut bands = vec![(0.0, Level::Interpreted)];
    let mut compiles = Vec::new();
    let mut queued = Vec::new();
    let mut markers = Vec::new();
    let mut events = Vec::new();
    for (position, event) in trace.events.iter().enumerate() {
        if event.routine != index {
            continue;
        }
        events.push(position);
        let at = event.at_ms;
        match (event.kind, event.tier) {
            (Kind::Install, Some(level)) => {
                bands.push((at, level));
                markers.push(Marker {
                    at,
                    kind: MarkerKind::Install(level),
                });
            }
            (Kind::CompileEnd, Some(level)) => {
                let duration = event.duration_ms.unwrap_or(0.0);
                compiles.push(Span {
                    start: (at - duration).max(0.0),
                    end: at,
                    level,
                });
            }
            (Kind::CompileStart, Some(level)) => {
                let wait = event.queued_ms.unwrap_or(0.0);
                if wait > 0.0 {
                    queued.push(Span {
                        start: (at - wait).max(0.0),
                        end: at,
                        level,
                    });
                }
            }
            (Kind::Osr, _) => markers.push(Marker {
                at,
                kind: MarkerKind::Osr,
            }),
            (Kind::Failed, _) => markers.push(Marker {
                at,
                kind: MarkerKind::Failed,
            }),
            _ => {}
        }
    }
    // Inline decisions appear where the routine first runs native code.
    let first_native = bands
        .iter()
        .find(|(_, level)| *level != Level::Interpreted)
        .map(|(at, _)| *at);
    // Calls the compiler kept as calls are not inlining; only frameless and
    // embedded decisions are shown.
    let inline_sites: Vec<&Inline> = trace
        .inlines
        .iter()
        .filter(|inline| inline.routine == index && inline.decision != "call")
        .collect();
    if let Some(at) = first_native {
        markers.extend(inline_sites.iter().map(|_| Marker {
            at,
            kind: MarkerKind::Inline,
        }));
    }
    markers.sort_by(|a, b| a.at.total_cmp(&b.at));
    Lane {
        routine: index,
        name: trace.routine_name(index),
        bands,
        compiles,
        queued,
        markers,
        events,
        inline_count: inline_sites.len(),
    }
}

fn field<'a>(object: &'a Json, key: &'static str) -> Result<&'a Json, TraceError> {
    object.get(key).ok_or(TraceError::Field(key))
}

fn number(object: &Json, key: &'static str) -> Result<f64, TraceError> {
    field(object, key)?.as_f64().ok_or(TraceError::Field(key))
}

fn uint(object: &Json, key: &'static str) -> Result<u64, TraceError> {
    let value = number(object, key)?;
    if value >= 0.0 && value.fract() == 0.0 && value <= u64::MAX as f64 {
        Ok(value as u64)
    } else {
        Err(TraceError::Field(key))
    }
}

fn text_of(object: &Json, key: &'static str) -> Result<String, TraceError> {
    field(object, key)?
        .as_str()
        .map(str::to_owned)
        .ok_or(TraceError::Field(key))
}

fn array<'a>(object: &'a Json, key: &'static str) -> Result<&'a [Json], TraceError> {
    field(object, key)?.as_array().ok_or(TraceError::Field(key))
}

/// A number, or `None` for `null` or a missing member.
fn optional_number(object: &Json, key: &str) -> Option<f64> {
    object.get(key).and_then(Json::as_f64)
}

fn optional_uint(object: &Json, key: &str) -> Option<u64> {
    optional_number(object, key)
        .filter(|value| *value >= 0.0 && value.fract() == 0.0)
        .map(|value| value as u64)
}

fn optional_level(object: &Json, key: &str) -> Option<Level> {
    object
        .get(key)
        .and_then(Json::as_str)
        .and_then(Level::from_name)
}

fn routine(value: &Json) -> Result<Routine, TraceError> {
    let engines = match field(value, "engines")? {
        Json::Null => None,
        table => {
            let mut counts = [0; 4];
            for (slot, name) in ["interpreter", "winch", "cranelift", "cranelift_speed"]
                .iter()
                .enumerate()
            {
                counts[slot] = uint(table, name)?;
            }
            Some(counts)
        }
    };
    Ok(Routine {
        index: uint(value, "index")? as usize,
        name: text_of(value, "name")?,
        calls: uint(value, "calls")?,
        instructions: uint(value, "instructions")?,
        osr: optional_uint(value, "osr"),
        exec_ms: optional_number(value, "exec_ms"),
        engines,
    })
}

fn event(value: &Json) -> Result<Event, TraceError> {
    let kind_name = text_of(value, "kind")?;
    let kind = Kind::from_name(&kind_name).ok_or(TraceError::Field("kind"))?;
    Ok(Event {
        at_ms: number(value, "at_ms")?,
        kind,
        routine: uint(value, "routine")? as usize,
        name: text_of(value, "name")?,
        detail: text_of(value, "detail")?,
        tier: optional_level(value, "tier"),
        from: optional_level(value, "from"),
        to: optional_level(value, "to"),
        duration_ms: optional_number(value, "duration_ms"),
        queued_ms: optional_number(value, "queued_ms"),
        ready_ms: optional_number(value, "ready_ms"),
        invocation: optional_uint(value, "invocation"),
        pc: optional_uint(value, "pc"),
    })
}

fn inline(value: &Json) -> Result<Inline, TraceError> {
    Ok(Inline {
        routine: uint(value, "routine")? as usize,
        routine_name: text_of(value, "routine_name")?,
        pc: uint(value, "pc")? as usize,
        callee: uint(value, "callee")? as usize,
        callee_name: text_of(value, "callee_name")?,
        decision: text_of(value, "decision")?,
        cost: uint(value, "cost")? as usize,
        depth: uint(value, "depth")? as usize,
    })
}

#[cfg(test)]
pub(crate) mod fixtures {
    /// A small trace in the report's own shape: one routine that is
    /// interpreted, compiled to winch, then optimized and OSR-entered, plus
    /// one inline decision.
    pub(crate) const SAMPLE: &str = r#"{
        "title": "Execution report: jit (Finished)",
        "sections": [],
        "trace": {
          "version": 1, "mode": "trace", "engine": "jit", "status": "Finished",
          "wall_ms": 10.0, "dropped_events": 0,
          "routines": [
            {"index": 0, "name": "<main>", "calls": 1, "instructions": 100, "osr": 0,
             "exec_ms": 0.5, "engines": {"interpreter": 100, "winch": 0, "cranelift": 0, "cranelift_speed": 0}},
            {"index": 1, "name": "merge_sort", "calls": 3, "instructions": 900, "osr": 1,
             "exec_ms": 8.0, "engines": {"interpreter": 100, "winch": 300, "cranelift": 500, "cranelift_speed": 0}}
          ],
          "events": [
            {"at_ms": 1.0, "kind": "request", "routine": 1, "name": "merge_sort", "detail": "winch", "tier": "winch", "executed": 189, "size": 40},
            {"at_ms": 1.2, "kind": "compile_start", "routine": 1, "name": "merge_sort", "detail": "winch", "tier": "winch", "worker": "ThreadId(2)", "queued_ms": 0.2},
            {"at_ms": 2.0, "kind": "compile_end", "routine": 1, "name": "merge_sort", "detail": "winch in 0.8 ms", "tier": "winch", "duration_ms": 0.8, "fallback": null},
            {"at_ms": 2.0, "kind": "install", "routine": 1, "name": "merge_sort", "detail": "uncompiled -> winch", "from": null, "tier": "winch"},
            {"at_ms": 3.0, "kind": "request", "routine": 1, "name": "merge_sort", "detail": "cranelift", "tier": "cranelift", "executed": 5004, "size": 40},
            {"at_ms": 6.0, "kind": "compile_end", "routine": 1, "name": "merge_sort", "detail": "cranelift in 3.0 ms", "tier": "cranelift", "duration_ms": 3.0, "fallback": null},
            {"at_ms": 6.1, "kind": "install", "routine": 1, "name": "merge_sort", "detail": "winch -> cranelift", "from": "winch", "tier": "cranelift"},
            {"at_ms": 6.5, "kind": "osr", "routine": 1, "name": "merge_sort", "detail": "winch -> cranelift", "from": "winch", "to": "cranelift", "invocation": 2, "pc": 7, "depth": 2, "version": 3, "ready_ms": 0.4},
            {"at_ms": 7.0, "kind": "failed", "routine": 0, "name": "<main>", "detail": "boom", "error": "boom"}
          ],
          "inlines": [
            {"routine": 1, "routine_name": "merge_sort", "pc": 29, "callee": 1, "callee_name": "merge_sort", "decision": "call", "cost": 0, "depth": 0},
            {"routine": 1, "routine_name": "merge_sort", "pc": 12, "callee": 2, "callee_name": "merge", "decision": "frameless", "cost": 40, "depth": 1}
          ]
        }
      }"#;
}

#[cfg(test)]
mod tests {
    use super::fixtures::SAMPLE;
    use super::*;

    #[test]
    fn parses_the_report_shape() {
        let trace = Trace::parse(SAMPLE).unwrap();
        assert_eq!(trace.engine, "jit");
        assert_eq!(trace.routines.len(), 2);
        assert_eq!(trace.routines[1].engines, Some([100, 300, 500, 0]));
        assert_eq!(trace.events.len(), 9);
        assert_eq!(trace.events[7].kind, Kind::Osr);
        assert_eq!(trace.events[7].to, Some(Level::Cranelift));
        assert_eq!(trace.inlines[0].decision, "call");
        assert_eq!(trace.span_ms, 10.0);
    }

    #[test]
    fn builds_bands_compile_spans_and_markers() {
        let trace = Trace::parse(SAMPLE).unwrap();
        let lanes = lanes(&trace);
        let merge = lanes.iter().find(|lane| lane.name == "merge_sort").unwrap();
        assert_eq!(merge.level_at(0.5), Level::Interpreted);
        assert_eq!(merge.level_at(2.5), Level::Winch);
        assert_eq!(merge.level_at(6.2), Level::Cranelift);
        assert_eq!(
            merge.compiles,
            vec![
                Span {
                    start: 1.2,
                    end: 2.0,
                    level: Level::Winch
                },
                Span {
                    start: 3.0,
                    end: 6.0,
                    level: Level::Cranelift
                },
            ]
        );
        assert_eq!(
            merge.queued,
            vec![Span {
                start: 1.0,
                end: 1.2,
                level: Level::Winch
            }]
        );
        assert!(merge.markers.contains(&Marker {
            at: 6.5,
            kind: MarkerKind::Osr
        }));
        // The frameless site shows where merge_sort first ran native code; the
        // call that stayed a call is not an inline marker.
        let inline_markers = merge
            .markers
            .iter()
            .filter(|marker| marker.kind == MarkerKind::Inline)
            .count();
        assert_eq!(inline_markers, 1);
        assert!(merge.markers.contains(&Marker {
            at: 2.0,
            kind: MarkerKind::Inline
        }));
        assert_eq!(merge.inline_count, 1);
    }

    #[test]
    fn explains_missing_or_unsupported_traces() {
        assert!(matches!(Trace::parse("{}"), Err(TraceError::NoTrace)));
        let other = SAMPLE.replace("\"version\": 1", "\"version\": 2");
        assert!(matches!(Trace::parse(&other), Err(TraceError::Version(2))));
        let broken = SAMPLE.replace("\"at_ms\": 6.5", "\"at_ms\": \"late\"");
        assert!(matches!(
            Trace::parse(&broken),
            Err(TraceError::Field("at_ms"))
        ));
        let unknown = SAMPLE.replace("\"kind\": \"osr\"", "\"kind\": \"jump\"");
        assert!(matches!(
            Trace::parse(&unknown),
            Err(TraceError::Field("kind"))
        ));
    }
}
