//! Opt-in measurement of fresh-process startup work.
//!
//! Build with `--features startup-profile` (root crate) and run with
//! `CYALLO_STARTUP_PROFILE=1` (text) or `=json` (one JSON object on stderr).
//! Without the cargo feature every function here is an empty inline no-op and
//! [`StageGuard`] is zero-sized, so shipping builds pay nothing.
//!
//! This crate is a deliberate, documented exception to the "no process-wide
//! mutable counters" rule in `docs/compiler-and-runtime-rules.md`: its statics
//! are write-only observations, hold no identity, registry or program state,
//! never influence compilation or execution, and exist only in measurement
//! builds. Stage times are inclusive wall time summed over every thread that
//! enters the stage, so they equal elapsed time only for `--jobs 1`; nested
//! stages overlap their parents. Allocation figures come from a counting
//! allocator wrapper and are global, so attribute them only at `--jobs 1`.

/// A timed region of startup work. Stages may nest; each is inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Stage {
    /// `lex_modern` plus parse-unit splitting, per file (user and core).
    Lex,
    /// Recursive-descent parse of one unit into an owned tree.
    Parse,
    /// `IndexedAst::from_source_tree`: owned tree to indexed tables.
    Freeze,
    /// Line starts and unit publication of one parsed file.
    Publish,
    /// Parsing the sealed embedded core package (includes its lex and parse).
    CorePackageParse,
    /// `SourceTrees::materialize`: indexed units back to owned trees.
    Materialize,
    /// `link_with_core` in full, including `CorePackageParse`.
    Resolve,
    /// Generic specialization over the linked modules.
    Specialize,
    /// Compile-time constant evaluation (runs checked lowering internally).
    ConstEval,
    /// `analyze_modules_cached` in full.
    ModuleAnalysis,
    /// Types, globals, inference and header vector construction.
    ModuleDeclarations,
    /// Cache key computation.
    ModuleKeys,
    /// Whole parallel body wave (initializers and per-routine setup).
    ModuleBodyWave,
    /// Per declared routine: syntax clone, cost estimate, `Routine::deferred`.
    DeferredSetup,
    /// Lowering of the entry and module initializer bodies.
    EntryLowering,
    /// `requires_tracing`, which clones each routine body.
    RequiresTracing,
    /// Assembly of the executable module after the wave.
    ModuleLink,
    /// `DeferredBody::analyze` in full (forced routine body check).
    BodyAnalysis,
    /// Typed lowering of one forced body.
    BodyLower,
    /// Cloning the header vector for the forced body's local effect check.
    HeaderClone,
    /// `effects::analyze_one` for the forced body.
    BodyEffects,
    /// GC and resume metadata for the forced body.
    BodyFinish,
    /// `bytecode::Module::lower`: per-routine wrapper creation.
    BytecodeModule,
    /// `bytecode::Function::lower`: one checked body to bytecode.
    BytecodeTranslate,
    /// Verified module construction, including any checks it forces and image verification.
    VerifiedModuleLower,
    /// Translation of one checked routine into a verified function image.
    VerifiedBodyLower,
}

impl Stage {
    pub const ALL: [Self; 26] = [
        Self::Lex,
        Self::Parse,
        Self::Freeze,
        Self::Publish,
        Self::CorePackageParse,
        Self::Materialize,
        Self::Resolve,
        Self::Specialize,
        Self::ConstEval,
        Self::ModuleAnalysis,
        Self::ModuleDeclarations,
        Self::ModuleKeys,
        Self::ModuleBodyWave,
        Self::DeferredSetup,
        Self::EntryLowering,
        Self::RequiresTracing,
        Self::ModuleLink,
        Self::BodyAnalysis,
        Self::BodyLower,
        Self::HeaderClone,
        Self::BodyEffects,
        Self::BodyFinish,
        Self::BytecodeModule,
        Self::BytecodeTranslate,
        Self::VerifiedModuleLower,
        Self::VerifiedBodyLower,
    ];
    pub const fn name(self) -> &'static str {
        match self {
            Self::Lex => "lex",
            Self::Parse => "parse",
            Self::Freeze => "freeze",
            Self::Publish => "publish",
            Self::CorePackageParse => "core_package_parse",
            Self::Materialize => "materialize",
            Self::Resolve => "resolve",
            Self::Specialize => "specialize",
            Self::ConstEval => "const_eval",
            Self::ModuleAnalysis => "module_analysis",
            Self::ModuleDeclarations => "module_declarations",
            Self::ModuleKeys => "module_keys",
            Self::ModuleBodyWave => "module_body_wave",
            Self::DeferredSetup => "deferred_setup",
            Self::EntryLowering => "entry_lowering",
            Self::RequiresTracing => "requires_tracing",
            Self::ModuleLink => "module_link",
            Self::BodyAnalysis => "body_analysis",
            Self::BodyLower => "body_lower",
            Self::HeaderClone => "header_clone",
            Self::BodyEffects => "body_effects",
            Self::BodyFinish => "body_finish",
            Self::BytecodeModule => "bytecode_module",
            Self::BytecodeTranslate => "bytecode_translate",
            Self::VerifiedModuleLower => "verified_module_lower",
            Self::VerifiedBodyLower => "verified_body_lower",
        }
    }
}

/// A count of work items. Bodies are routines with a source body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Counter {
    SourceBytes,
    Tokens,
    Files,
    ParseUnits,
    /// Routines produced by the parser (owned trees).
    BodiesParsed,
    /// Routines copied into indexed tables.
    BodiesFrozen,
    /// Routines copied back into owned trees by the linker.
    BodiesMaterialized,
    /// Routines in the resolver output handed to analysis.
    BodiesResolved,
    /// Routines whose syntax was cloned into a deferred body.
    BodiesDeferredCopied,
    /// Routines cloned by `requires_tracing`.
    BodiesTracingCopied,
    /// Routines whose checked body was produced (forced or eager).
    BodiesChecked,
    /// Routines translated to bytecode.
    BodiesTranslated,
    /// Header-vector clones made for forced bodies.
    HeaderClones,
    /// Header elements copied by those clones.
    HeaderElementsCloned,
    /// Checked instructions in forced bodies.
    CheckedInstructions,
    /// Bytecode words in translated bodies.
    BytecodeWords,
    /// Routines translated into verified function images.
    VerifiedBodiesLowered,
    /// Scalar routines emitted from the new HIR → MIR → AIR pipeline.
    BodiesEmittedFromAir,
    /// Routine bodies lowered from indexed syntax without AST materialization.
    BodiesPreparedFromIndexedSyntax,
}

impl Counter {
    pub const ALL: [Self; 19] = [
        Self::SourceBytes,
        Self::Tokens,
        Self::Files,
        Self::ParseUnits,
        Self::BodiesParsed,
        Self::BodiesFrozen,
        Self::BodiesMaterialized,
        Self::BodiesResolved,
        Self::BodiesDeferredCopied,
        Self::BodiesTracingCopied,
        Self::BodiesChecked,
        Self::BodiesTranslated,
        Self::HeaderClones,
        Self::HeaderElementsCloned,
        Self::CheckedInstructions,
        Self::BytecodeWords,
        Self::VerifiedBodiesLowered,
        Self::BodiesEmittedFromAir,
        Self::BodiesPreparedFromIndexedSyntax,
    ];
    pub const fn name(self) -> &'static str {
        match self {
            Self::SourceBytes => "source_bytes",
            Self::Tokens => "tokens",
            Self::Files => "files",
            Self::ParseUnits => "parse_units",
            Self::BodiesParsed => "bodies_parsed",
            Self::BodiesFrozen => "bodies_frozen",
            Self::BodiesMaterialized => "bodies_materialized",
            Self::BodiesResolved => "bodies_resolved",
            Self::BodiesDeferredCopied => "bodies_deferred_copied",
            Self::BodiesTracingCopied => "bodies_tracing_copied",
            Self::BodiesChecked => "bodies_checked",
            Self::BodiesTranslated => "bodies_translated",
            Self::HeaderClones => "header_clones",
            Self::HeaderElementsCloned => "header_elements_cloned",
            Self::CheckedInstructions => "checked_instructions",
            Self::BytecodeWords => "bytecode_words",
            Self::VerifiedBodiesLowered => "verified_bodies_lowered",
            Self::BodiesEmittedFromAir => "bodies_emitted_from_air",
            Self::BodiesPreparedFromIndexedSyntax => "bodies_prepared_from_indexed_syntax",
        }
    }
}

#[cfg(feature = "enabled")]
mod on {
    use super::{Counter, Stage};
    use std::alloc::{GlobalAlloc, Layout};
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    use std::time::Instant;

    const STAGES: usize = Stage::ALL.len();
    const COUNTERS: usize = Counter::ALL.len();
    const ZERO: AtomicU64 = AtomicU64::new(0);

    static NANOS: [AtomicU64; STAGES] = [ZERO; STAGES];
    static CALLS: [AtomicU64; STAGES] = [ZERO; STAGES];
    static ALLOC: [AtomicU64; STAGES] = [ZERO; STAGES];
    static COUNT: [AtomicU64; COUNTERS] = [ZERO; COUNTERS];
    static ALLOCATED: AtomicU64 = AtomicU64::new(0);
    static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
    static LIVE: AtomicU64 = AtomicU64::new(0);
    static PEAK: AtomicU64 = AtomicU64::new(0);
    static START: OnceLock<Instant> = OnceLock::new();
    static FIRST_OUTPUT: OnceLock<Snapshot> = OnceLock::new();

    /// Counting wrapper for the global allocator, measurement builds only.
    pub struct Counting<A>(pub A);
    // SAFETY: every operation forwards unchanged to the wrapped allocator; the
    // wrapper only adds relaxed counter updates.
    unsafe impl<A: GlobalAlloc> GlobalAlloc for Counting<A> {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            grow(layout.size());
            unsafe { self.0.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            grow(layout.size());
            unsafe { self.0.alloc_zeroed(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            LIVE.fetch_sub(layout.size() as u64, Relaxed);
            unsafe { self.0.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            LIVE.fetch_sub(layout.size() as u64, Relaxed);
            grow(new_size);
            unsafe { self.0.realloc(ptr, layout, new_size) }
        }
    }
    fn grow(size: usize) {
        let size = size as u64;
        ALLOCATED.fetch_add(size, Relaxed);
        ALLOCATIONS.fetch_add(1, Relaxed);
        let live = LIVE.fetch_add(size, Relaxed) + size;
        PEAK.fetch_max(live, Relaxed);
    }

    pub struct StageGuard {
        stage: Stage,
        started: Instant,
        allocated: u64,
    }
    impl Drop for StageGuard {
        fn drop(&mut self) {
            let i = self.stage as usize;
            NANOS[i].fetch_add(self.started.elapsed().as_nanos() as u64, Relaxed);
            CALLS[i].fetch_add(1, Relaxed);
            ALLOC[i].fetch_add(
                ALLOCATED.load(Relaxed).wrapping_sub(self.allocated),
                Relaxed,
            );
        }
    }
    pub fn stage(stage: Stage) -> StageGuard {
        StageGuard {
            stage,
            started: Instant::now(),
            allocated: ALLOCATED.load(Relaxed),
        }
    }
    pub fn count(counter: Counter, n: usize) {
        COUNT[counter as usize].fetch_add(n as u64, Relaxed);
    }
    pub fn init() {
        START.get_or_init(Instant::now);
    }

    struct Snapshot {
        elapsed_ns: u64,
        counters: [u64; COUNTERS],
        stage_ns: [u64; STAGES],
        allocated: u64,
    }
    fn snapshot() -> Snapshot {
        Snapshot {
            elapsed_ns: START.get().map_or(0, |s| s.elapsed().as_nanos() as u64),
            counters: std::array::from_fn(|i| COUNT[i].load(Relaxed)),
            stage_ns: std::array::from_fn(|i| NANOS[i].load(Relaxed)),
            allocated: ALLOCATED.load(Relaxed),
        }
    }
    pub fn mark_first_output() {
        if FIRST_OUTPUT.get().is_none() {
            let _ = FIRST_OUTPUT.set(snapshot());
        }
    }

    fn json_snapshot(out: &mut String, s: &Snapshot) {
        use std::fmt::Write;
        let _ = write!(
            out,
            "{{\"elapsed_ns\":{},\"allocated_bytes\":{},\"counters\":{{",
            s.elapsed_ns, s.allocated
        );
        for (i, c) in Counter::ALL.iter().enumerate() {
            let _ = write!(out, "{}\"{}\":{}", comma(i), c.name(), s.counters[i]);
        }
        out.push_str("},\"stage_ns\":{");
        for (i, st) in Stage::ALL.iter().enumerate() {
            let _ = write!(out, "{}\"{}\":{}", comma(i), st.name(), s.stage_ns[i]);
        }
        out.push_str("}}");
    }
    fn comma(i: usize) -> &'static str {
        if i == 0 { "" } else { "," }
    }

    /// Print the report when `CYALLO_STARTUP_PROFILE` is set: `json` for one
    /// JSON object, any other non-empty value for a text table.
    pub fn finish() {
        use std::fmt::Write;
        let Some(mode) = std::env::var_os("CYALLO_STARTUP_PROFILE") else {
            return;
        };
        let end = snapshot();
        let mut out = String::new();
        if mode == "json" {
            out.push_str("{\"end\":");
            json_snapshot(&mut out, &end);
            if let Some(first) = FIRST_OUTPUT.get() {
                out.push_str(",\"first_output\":");
                json_snapshot(&mut out, first);
            }
            let _ = write!(
                out,
                ",\"peak_live_bytes\":{},\"allocations\":{},\"stages\":{{",
                PEAK.load(Relaxed),
                ALLOCATIONS.load(Relaxed)
            );
            for (i, st) in Stage::ALL.iter().enumerate() {
                let _ = write!(
                    out,
                    "{}\"{}\":{{\"ns\":{},\"calls\":{},\"alloc_bytes\":{}}}",
                    comma(i),
                    st.name(),
                    NANOS[i].load(Relaxed),
                    CALLS[i].load(Relaxed),
                    ALLOC[i].load(Relaxed)
                );
            }
            out.push_str("}}\n");
        } else {
            let _ = writeln!(
                out,
                "startup profile (inclusive wall time summed over threads)"
            );
            for (i, st) in Stage::ALL.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "  {:<22} {:>10.3} ms {:>8} calls {:>12} B",
                    st.name(),
                    NANOS[i].load(Relaxed) as f64 / 1e6,
                    CALLS[i].load(Relaxed),
                    ALLOC[i].load(Relaxed)
                );
            }
            for (i, c) in Counter::ALL.iter().enumerate() {
                let at_first = FIRST_OUTPUT.get().map(|f| f.counters[i]);
                let _ = writeln!(
                    out,
                    "  {:<24} {:>10} (at first output: {})",
                    c.name(),
                    end.counters[i],
                    at_first.map_or("-".into(), |v| v.to_string())
                );
            }
            let _ = writeln!(
                out,
                "  first output at {:.3} ms, end at {:.3} ms, peak live {} B",
                FIRST_OUTPUT
                    .get()
                    .map_or(0.0, |f| f.elapsed_ns as f64 / 1e6),
                end.elapsed_ns as f64 / 1e6,
                PEAK.load(Relaxed)
            );
        }
        eprint!("{out}");
    }
}

#[cfg(feature = "enabled")]
pub use on::{Counting, StageGuard, count, finish, init, mark_first_output, stage};

#[cfg(not(feature = "enabled"))]
mod off {
    use super::{Counter, Stage};
    /// Zero-sized when the `enabled` feature is off.
    pub struct StageGuard;
    #[inline(always)]
    pub fn stage(_: Stage) -> StageGuard {
        StageGuard
    }
    #[inline(always)]
    pub fn count(_: Counter, _: usize) {}
    #[inline(always)]
    pub fn init() {}
    #[inline(always)]
    pub fn mark_first_output() {}
    #[inline(always)]
    pub fn finish() {}
}
#[cfg(not(feature = "enabled"))]
pub use off::{StageGuard, count, finish, init, mark_first_output, stage};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stage_and_counter_tables_cover_every_variant_in_order() {
        for (i, s) in Stage::ALL.iter().enumerate() {
            assert_eq!(*s as usize, i, "{}", s.name());
        }
        for (i, c) in Counter::ALL.iter().enumerate() {
            assert_eq!(*c as usize, i, "{}", c.name());
        }
    }
}
