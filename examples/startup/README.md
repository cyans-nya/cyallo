# Startup baseline workloads

Generated Cyallo programs for measuring fresh-process startup; the generator and
measurement commands are in [`scripts/startup_baseline.py`](../../scripts/startup_baseline.py),
and the results in [`docs/startup-baseline.md`](../../docs/startup-baseline.md).
Regenerate this directory with:

```sh
uv run scripts/startup_baseline.py generate examples/startup
```

Every program prints `ready`, then waits for one input line, then runs its
variant action. A harness times the first byte of output, sends the line, and
times the exit, so work done before the first output and work forced by the
call after it are measured separately.

Directory names read `<kind>-n<routines>-b<statements per routine>-m<extra files>`:

| Kind | After the input line |
| --- | --- |
| `unused` | prints `done`; the N routines are never called |
| `unused_big` | same, with one 1500-statement routine present and uncalled |
| `call_one` | calls the large routine once |
| `call_all` | calls every routine once through a single routine `run_all` |

Run one with, for example,
`echo go | cargo run --release -- run examples/startup/call_one-n1024-b8-m0/main.ya`.
`unused`, `unused_big` and `call_one` print `done` or the large routine's result;
`call_all` prints a checksum. The full measured matrix is generated into a
temporary directory by the `bench` and `profile` commands.
