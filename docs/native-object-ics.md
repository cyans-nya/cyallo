# Native object inline caches

> **Legacy engine (2026-11-20).** This describes the tiered engine in `src/runtime` and `src/backend` (everything outside `src/backend/verified`), which the verified pipeline replaces. It stays accurate while that engine exists and is retired with it in the [legacy removal plan](legacy-removal-plan.md). Current native-tier work: [native tiers: next steps](native-tiers-next-steps.md).

Winch and both Cranelift modes use Session-owned shape/slot feedback. Generated
code guards the receiver, shape, computed key identity, slot extent and
stored/input representation. Four cases are retained per site; other cases use
the checked dynamic operation.

Supported scalar reads (`i64`, `f64`, `bool`, `char`) load directly from native
property slots. Same-representation scalar writes use a narrow commit helper
after the generated guard. It materializes pending work, commits the scalar,
updates its mirror/storage version, and preserves the successor PC and fuel.
Owned values and representation transitions retain the general path, including
clone/drop behavior and typed failures. Object-handle copies use native slots.

Native views contain no owning guest references. They are rebuilt after moving
GC and refreshed after shape/contract changes. Executables embed private ABI
offsets, never Session-local shape IDs or heap pointers. Computed-key guards
retain the observed string allocation, preventing address reuse from accepting
a different key. Equal strings at different allocations take exact content lookup.

Cache arrays are allocated lazily for routines with eligible property operations.
Programs without objects do not allocate object-slot mirrors. Caches survive
ordinary calls and publication; REPL relinking clears affected feedback.
Independent Sessions retain independent domains, including when IDs coincide.

`Session::object_stats()` reports generated read hits, write commits, guard
misses, helper lookups, shape misses and misses beyond four shapes.
`set_object_ic_mode(ObjectIcMode::Disabled)` provides an explicit verification
control. Both interfaces use instance-owned state.

## Verification and fuzzing

Run `cargo test --test object_fuzz --all-features`. The seeded campaign generates
32 programs with required fields and configurable tails, additions, deletions,
retyping, offset shifts, aliases, Unicode keys, GC/RC payloads and callable
properties. It covers completion and typed missing-key, readonly and required-type
failures. It compares output, typed failures and exact instruction counts for:

- Eager and deferred checking.
- Winch, Cranelift fast and Cranelift speed.
- Enabled and disabled ICs.
- GC stress and one-instruction/137-instruction handoffs.

There are 384 native runs against 64 interpreter baselines. Enabled runs must
record actual native read/write hits, guard misses and misses beyond four
shapes; disabled runs require
zero hits. Failures print the seed and complete generated Cyallo source.

Additional cases cover scalar tags, saved generator roots, independent Sessions
with equal shape IDs but different field orders, and publication followed by
native recompilation. Publication stays append-only. Existing artifacts use
interpreter fallback for descendant snapshots until rebuilt; automatic native
compilation after publication remains separate work.

The campaign found and fixed a cache-lifetime bug: preparing a caller truncated
the cache vector and discarded its callees' feedback. It also exposed the
numeric-cast arm masking checked conversions from `dynamic`.

## Measured scope

The internal benchmark runs [native-ic.ya](../examples/objects/native-ic.ya):
200,000 writes and 200,000 reads of one typed field. Samples use fresh Sessions
and the same compiled artifact. Initial artifact compilation is outside the timer; tiered promotion work may
occur during execution. Reproduce the
debug-runtime measurements with:

```sh
cargo bench --bench objects --profile dev
```

Local aarch64 results, five-sample medians, seconds:

| Engine | ICs disabled | ICs enabled |
| --- | ---: | ---: |
| Tiered | 0.873572 | 0.162472 |
| Cranelift fast | 0.894496 | 0.160196 |
| Cranelift speed | 0.877618 | 0.162070 |

Enabled samples recorded 199,999 read hits, 199,999 write commits and two misses.
Disabled samples recorded zero hits and 400,000 misses. This measures a scalar,
single-shape loop with an unoptimized Rust runtime, and does not establish a
release-runtime or general application speedup. Use `cargo bench --bench objects`
for a release-runtime comparison.

A separate three-sample debug CLI comparison, including startup and compilation,
measured before/after medians of 1.138/0.189 s (tiered), 1.164/0.217 s (fast), and
1.172/0.230 s (speed). Interpreter medians were 1.315/1.326 s. These are local
observations, not cross-machine benchmarks.

Direct stores without the commit helper, owned/non-scalar handlers, speculative
call specialization, unboxed canonical storage and perfect-hash plans remain
future work. Their performance is not inferred from these measurements.
