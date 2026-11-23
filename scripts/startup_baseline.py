#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["typer>=0.12", "rich>=13"]
# ///
"""Startup baseline: generate Cyallo workloads, time fresh processes, attribute stages.

    uv run scripts/startup_baseline.py generate examples/startup
    uv run scripts/startup_baseline.py bench --cyallo target/release/cyallo
    uv run scripts/startup_baseline.py profile --cyallo target-profile/release/cyallo

`bench` needs a plain release build. `profile` needs a build made with
`--features startup-profile` (for example `--target-dir target-profile`).
Both start a new process for every sample. This revision keeps no persistent
compiler cache (the module cache lives inside one process), so every sample
starts with an empty cache; the script never passes a cache directory.
"""

from __future__ import annotations

import json
import os
import platform
import random
import shutil
import statistics
import subprocess
import tempfile
import time
from dataclasses import asdict, dataclass, field
from enum import Enum
from pathlib import Path
from typing import Annotated

import typer
from rich.console import Console
from rich.table import Table

app = typer.Typer(add_completion=False, no_args_is_help=True)
console = Console()


class Kind(str, Enum):
    """What the workload does after the first output."""

    unused = "unused"  # N unused routines; entry never calls them
    unused_big = "unused_big"  # as unused, plus one large routine never called
    call_one = "call_one"  # as unused_big, then calls the large routine once
    call_all = "call_all"  # N routines, all called once through a deferred routine


class Engine(str, Enum):
    default = "default"
    ng = "ng"
    legacy_interpreter = "legacy_interpreter"
    legacy_jit = "legacy_jit"


@dataclass(frozen=True)
class Workload:
    kind: Kind
    routines: int  # N: routines excluding the large one
    body: int = 8  # statements per ordinary routine
    modules: int = 0  # M: extra files holding the routines (0: all in the entry)
    big: int = 1500  # statements in the large routine

    @property
    def label(self) -> str:
        return f"{self.kind.value} n={self.routines} b{self.body} m{self.modules}"

    @property
    def slug(self) -> str:
        return f"{self.kind.value}-n{self.routines}-b{self.body}-m{self.modules}"


# --------------------------------------------------------------------------- generation


def statements(count: int, seed: int) -> str:
    """Deterministic integer arithmetic with branches; every statement is reachable."""
    lines = []
    for s in range(count):
        step = seed + s
        match s % 4:
            case 0:
                lines.append(f"    a = (a * 3 + {step % 97}) % 1000003")
            case 1:
                lines.append(f"    if a > {1000000 + step} {{\n        a = a - {step % 89 + 1}\n    }}")
            case 2:
                lines.append(f"    b = (b + a % {step % 7 + 2}) % 1009")
            case _:
                lines.append(f"    a = (a + b * {step % 5 + 1}) % 1000003")
    return "\n".join(lines)


def routine(name: str, count: int, seed: int, public: bool) -> str:
    return (
        f"{'pub ' if public else ''}fn {name}(x: i64) -> i64 {{\n"
        f"    let mut a = x + {seed}\n    let mut b = 1\n"
        f"{statements(count, seed)}\n    return a + b\n}}\n"
    )


def render(w: Workload) -> dict[str, str]:
    """Return {relative path: source}. `main.ya` is the entry."""
    has_big = w.kind is not Kind.unused and w.kind is not Kind.call_all
    files: dict[str, list[str]] = {"main.ya": []}
    homes: list[str] = []  # module name or "" for each routine
    for k in range(w.routines):
        home = f"m{k % w.modules}" if w.modules else ""
        homes.append(home)
        files.setdefault(f"{home}.ya" if home else "main.ya", []).append(
            routine(f"r{k}", w.body, k, public=bool(home))
        )
    entry = files["main.ya"]
    prefix = [f"mod m{j};" for j in range(w.modules)]
    if w.kind is Kind.call_all:
        calls = [
            f"    total = (total + {f'crate::{h}::' if h else ''}r{k}({k})) % 1000003"
            for k, h in enumerate(homes)
        ]
        entry.append("fn run_all() -> i64 {\n    let mut total = 0\n" + "\n".join(calls) + "\n    return total\n}\n")
    if has_big:
        entry.append(routine("big", w.big, 7, public=False))
    # One real import per module keeps import resolution in play without naming every routine.
    uses = [f"use crate::m{j}::r{j}" for j in range(min(w.modules, w.routines))]
    action = {
        Kind.call_one: "std::io::print(big(3))",
        Kind.call_all: "std::io::print(run_all())",
    }.get(w.kind, 'std::io::print("done")')
    tail = [
        'let mut answer = ""',
        'std::io::print("ready")',
        "std::runtime::caie::input(&mut answer)",
        action,
    ]
    out = {}
    for name, parts in files.items():
        if name == "main.ya":
            head = "\n".join(prefix + uses) + ("\n" if prefix or uses else "")
            out[name] = head + "\n".join(parts) + "\n" + "\n".join(tail) + "\n"
        else:
            out[name] = "\n".join(parts)
    return out


def write(w: Workload, directory: Path) -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    for name, text in render(w).items():
        (directory / name).write_text(text)
    return directory / "main.ya"


COMMITTED = [
    Workload(Kind.unused, 0),
    Workload(Kind.unused, 256),
    Workload(Kind.unused, 4096),
    Workload(Kind.unused, 1024, body=64),
    Workload(Kind.unused, 1024, modules=16),
    Workload(Kind.call_one, 1024),
    Workload(Kind.call_all, 256),
]

# The measured matrix: each sweep varies one factor and holds the rest at the default.
SWEEPS: dict[str, list[Workload]] = {
    "unused-routine count": [Workload(Kind.unused, n) for n in (0, 256, 1024, 4096, 16384)],
    "body size (statements), n=1024": [Workload(Kind.unused, 1024, body=b) for b in (2, 8, 32, 128)],
    "import count (files), n=1024": [Workload(Kind.unused, 1024, modules=m) for m in (0, 4, 16, 64)],
    "first call of one large routine": [
        Workload(k, n) for n in (0, 1024, 4096) for k in (Kind.unused_big, Kind.call_one)
    ],
    "call every routine": [Workload(k, n) for n in (256, 1024, 4096) for k in (Kind.unused, Kind.call_all)],
}


@app.command()
def generate(
    out: Annotated[Path, typer.Argument(help="Directory to receive one subdirectory per workload")],
) -> None:
    """Write the committed example set (the bench command generates its full matrix itself)."""
    for w in COMMITTED:
        entry = write(w, out / w.slug)
        console.print(f"{entry}  [dim]{w.label}[/dim]")


# --------------------------------------------------------------------------- measurement


@dataclass
class Sample:
    first_output_ms: float  # process launch to first byte on stdout
    after_output_ms: float  # first byte to exit, covering the call after the pause
    total_ms: float


def run_once(
    cyallo: Path, entry: Path, jobs: int, engine: Engine, env: dict[str, str] | None = None
) -> tuple[Sample, str]:
    cmd = [str(cyallo), "run", str(entry), "--jobs", str(jobs)]
    if engine != Engine.default:
        cmd.extend(["--engine", engine.value])
    started = time.perf_counter()
    proc = subprocess.Popen(
        cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env
    )
    assert proc.stdin and proc.stdout and proc.stderr
    first = os.read(proc.stdout.fileno(), 1)
    t_first = time.perf_counter()
    if not first:
        raise RuntimeError(f"no output from {cmd}: {proc.stderr.read().decode()}")
    proc.stdin.write(b"go\n")
    proc.stdin.close()
    proc.stdout.read()
    stderr = proc.stderr.read().decode()
    code = proc.wait()
    t_end = time.perf_counter()
    if code != 0:
        raise RuntimeError(f"exit {code} from {cmd}: {stderr}")
    return Sample((t_first - started) * 1e3, (t_end - t_first) * 1e3, (t_end - started) * 1e3), stderr


def percentile(values: list[float], q: float) -> float:
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, round(q * (len(ordered) - 1)))]


@app.command()
def bench(
    cyallo: Annotated[Path, typer.Option(help="Plain release binary")] = Path("target/release/cyallo"),
    engine: Annotated[Engine, typer.Option(help="default omits --engine")] = Engine.default,
    reps: Annotated[int, typer.Option(help="Paired rounds; each round runs every cell once")] = 30,
    jobs: Annotated[str, typer.Option(help="Comma list of --jobs values (0: all cores)")] = "1,0",
    sweep: Annotated[list[str] | None, typer.Option(help="Only sweeps whose name contains this")] = None,
    seed: int = 20261119,
    out: Annotated[Path | None, typer.Option(help="Write raw samples and summary as JSON")] = None,
) -> None:
    """Randomized paired fresh-process runs: median and p95 time to first output."""
    rng = random.Random(seed)
    chosen = {n: ws for n, ws in SWEEPS.items() if not sweep or any(s in n for s in sweep)}
    cells = list(dict.fromkeys(w for ws in chosen.values() for w in ws))
    control = Workload(Kind.unused, 0)
    cells = [control] + [c for c in cells if c != control]
    job_values = [int(j) for j in jobs.split(",")]
    root = Path(tempfile.mkdtemp(prefix="cyallo-startup-"))
    entries = {w: write(w, root / w.slug) for w in cells}
    results: dict[tuple[Workload, int], list[Sample]] = {(w, j): [] for w in cells for j in job_values}
    pairs = [(w, j) for w in cells for j in job_values]
    try:
        for _ in range(2):  # warm the binary and the filesystem cache; not recorded
            run_once(cyallo, entries[control], job_values[0], engine)
        for rep in range(reps):
            rng.shuffle(pairs)  # fresh random order each round, control included
            for w, j in pairs:
                results[w, j].append(run_once(cyallo, entries[w], j, engine)[0])
            console.print(f"round {rep + 1}/{reps}", end="\r")
    finally:
        shutil.rmtree(root, ignore_errors=True)
    console.print()
    meta = {
        "host": platform.platform(),
        "machine": platform.machine(),
        "cpus": os.cpu_count(),
        "revision": git("rev-parse", "HEAD"),
        "binary": str(cyallo),
        "engine": engine.value,
        "reps": reps,
        "filesystem_cache": "warm (sources written immediately before timing)",
        "compiler_cache": "empty (no persistent cache exists; every sample is a fresh process)",
    }
    summary = []
    for j in job_values:
        for name, ws in chosen.items():
            table = Table(title=f"{name}  (--jobs {j}, {reps} paired rounds)")
            for column in ("workload", "first out med", "p95", "Δ vs n=0 med", "after-output med", "p95", "total med"):
                table.add_column(column, justify="left" if column == "workload" else "right")
            base = [s.first_output_ms for s in results[control, j]]
            for w in ws:
                samples = results[w, j]
                first = [s.first_output_ms for s in samples]
                after = [s.after_output_ms for s in samples]
                total = [s.total_ms for s in samples]
                # Paired difference against the control sample from the same round.
                delta = statistics.median(f - b for f, b in zip(first, base))
                row = {
                    "sweep": name,
                    "jobs": j,
                    "workload": asdict(w) | {"kind": w.kind.value},
                    "first_median_ms": statistics.median(first),
                    "first_p95_ms": percentile(first, 0.95),
                    "first_delta_vs_control_ms": delta,
                    "after_median_ms": statistics.median(after),
                    "after_p95_ms": percentile(after, 0.95),
                    "total_median_ms": statistics.median(total),
                }
                summary.append(row)
                table.add_row(
                    w.label,
                    f"{row['first_median_ms']:.2f}",
                    f"{row['first_p95_ms']:.2f}",
                    f"{delta:+.2f}",
                    f"{row['after_median_ms']:.2f}",
                    f"{row['after_p95_ms']:.2f}",
                    f"{row['total_median_ms']:.2f}",
                )
            console.print(table)
    if out:
        raw = {f"{w.slug}@jobs{j}": [asdict(s) for s in ss] for (w, j), ss in results.items()}
        out.write_text(json.dumps({"meta": meta, "summary": summary, "samples": raw}, indent=1) + "\n")
        console.print(f"wrote {out}")


def git(*args: str) -> str:
    return subprocess.run(["git", *args], capture_output=True, text=True).stdout.strip()


# --------------------------------------------------------------------------- attribution

# Stages whose parents already include them; shown indented in the report.
CHILDREN = {
    "core_package_parse": ("lex", "parse"),
    "resolve": ("core_package_parse",),
    "module_analysis": ("module_declarations", "module_keys", "module_body_wave", "module_link", "requires_tracing"),
    "module_body_wave": ("deferred_setup", "entry_lowering"),
    "body_analysis": ("body_lower", "header_clone", "body_effects", "body_finish"),
}


@app.command()
def profile(
    cyallo: Annotated[Path, typer.Option(help="Binary built with --features startup-profile")] = Path(
        "target-profile/release/cyallo"
    ),
    engine: Annotated[Engine, typer.Option(help="default omits --engine")] = Engine.default,
    reps: int = 15,
    jobs: Annotated[int, typer.Option(help="Use 1: stage times then equal elapsed time")] = 1,
    sweep: Annotated[list[str] | None, typer.Option(help="Only sweeps whose name contains this")] = None,
    out: Annotated[Path | None, typer.Option(help="Write per-workload medians as JSON")] = None,
) -> None:
    """Median per-stage time, allocation and work counts from the instrumented build."""
    chosen = {n: ws for n, ws in SWEEPS.items() if not sweep or any(s in n for s in sweep)}
    root = Path(tempfile.mkdtemp(prefix="cyallo-profile-"))
    env = os.environ | {"CYALLO_STARTUP_PROFILE": "json"}
    report: dict[str, dict] = {}
    try:
        for name, ws in chosen.items():
            for w in ws:
                if w.slug in report:
                    continue
                entry = write(w, root / w.slug)
                runs = []
                for _ in range(reps):
                    _, err = run_once(cyallo, entry, jobs, engine, env)
                    report_start = err.find('{"end":')
                    if report_start == -1:
                        raise RuntimeError(f"no startup profile for {w.label} with {engine.value}: {err}")
                    profile_run, _ = json.JSONDecoder().raw_decode(err[report_start:])
                    if "first_output" not in profile_run:
                        raise RuntimeError(f"no first-output snapshot for {w.label} with {engine.value}")
                    runs.append(profile_run)
                report[w.slug] = summarize(w, runs) | {"engine": engine.value, "jobs": jobs}
    finally:
        shutil.rmtree(root, ignore_errors=True)
    for name, ws in chosen.items():
        stage_table(name, [(w, report[w.slug]) for w in ws])
    if out:
        out.write_text(json.dumps(report, indent=1) + "\n")


def med(runs: list[dict], *path: str) -> float:
    values = []
    for run in runs:
        node = run
        for key in path:
            node = node.get(key, 0) if isinstance(node, dict) else 0
        values.append(node)
    return statistics.median(values)


def summarize(w: Workload, runs: list[dict]) -> dict:
    stages = runs[0]["stages"].keys()
    counters = runs[0]["end"]["counters"].keys()
    return {
        "workload": w.label,
        "stage_ms": {s: med(runs, "stages", s, "ns") / 1e6 for s in stages},
        "stage_ms_first_output": {s: med(runs, "first_output", "stage_ns", s) / 1e6 for s in stages},
        "stage_calls": {s: med(runs, "stages", s, "calls") for s in stages},
        "stage_alloc_bytes": {s: med(runs, "stages", s, "alloc_bytes") for s in stages},
        "counters_end": {c: med(runs, "end", "counters", c) for c in counters},
        "counters_first_output": {c: med(runs, "first_output", "counters", c) for c in counters},
        "first_output_ms": med(runs, "first_output", "elapsed_ns") / 1e6,
        "end_ms": med(runs, "end", "elapsed_ns") / 1e6,
        "peak_live_bytes": med(runs, "peak_live_bytes"),
        "total_alloc_bytes": med(runs, "end", "allocated_bytes"),
    }


STAGE_ROWS = [  # (stage, indent)
    ("lex", 0), ("parse", 0), ("freeze", 0), ("publish", 0), ("materialize", 0),
    ("resolve", 0), ("core_package_parse", 1),
    ("specialize", 0), ("const_eval", 0),
    ("module_analysis", 0), ("module_declarations", 1), ("module_keys", 1), ("module_body_wave", 1),
    ("deferred_setup", 2), ("entry_lowering", 2), ("requires_tracing", 1), ("module_link", 1),
    ("bytecode_module", 0),
    ("body_analysis", 0), ("body_lower", 1), ("header_clone", 1), ("body_effects", 1), ("body_finish", 1),
    ("bytecode_translate", 0),
    ("verified_module_lower", 0), ("verified_body_lower", 1),
]
SHOWN_COUNTERS = ["bodies_parsed", "bodies_frozen", "bodies_materialized", "bodies_resolved",
                  "bodies_deferred_copied", "bodies_tracing_copied", "bodies_checked", "bodies_translated",
                  "header_elements_cloned", "verified_bodies_lowered"]


def short(label: str) -> str:
    return label.replace("unused_big", "ubig").replace("call_one", "one").replace("call_all", "all").replace(
        "unused", "unu"
    )


def stage_rows(rows: list[tuple[str, dict]]) -> tuple[list[str], list[list[str]]]:
    """Header and rows with workloads as columns, so wide sweeps stay readable."""
    head = ["median ms"] + [short(label) for label, _ in rows]
    body = [
        ["\u00a0" * 2 * depth + stage]
        + [f"{r['stage_ms'][stage]:.2f}" if stage in r["stage_ms"] else "—" for _, r in rows]
        for stage, depth in STAGE_ROWS
    ]
    body.append(["time to first output"] + [f"{r['first_output_ms']:.2f}" for _, r in rows])
    body.append(["time to exit"] + [f"{r['end_ms']:.2f}" for _, r in rows])
    body.append(["peak live MB"] + [f"{r['peak_live_bytes'] / 1e6:.1f}" for _, r in rows])
    body.append(["allocated MB (total)"] + [f"{r['total_alloc_bytes'] / 1e6:.1f}" for _, r in rows])
    for c in SHOWN_COUNTERS:
        body.append(
            [f"{c} (first out / exit)"]
            + [
                f"{r['counters_first_output'][c]:.0f} / {r['counters_end'][c]:.0f}"
                if c in r["counters_end"]
                else "—"
                for _, r in rows
            ]
        )
    return head, body


def stage_table(title: str, rows: list[tuple[Workload, dict]]) -> None:
    head, body = stage_rows([(w.label, r) for w, r in rows])
    table = Table(title=f"{title} (stages are inclusive; resolve includes core parse)")
    for h in head:
        table.add_column(h, justify="left" if h == head[0] else "right")
    for row in body:
        table.add_row(*row)
    console.print(table)


def markdown(head: list[str], body: list[list[str]]) -> str:
    lines = ["| " + " | ".join(head) + " |", "|" + "|".join(["---"] + ["---:"] * (len(head) - 1)) + "|"]
    lines += ["| " + " | ".join(row) + " |" for row in body]
    return "\n".join(lines)


@app.command()
def report(
    profile_json: Path = Path("docs/measurements/startup-baseline-profile.json"),
    bench_json: Path = Path("docs/measurements/startup-baseline-bench.json"),
) -> None:
    """Print the measurements as Markdown tables (the source of docs/startup-baseline.md)."""
    profile = json.loads(profile_json.read_text())
    for name, ws in SWEEPS.items():
        rows = [(w.label, profile[w.slug]) for w in ws if w.slug in profile]
        if rows:
            print(f"\n#### Stages: {name}\n")
            print(markdown(*stage_rows(rows)))
    if bench_json.exists():
        bench = json.loads(bench_json.read_text())
        print(f"\n#### Time to first output\n\n`{bench['meta']}`\n")
        head = ["sweep", "jobs", "workload", "first out med", "first out p95", "Δ vs n=0 (paired)", "after-output med", "after-output p95", "exit med"]
        body = [
            [r["sweep"], str(r["jobs"]), short(Workload(Kind(r["workload"]["kind"]), r["workload"]["routines"], r["workload"]["body"], r["workload"]["modules"]).label),
             f"{r['first_median_ms']:.1f}", f"{r['first_p95_ms']:.1f}", f"{r['first_delta_vs_control_ms']:+.1f}",
             f"{r['after_median_ms']:.1f}", f"{r['after_p95_ms']:.1f}", f"{r['total_median_ms']:.1f}"]
            for r in bench["summary"]
        ]
        print(markdown(head, body))


if __name__ == "__main__":
    app()
