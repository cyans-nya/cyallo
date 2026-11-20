# Initial cell GC measurements

> **Legacy engine (2026-11-20).** This describes the tiered engine in `src/runtime` and `src/backend` (everything outside `src/backend/verified`), which the verified pipeline replaces. It stays accurate while that engine exists and is retired with it in the [legacy removal plan](legacy-removal-plan.md). Current native-tier work: [native tiers: next steps](native-tiers-next-steps.md).

Recorded 2026-11-11 on native ARM64 macOS with `cargo bench --bench gc`.
These are medians of nine fresh single-Session samples, in milliseconds.
Parsing/code generation are outside timing; Session setup and initial native
materialization are inside. Winch is explicitly precompiled and frozen;
Cranelift engines compile synchronously before sampling. Reporting is disabled.
GC uses the default 4,096-cell allocation interval, not stress mode.

| Engine | Scalar off/on | Short-lived off/on | Surviving array + copies off/on |
| --- | ---: | ---: | ---: |
| Interpreter | 24.454 / 24.929 | 6.016 / 7.090 | 2.635 / 3.144 |
| Winch | 0.989 / 0.951 | 4.593 / 5.646 | 2.766 / 3.240 |
| Cranelift | 0.730 / 0.739 | 4.907 / 5.857 | 3.593 / 4.078 |
| Cranelift speed | 0.623 / 0.628 | 4.497 / 5.629 | 2.769 / 3.215 |

Scalar performs 100,001 iterations without managed allocations after setup.
Native differences are around run-to-run noise; no per-instruction native root
publication is introduced. The interpreter's extra GC instruction-PC tracking
cost is visible even when no collection is needed.

Short-lived calls construct/copy two-element arrays 5,001 times. GC performed
eight collections, reclaimed 32,760 cells, and reduced peak occupied cells from
35,009 to 4,104; 2,249 cells remained before a final explicit collection. Runtime
cost rose about 18–25%. The copying case retains a 512-element array while making
101 private copies: twelve collections reclaimed 48,316 cells and reduced peak
occupancy from 52,428 to 5,108. Runtime cost rose about 14–19%.

These measurements compare the new implementation with collection enabled and
disabled; they do not establish a pre-change regression baseline or an RSS
bound. Cell counts exclude buffers, roots, native mirrors, and collector scratch.
The 64 MiB managed-byte policy is still pending. Root registration and temporary
snapshot ownership are performance work before default rollout.
