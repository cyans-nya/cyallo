# Winch import

`winch-codegen` is the crates.io 50.0.1 release at the commit in
[UPSTREAM.toml](UPSTREAM.toml), with its Apache/LLVM-exception license intact.
The published manifest already resolves upstream workspace dependencies; its
original manifest is retained as `Cargo.toml.orig`. The import set excludes
registry bookkeeping and the upstream release lockfile. Our root `Cargo.lock`
pins the complete compiler dependency cohort.

Only the ISA visibility hooks, checked division/load/store hooks, and the
standalone native assembler modules differ from the import. CAIE decoding,
semantics, cache/recovery policy, native storage, scheduling, and OSR live in
`src/backend/winch`, `src/runtime/native.rs`, and `src/osr.rs`.

```sh
cargo xtask vendor-winch check                  # cached archive, offline
cargo xtask vendor-winch check --fetch          # download if absent
cargo xtask vendor-winch import /tmp/winch-copy
cargo xtask vendor-winch update 50.0.1 /tmp/winch-candidate
cargo xtask vendor-winch check-upstream --output /tmp/upstream.md
```

`check` verifies the archive SHA256, series checksum (the `series` file followed
by each patch's bytes in order), reconstructs the explicit import set, applies
exact-context patches with `git apply`, and compares every file with the fork.
Unrecorded edits, extra files, checksum failures, and conflicts fail the check.
`--archive PATH` allows an explicit source archive. `import` refuses to overwrite
an existing destination. `update` produces an isolated candidate/report and
never edits production; conflicts are recorded in that report for resolution.

Each patch carries purpose, subsystem, base, and removal condition. To change
the fork, regenerate the reviewed exact-context diff against the recorded
archive and update its series checksum in the same change. Never reformat the
upstream tree. Remove hooks when upstream provides equivalent APIs.

The weekly workflow reports new tags and changes in emitters, allocation,
frame/ABI, publication, and dependency manifests. It has read-only permissions
and never merges or advances a production pin. For an upgrade, review those
subsystems, update the coherent Winch/Cranelift/internal dependency set, record
the immutable commit and archive checksum, refresh the patch rationale, and
run reconstruction, parity, OSR, error/budget/alias tests and benchmarks on
native ARM64 and x64 hosts. A clean patch apply is insufficient. Correctness or
security fixes may be backported as individually recorded patches. If a gate
fails, retain the current pin and record the blocker in the candidate report.

Local runtime validation currently covers ARM64 macOS. Both ISA seams compile
and emit fixups locally; Linux ARM64/x64 runtime gates are configured in
`.github/workflows/native.yml` and must pass before claiming those hosts have
been validated. Windows x64 ABI emission remains experimental.

Runner labels follow the [GitHub runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).
