#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["typer>=0.12", "rich>=13"]
# ///
"""Native tier timing samples under different machine conditions.

    uv run scripts/tier_samples.py run efficient      # then switch the power mode
    uv run scripts/tier_samples.py run performance
    uv run scripts/tier_samples.py compare efficient performance

`run` repeats the ignored test `tests/native_tier_samples.rs`, keeps the median of
each metric, and records the power state macOS reports (read-only `pmset -g`).
Results are stored in target/tier-samples/<label>.json. `compare` prints the
metrics side by side and the tier-up threshold each label implies:
N(s) = cranelift_ll_emit_us(s) * 1000 / (winch_ns - cranelift_ll_ns) instructions,
using the scalar loop's gap.
"""

from __future__ import annotations

import json
import os
import statistics
import subprocess
import sys
import time
from pathlib import Path

import typer
from rich.console import Console
from rich.table import Table

app = typer.Typer(add_completion=False)
console = Console()
ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "target" / "tier-samples"


def power_state() -> dict[str, str]:
    """Power facts from macOS, when available. Reads only; changes nothing."""
    facts: dict[str, str] = {}
    for command in (["pmset", "-g"], ["pmset", "-g", "batt"], ["pmset", "-g", "therm"]):
        try:
            text = subprocess.run(command, capture_output=True, text=True, timeout=10).stdout
        except (OSError, subprocess.TimeoutExpired):
            continue
        for line in text.splitlines():
            lowered = line.lower()
            if any(key in lowered for key in ("lowpowermode", "powermode", "power source", "cpu_speed_limit", "thermal")):
                facts[" ".join(command[1:])] = facts.get(" ".join(command[1:]), "") + line.strip() + "; "
    return facts


def sample_once(path: Path) -> dict:
    env = {**os.environ, "TIER_SAMPLES_OUT": str(path)}
    subprocess.run(
        ["cargo", "test", "--release", "--offline", "--locked", "--test", "native_tier_samples", "--", "--ignored", "--nocapture"],
        cwd=ROOT, env=env, check=True, capture_output=True, text=True,
    )
    return json.loads(path.read_text())


def median_of(runs: list[dict]) -> dict:
    merged = json.loads(json.dumps(runs[0]))
    for index, item in enumerate(merged["runs"]):
        for tier in item["ns_per_instruction"]:
            item["ns_per_instruction"][tier] = statistics.median(r["runs"][index]["ns_per_instruction"][tier] for r in runs)
    for index, item in enumerate(merged["emits"]):
        for tier in item["emit_us"]:
            item["emit_us"][tier] = statistics.median(r["emits"][index]["emit_us"][tier] for r in runs)
    return merged


@app.command()
def run(label: str, repeats: int = typer.Option(5, help="Repetitions; medians are kept"),
        pause: float = typer.Option(0.0, help="Seconds to wait between repetitions")) -> None:
    """Sample the tiers and store the result under LABEL."""
    OUT.mkdir(parents=True, exist_ok=True)
    before = power_state()
    results = []
    for index in range(repeats):
        results.append(sample_once(OUT / f"{label}.{index}.json"))
        console.print(f"[dim]sample {index + 1}/{repeats} done[/dim]")
        if pause and index + 1 < repeats:
            time.sleep(pause)
    merged = median_of(results)
    merged["label"] = label
    merged["time"] = time.strftime("%Y-%m-%d %H:%M:%S")
    merged["power_before"] = before
    merged["power_after"] = power_state()
    (OUT / f"{label}.json").write_text(json.dumps(merged, indent=2) + "\n")
    console.print(f"stored [bold]{OUT / (label + '.json')}[/bold]")
    for key, value in before.items():
        console.print(f"[dim]{key}: {value}[/dim]")


@app.command()
def compare(labels: list[str]) -> None:
    """Show stored samples side by side."""
    data = [json.loads((OUT / f"{label}.json").read_text()) for label in labels]
    table = Table(title="ns per executed instruction")
    table.add_column("program / tier")
    for label in labels:
        table.add_column(label, justify="right")
    for index, item in enumerate(data[0]["runs"]):
        for tier in item["ns_per_instruction"]:
            row = [f"{item['program']} / {tier}"]
            for d in data:
                row.append(f"{d['runs'][index]['ns_per_instruction'][tier]:.3f}")
            table.add_row(*row)
    console.print(table)
    table = Table(title="emit time (us)")
    table.add_column("sites / tier")
    for label in labels:
        table.add_column(label, justify="right")
    for index, item in enumerate(data[0]["emits"]):
        for tier in item["emit_us"]:
            row = [f"{item['sites']} / {tier}"]
            for d in data:
                row.append(f"{d['emits'][index]['emit_us'][tier]:.1f}")
            table.add_row(*row)
    console.print(table)
    table = Table(title="implied promotion threshold (executed instructions in the function)")
    table.add_column("sites")
    for label in labels:
        table.add_column(f"{label} (gap ns)", justify="right")
    gaps = []
    for d in data:
        ns = d["runs"][0]["ns_per_instruction"]
        gaps.append(ns["winch"] - ns["cranelift_ll"])
    for index, item in enumerate(data[0]["emits"]):
        row = [str(item["sites"])]
        for d, gap in zip(data, gaps):
            threshold = d["emits"][index]["emit_us"]["cranelift_ll"] * 1000 / gap
            row.append(f"{threshold:,.0f} ({gap:.2f})")
        table.add_row(*row)
    console.print(table)
    for label, d in zip(labels, data):
        console.print(f"[bold]{label}[/bold] {d['time']}: {d['power_before']}")


if __name__ == "__main__":
    app()
