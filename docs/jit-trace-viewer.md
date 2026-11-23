# JIT trace viewer

> **Legacy engine (2026-11-20).** This describes the tiered engine in `src/runtime` and `src/backend` (everything outside `src/backend/verified`), which the verified pipeline replaces. It stays accurate while that engine exists and is retired with it in the [legacy removal plan](legacy-removal-plan.md). Current native-tier work: [native tiers: next steps](native-tiers-next-steps.md).

`cyallo trace` opens an interactive timeline of one JIT run. For each routine it
shows which code tier was installed when, how long compiles ran and waited, where
OSR entries happened, and which call sites were inlined. Use it to answer questions
such as:

- When did `merge_sort` leave the interpreter, and for which tier?
- Did a hot loop enter native code by OSR, and how long did it run in the old tier
  after the new code was ready?
- Why is a routine still interpreted?
- Did inlining happen at the call sites I expected?

The viewer reads a file that the run wrote. It does not run the program.

## 1. Record a trace

```bash
cargo build --release
./target/release/cyallo run examples/mergesort.yas --jit-trace --jit-trace-format=json --jit-trace-out trace.json
```

`--jit-trace-out PATH` writes the report to `PATH`. Program output still goes to
stdout, and stderr stays empty. Without the argument the report goes to stderr, so
you can redirect it instead: `2> trace.json >/dev/null`.

| Flags | What you get |
| --- | --- |
| `--jit-trace --jit-trace-format=json` | Full trace: every tier event, every VM entry, OSR, inline decisions. Use this for the viewer. |
| `--jit-trace=summary --jit-trace-format=json` | Counters and compile events only. The viewer opens it, but shows no VM entries or OSR. |
| `--jit-trace` (human format) | The human report on stderr. The viewer cannot read it. |
| `--jit-no-inline` | Turns routine inlining off, for comparing two traces. |
| `--jit-trace-out PATH` | Writes the final report to `PATH` instead of stderr. Needs `--jit-trace` or `--jit-trace=summary`. |

`--jit-trace-format=json` only has an effect together with `--jit-trace` or
`--jit-trace=summary`.

A bare `--jit-trace` on a terminal also draws a live status block while the program
runs. It is off when the format is JSON, so the JSON on stderr stays clean.

Tracing adds observer overhead. Use it to understand transitions. For timing
conclusions, compare against a plain `cyallo run` built with `--release`.

## 2. Open the viewer

```bash
./target/release/cyallo trace trace.json
```

Requirements:

- Run it in a terminal. Both stdin and stdout must be terminals.
- Use at least 120 columns and 30 rows. Narrower terminals clip the title bar and
  the events panel.
- Press `q` to quit. The terminal is restored on exit.

## 3. Read the screen

Here is a real screen for `examples/mergesort.yas`, at the start of the run:

```text
 cyallo trace · guide-trace.json · jit Finished · wall 183.918 ms · view 0.000–183.918 ms · cursor 0.000 ms · events: al
                      ▼0.00ms          30.03ms         60.05ms         90.08ms         120.11ms        150.14ms        1
▸ <main>              ··▲▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▲████████████████████████████████████████████████████████████████◆
    now: interpreter  ··▲▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▲████████████████████████████████████████████████████████████████◆
                      ··▲▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▲████████████████████████████████████████████████████████████████◆
                      ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  merge_sort          ·····▲◆◆▒▒▒▒◆▒▒▒▒▒▒▒▒▒▒◆▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒◆▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▲◆██████████◆███◆███████
    now: interpreter  ·····▲◆◆▒▒▒▒◆▒▒▒▒▒▒▒▒▒▒◆▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒◆▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▲◆██████████◆███◆███████
                      ·····▲◆◆▒▒▒▒◆▒▒▒▒▒▒▒▒▒▒◆▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒◆▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▲◆██████████◆███◆███████
                         ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

┌ events near cursor ──────────────────────────────────────────────────────────────────────────────────────────────────┐
│<main>: interpreter at cursor, 0 inline site(s) · instructions 21664 (interp 9216, winch 12429, cranelift 19, speed 0)│
│     0.261 ms  <main>       request    winch manual request; 94 bytecodes                                             │
│     0.335 ms  <main>       compile    winch on ThreadId(109); queued 0.074 ms                                        │
│     0.661 ms  <main>       enter      entry -> interpreter; invocation 1 pc 0 depth 1 version 0                      │
│     2.860 ms  <main>       request    cranelift after 5120 instructions; 94 bytecodes                                │
│     3.646 ms  <main>       compiled   winch in 3.311 ms                                                              │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

The screen has five parts.

**Title bar.** File name, engine and status, the run's wall time, the visible time
window (`view`), the cursor position, and the active event filter.

**Ruler.** Time labels across the timeline, and `▼` at the cursor's column.

**Lanes.** One lane per routine that ran or had events. The `▸` marks the selected
routine. The label column shows the routine name and `now:` followed by the level
installed at the cursor. A lane has several lines. Its upper lines show the tier
band and markers. Its bottom line shows compile work.

**Events panel.** A summary line for the selected routine, then the five events
nearest the cursor, in time order. The filter applies to this list too.

**Footer.** Key summary. Press `?` for the full list.

### Glyphs

| Glyph | Meaning |
| --- | --- |
| `·` | Interpreter. No native code is installed yet. |
| `▒` | Winch (baseline) code is installed. |
| `█` | Cranelift code is installed. Speed-tier code uses the same glyph, in a brighter green. |
| `▲` | A code tier was installed here. The color shows the tier. |
| `◆` | OSR entry: a running invocation moved into the new code in the middle of execution. |
| `◇` | Inline decision: this routine's native code inlines at least one callee. Shown where the routine first ran native code. Hidden when an install shares the column. |
| `✖` | The compiler failed for this routine. |
| `━` | A compiler worker was busy with this routine. |
| `┄` | A compile request waited for a worker. |

The band shows the installed tier, not the tier executing at that instant. OSR
markers show when the VM actually entered the new code.

## 4. Keys

| Key | Action |
| --- | --- |
| `←` `→` or `h` `l` | Move the cursor by 1/40 of the visible window. The window follows it. |
| `[` `]` | Pan the window left or right by half its width. |
| `+` `-` (or `=` `_`) | Zoom in or out. The cursor stays on screen. |
| `0` | Fit the whole run in the window. |
| `Home` `End` | Move the cursor to the start or end of the run. |
| `n` `p` | Jump to the next or previous event that matches the filter. |
| `↑` `↓` or `k` `j` | Select the previous or next routine. |
| `f` | Cycle the filter: `all`, then `compile/install`, then `entries/OSR/inline`. |
| `?` | Show or hide the key list. While it is open, `q` quits; any other key closes it. |
| `q` or `Esc` | Quit. |

Filters:

- `compile/install` shows `request`, `compile`, `compiled`, `install`, `discard`,
  `supersede`, `admit` and `failed`, and hides VM entries and OSR.
- `entries/OSR/inline` shows VM entries, handoffs, OSR and inline markers, and hides
  compile and install.

Failures are always shown.

## 5. Recipes

### When did a routine get optimized?

1. Press `f` until the filter says `compile/install`.
2. Press `n` repeatedly. Each step lands on the next install or compile event.
3. Read the events panel. An install reads `uncompiled -> winch` or
   `winch -> cranelift`, and the nearest events show what requested it.

Or select the routine with `↑` `↓` and watch the `now:` label as you move the cursor
with `←` `→`. It changes at each install.

### Did OSR happen, and was the new code ready early enough?

1. Press `f` until the filter says `entries/OSR/inline`.
2. Zoom to the region of interest with `+`, then place the cursor on a `◆`.
3. An OSR event carries `ready-to-entry`: the time between when the target code
   finished compiling and when the running invocation entered it. A large value
   means the invocation kept running in the lower tier for that long after the
   faster code was available.

In the mergesort run, the first OSR entry of `merge_sort` happens at 89.462 ms:
`interpreter -> winch; invocation 2 pc 30 depth 2; ready-to-entry 78.833 ms`. The
Winch code for `merge_sort` was ready about 79 ms earlier, but invocation 2 was still
interpreted until it entered it. Your numbers will differ from run to run; the
mechanism is the same.

### Why is a loop still interpreted?

1. Select its routine with `↑` `↓`.
2. The band is `·` until the first `▲`. If no install appears, the routine never
   reached a threshold.
3. Press `n` to step through `request` events for that routine. A request says
   which tier was asked for and why, for example `after 5120 instructions` or
   `after 189 instructions`.
4. A `compile` event that waits shows `queued` time. A long queue means the compiler
   workers were busy.

### Did inlining happen where I expected?

1. Read the summary line of the selected routine: `N inline site(s)`.
2. Look for `◇` on the routine's lane. It marks where the routine first ran native
   code, not the call site itself.
3. The JSON `inlines` list has the exact call sites, with `decision` set to
   `frameless`, `embedded` or `call`. A `call` means the callee was not inlined.

### Compare two runs

Record one trace with the default settings and one with `--jit-no-inline`. Open
them in two terminals, or open one after the other, and compare the bands and the
`instructions` split per tier in the summary line.

## 6. Reading the numbers

- Times are wall-clock milliseconds from the start of backend setup. The title shows
  the run's wall time.
- Compile bars are real time on compiler worker threads. They overlap execution and
  do not add to the run's time.
- The instruction split per tier in the summary line counts logical bytecode
  instructions, including inlined bodies, charged to the tier that executed them.
- A debug build (`cargo build`) is much slower. For performance questions, use
  `--release`.

## 7. JSON trace reference

The `trace` member of the report is the only part the viewer reads. Other members
(`title`, `sections`, `inspection`) are for people.

```json
{
  "trace": {
    "version": 1,
    "mode": "trace",
    "engine": "jit",
    "status": "Finished",
    "wall_ms": 183.918,
    "dropped_events": 0,
    "routines": [
      { "index": 1, "name": "merge_sort", "calls": 1599, "instructions": 641390,
        "osr": 12, "exec_ms": 176.897,
        "engines": { "interpreter": 14181, "winch": 455744, "cranelift": 171465, "cranelift_speed": 0 } }
    ],
    "events": [ ... ],
    "inlines": [ ... ]
  }
}
```

- `version` must be `1`. The viewer rejects other versions.
- `mode` is `trace` or `summary`. A summary has `osr`, `exec_ms` and `engines` set
  to `null`, and no VM entry events.
- `dropped_events` counts observer events that were not kept. The counts in
  `routines` remain complete.

Each event has `at_ms`, `kind`, `routine` (index), `name` and `detail`. The rest of its
fields depend on `kind`:

| `kind` | Extra fields |
| --- | --- |
| `request` | `tier`, `executed` (instructions when requested, or `null` for a manual request), `size` (bytecodes) |
| `supersede` | `from`, `to` |
| `compile_start` | `tier`, `worker`, `queued_ms` |
| `compile_end` | `tier`, `duration_ms`, `fallback` (or `null`) |
| `install` | `from` (or `null`), `tier` |
| `discard` | `tier`, `installed` |
| `failed` | `error` |
| `admit` | `waited_ms`, `compiled` |
| `enter` | `from` (or `null`), `to`, `invocation`, `pc`, `depth`, `version` |
| `handoff` | same as `enter` |
| `osr` | same as `enter`, plus `ready_ms` when the target code had already finished compiling |

`enter` and `handoff` also carry `live_slots` when the target is speed code.

Levels are `interpreter`, `winch`, `cranelift` and `cranelift_speed`.

Each `inlines` entry has `routine`, `routine_name`, `pc`, `callee`, `callee_name`,
`decision` (`frameless`, `embedded` or `call`), `cost` (words) and `depth`.

## 8. Troubleshooting

| Message | Cause and fix |
| --- | --- |
| `no trace member; write the report with --jit-trace --jit-trace-format=json` | The file came from a plain run, from `--jit-trace` with the human format, or from `--jit-trace-format=json` without `--jit-trace`. Record again with the flags in section 1. |
| `trace member ... is missing or has the wrong type` | The file was edited, or written by a different version of cyallo. Record it again. |
| `trace version N is not supported` | Same as above. This viewer reads version 1. |
| `invalid JSON at byte N` | The file is truncated or has extra text. Common cause: a REPL session, which writes one JSON document per entry. Record with `cyallo run` instead. |
| `--jit-trace-out needs --jit-trace or --jit-trace=summary` | Add one of the trace flags. Without one, no report is produced, so there is nothing to write. |
| `cannot write PATH` | The directory does not exist or is not writable. |
| `cyallo trace is interactive; run it in a terminal` | Stdin or stdout is not a terminal. Run the command directly in a terminal, not in a pipeline or a script. |
| Glyphs appear as question marks or boxes | The terminal font lacks box-drawing and geometric symbols (`▲ ◆ ◇ ✖ ━ ┄ █ ▒`). Use a font with broad Unicode coverage. |
| Title or events panel is cut off | The terminal is narrower than about 120 columns. Widen it. |

## 9. Limits

- The viewer shows one run per file, and does not refresh. Record again to see new
  behavior.
- Inline markers are approximate. The compiler does not record when it made an
  inline decision, so the marker goes where the routine first ran native code.
- Markers that land in the same column are drawn once, by precedence: failure, then
  install, then OSR, then inline. The events panel still lists everything near the
  cursor.
- A summary trace has compile and install events, but no VM entries or OSR.
- The viewer has no mouse support.

## See also

- [JIT reports and tiers](../README.md#bytecode-and-jit-tiers) for the human report.
- [Inspection and admission](jit-inspection-and-admission.md) for compiler notices.
- [Routine optimization results](routine-optimization-results.md) for measured
  effects of inlining.
