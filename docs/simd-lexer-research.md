# Portable SIMD for the Cyallo lexer

Prototype two local byte scans independently. Scan the body of `//` comments or runs of ignored space, tab, and carriage return. Use nightly `std::simd` inside `cyallo-parser` and keep a scalar reference for differential testing. Comment scanning has the simpler predicate and is the first candidate. Adoption depends on release measurements on representative `.ya` sources. [Cyallo lexer](../crates/cyallo-parser/src/lexer.rs) · [Compiler and runtime rules](compiler-and-runtime-rules.md#readability-and-correctness)

## Bounded byte scans

`lex` currently consumes a UTF-8 `&str` and emits typed tokens with `u32` byte spans and a zero-width EOF token. The parser uses these spans to retrieve source text. `lex_modern` delegates to `lex`, and parse-unit indexing calls `lex_modern` before splitting the token stream. Measure lexing and indexing separately. [Lexer](../crates/cyallo-parser/src/lexer.rs) · [Parse-unit indexing](../crates/cyallo-parser/src/units.rs)

Enable `#![feature(portable_simd)]` in the parser crate for the prototype. Start with `std::simd::u8x16`, use `u8x16::LEN` as the full-block width, and treat 16 lanes as experimental. On a `&[u8]` suffix, load only when at least `LEN` bytes remain. `from_slice` takes the first `LEN` bytes and panics on shorter input. Use `std::simd::cmp::SimdPartialEq` to build a stop mask and `Mask::first_set()` to choose its first true lane. Advance by that lane offset or by the full width when the mask is empty. Scan the remaining bytes with the equivalent scalar predicate. This uses safe slice loads with no alignment or padding requirement. Rust defines the operations across targets, while generated instructions and throughput depend on the target. Cyallo CI already installs nightly and covers both x86-64 and AArch64. [Rust portable SIMD](https://doc.rust-lang.org/nightly/std/simd/index.html) · [`u8x16`](https://doc.rust-lang.org/nightly/std/simd/type.u8x16.html) · [`Simd::from_slice`](https://doc.rust-lang.org/nightly/std/simd/struct.Simd.html#method.from_slice) · [`Mask::first_set`](https://doc.rust-lang.org/nightly/std/simd/struct.Mask.html#method.first_set) · [Native CI](../.github/workflows/native.yml)

| Scanner | Entry and stop condition |
| --- | --- |
| Line comment | Enter after the existing `//` lookahead and skip the two slashes. Stop at the first `b'\n'` or EOF. The comment body can include CR and any other UTF-8 bytes. Leave LF for the outer loop to emit as `Newline`. |
| Ignored whitespace | Enter on the existing space, tab, or CR branch. OR the three equality masks, then invert the result to find the first byte outside the set. Stop before LF, any non-ASCII byte, or any other token byte. Compare against a scalar run loop as well as the current one-byte branch so the benchmark isolates the value of SIMD. |

This reference helper returns the number of comment bytes to skip after `//`.

```rust
use std::simd::{cmp::SimdPartialEq, u8x16};

fn line_comment_len(bytes: &[u8]) -> usize {
    let mut offset = 0;
    let newline = u8x16::splat(b'\n');
    while bytes.len() - offset >= u8x16::LEN {
        let block = u8x16::from_slice(&bytes[offset..]);
        if let Some(lane) = block.simd_eq(newline).first_set() {
            return offset + lane;
        }
        offset += u8x16::LEN;
    }
    while offset < bytes.len() && bytes[offset] != b'\n' {
        offset += 1;
    }
    offset
}
```

Use byte subslices throughout both scans. A comment block can split a UTF-8 scalar internally, but LF can only occur at an ASCII character boundary and EOF is a character boundary. The whitespace scan starts at a boundary and advances over ASCII bytes, so the outer loop resumes at a boundary even when the next byte starts a non-ASCII character. Keep the outer dispatch, punctuation lookahead, literal handling, byte-span limit, and diagnostic construction in the existing scalar lexer. [Rust `str` invariant](https://doc.rust-lang.org/nightly/std/primitive.str.html#invariant) · [Cyallo lexer](../crates/cyallo-parser/src/lexer.rs)

## Byte-oriented source input

The current `&str` entry point inherits a whole-file UTF-8 guarantee from its caller. CLI and module file loaders use `read_to_string`, so an invalid byte in a discarded comment can prevent lexing. Changing the lexer signature alone would leave that loader behavior in place. The SIMD scans work on bytes under either API. [CLI loading](../src/main.rs) · [Module loading](../crates/cyallo-modules/src/loader.rs) · [Lexer](../crates/cyallo-parser/src/lexer.rs)

The byte-oriented proposal uses `&[u8]` for lexing and keeps half-open byte spans. Give each consumer the representation its semantics require.

| Source region | Candidate policy |
| --- | --- |
| ASCII syntax, identifiers and keywords | Scan the complete ASCII word and compare its bytes with the language's keyword spellings. |
| `//` comment body | Accept arbitrary bytes up to LF or EOF. The lexer discards them. Preserve the original bytes in the source snapshot if tools need to display or reproduce the file. |
| `TEXT` and `CHAR` literals | Decode the literal region as UTF-8 under the existing text and Unicode-scalar contracts. Report malformed encoding with a typed diagnostic anchored to its byte span. Preserve escape behavior and the current diagnostic order for valid UTF-8 inputs. |
| Other non-ASCII input | Preserve the existing unexpected-character diagnostic for a valid scalar and give malformed byte sequences their own typed diagnostic. |

### Keyword matching

Modern Cyallo already matches keyword spellings exactly, even though the matcher takes a `&str`. In the byte-oriented lexer, classify the full ASCII word with byte patterns such as `b"if"`. Only that exact spelling is the `if` keyword. `IF` and `if_else` remain identifiers. Apply normalization and case rules only where the language defines them. CAIE compatibility already uses ASCII-only case-insensitive matching, which can also operate directly on byte slices. [Modern keyword classifier](../crates/cyallo-parser/src/token.rs) · [CAIE keyword classifier](../crates/caie-int-parser/src/token.rs)

Test exact keywords, longer identifiers with keyword prefixes, ASCII case variants in both dialects, and non-ASCII lookalikes. Keep Unicode casing, locale-sensitive mappings and normalization out of keyword recognition. The lexer can decode bytes for literal values and diagnostics independently of how it matches keywords.

### Migration and validation

Test the byte-input API independently of SIMD. Keep a `&str` convenience adapter for text callers if useful. To accept raw files end to end, pass bytes through the CLI and module loaders, source snapshots and archives, and expose decoded token text where the parser needs it. Diagnostic line indexing can count LF in raw bytes. Rendering can replace undecodable bytes for display while retaining a mapping from original byte spans to rendered positions. Add tests for malformed bytes before, within and after comments and literals, including at vector boundaries and EOF. Run scalar and SIMD versions under the same proposed policy and compare both with today's lexer on valid UTF-8 inputs. [Source snapshots](../crates/cyallo-parser/src/units.rs) · [Source positions](architecture.md#source-positions-and-bytecode) · [Typed errors](compiler-and-runtime-rules.md#typed-status-and-error-models)

## Other approaches

- **`memchr` as a control.** `rustc_lexer` delegates the LF search in line comments to `memchr`. Benchmark `memchr(b'\n', tail)` alongside the scalar and portable SIMD comment scans. The library provides SIMD implementations for x86-64 and AArch64. Its single-byte API solves the comment search directly. The multi-needle APIs find membership in a small byte set, while ignored whitespace needs the first byte outside the set. [Rust compiler line-comment lexer](https://github.com/rust-lang/rust/blob/main/compiler/rustc_lexer/src/lib.rs#L729-L742) · [memchr 2.8.3 documentation](https://docs.rs/memchr/2.8.3/memchr/)
- **More local scanners.** If profiles show long identifier or digit runs, use lane-wise inclusive ASCII range checks and `_` equality for the existing continuation rules. Measure long runs against the cost of checking short ones. Keep keyword lookup, the lifetime-versus-char branch, decimal lookahead (including `..`, `.*`, and a previous dot), exponent validation, and numeric suffix handling scalar. A later literal experiment could find the first quote, backslash, CR, or LF, then resume scalar processing at that byte so the existing escape and diagnostic rules apply across vector boundaries. [Rust `SimdPartialOrd`](https://doc.rust-lang.org/nightly/std/simd/cmp/trait.SimdPartialOrd.html) · [Cyallo lexer](../crates/cyallo-parser/src/lexer.rs)
- **Target-specific intrinsics or a structural index.** Clang’s block-comment scanner illustrates architecture-specific SIMD for locating candidate slashes before scalar handling. This route requires architecture-specific code and attention to available CPU features. The simdjson paper describes a two-stage structural index with separate backslash and quote detection. A Cyallo prepass would also need to model comments, significant LF, and both uses of single quotes. Revisit either design if profiling shows that the local scans leave a significant lexer bottleneck. [Clang lexer](https://raw.githubusercontent.com/llvm/llvm-project/main/clang/lib/Lex/Lexer.cpp) · [Rust `std::arch`](https://doc.rust-lang.org/nightly/std/arch/index.html) · [Langdale and Lemire, §3.1](https://arxiv.org/html/1902.08318v7#S3.SS1) · [Cyallo lexer](../crates/cyallo-parser/src/lexer.rs)

## Local probe results

I compiled standalone versions of both scans with Rust `1.101.0-nightly (07e526db9)`, LLVM 23.1.4, and optimization level 3 on `aarch64-apple-darwin`. Both SIMD scans agreed with their scalar references on **4,817,680** generated byte slices. The probe varied the suffix start and lengths from zero through three vectors, then substituted every possible byte at each position. The optimized assembly contained vector loads and `cmeq.16b` comparisons in both scans.

The numbers below are median nanoseconds per standalone scanner call on repeated, in-memory synthetic input. Each input contained a run of spaces of the indicated length, followed by LF and trailing bytes. The whitespace probe alternated two variants across eight paired samples of two million calls per variant. The separate comment probe rotated three variants across nine samples of 500,000 calls each and used `memchr` 2.8.3.

| Bytes before LF | Whitespace scalar | Whitespace SIMD | Comment scalar | Comment SIMD | Comment `memchr` |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 | 2.1 | 2.8 | 2.3 | 2.6 | 1.9 |
| 4 | 3.4 | 2.8 | 2.9 | 1.8 | 1.4 |
| 16 | 9.7 | 10.9 | 8.1 | 2.1 | 2.5 |
| 64 | 26.5 | 12.4 | 25.4 | 3.2 | 3.5 |
| 4096 | 1650.8 | 200.6 | 1462.0 | 137.0 | 52.0 |

Both SIMD scans improved long-run throughput in these probes. Short-run costs were mixed and varied with the standalone build. Measure the distribution of actual comment and whitespace lengths before choosing a threshold. The table measures scanner calls on one host. Full lexer parity, throughput on real sources, and startup effects remain open.

## Verification and adoption

1. Compare the candidate with the current lexer on `.ya` sources and generated valid UTF-8. Compare token kinds, spans, source slices, EOF, and diagnostic code, message, and span. Exercise empty input, all-trivia input, LF and CRLF, lone CR inside and outside comments, a comment at EOF, slash operators, and strings containing `//`. Place separators, malformed literals, and non-ASCII bytes before, on, and after vector boundaries. Vary source-prefix alignment and test lengths around one and two vectors, including the final scalar tail. Preserve the smallest source reproducer for any mismatch. [Lexer](../crates/cyallo-parser/src/lexer.rs) · [Compiler and runtime rules](compiler-and-runtime-rules.md#readability-and-correctness)
2. Profile release lexing and file indexing. Benchmark the current lexer, scalar runs, comment SIMD only, whitespace SIMD only, and both together with the same compiler and flags. Include small real files, a larger multi-file source set, and generated cases with short and long comments or whitespace. Compare per-file latency and bytes per second, and record the architecture, `rustc -Vv`, flags, corpus, and repetitions. Inspect optimized code if either target gives surprising results. Check x86-64 and AArch64 before generalizing a speedup. [Existing Cyallo benchmark methodology](benchmarks.md) · [Native CI](../.github/workflows/native.yml)
3. If a variant improves representative lexing without a meaningful small-file regression, test it through the normal parser entry points with `cargo +nightly test -p cyallo-parser --locked`. Use the built release CLI to measure fresh-process time to first output on a `.ya` program with a fixed printing entry. Time full checking separately, including `cyallo check examples/modules/static/main.yas --jobs 4`. Report lexer and CLI results separately. Use the measured workload to justify the final vector width and any minimum run threshold. [Parser entry points](../crates/cyallo-parser/src/lib.rs) · [Module check command](modules.md) · [Compiler and runtime rules](compiler-and-runtime-rules.md#readability-and-correctness)

## Experiment results (2026-11-19)

Host `aarch64-apple-darwin`, Apple M4 Max, `rustc 1.101.0-nightly (07e526db9 2026-10-19)`, LLVM 23.1.4, `cargo +nightly bench -p cyallo-parser --features memchr-control --bench lexer_scan` (release profile defaults, no extra flags; `u8x16`). Variants rotate across 15 samples; medians. The real corpus is every `.ya` under `examples`, `std`, `tests` plus `std.ya`: 87 files, 55 KB, average 640 B. Differential tests (`lexer::tests`) compare token kinds, spans, text and diagnostics for every variant against the original lexer on boundary, edge, 20,000 random and repository sources at every prefix alignment.

Lexer speed relative to the original lexer (>1 is faster; real-source figures vary about ±4% between runs):

| Input | scalar runs | comment SIMD | whitespace SIMD | both SIMD | comment `memchr` |
| --- | ---: | ---: | ---: | ---: | ---: |
| Real `.ya`, per file | 0.97 | 1.02–1.05 | 0.90 | 0.92 | 0.94–0.96 |
| Real, smallest 10 files | 0.94–0.96 | 0.99–1.01 | 0.87–0.92 | 0.92–0.94 | 0.89–0.94 |
| 200 comments of 2 B | 0.91–0.94 | 0.96–1.04 | 0.80–0.91 | 0.78–0.84 | 0.94–1.00 |
| 200 comments of 16 B | 0.95 | 1.5 | 0.92 | 1.0 | 1.25 |
| 200 comments of 64 B | 0.98 | 2.7 | 0.95 | 2.0–2.1 | 2.5 |
| 200 comments of 4096 B | 1.0 | 9.6 | 1.0 | 9.3 | 19.8 |
| Blank runs of 1–4 B | 0.88–1.04 | ~1.0 | 0.81–1.13 | 0.87–1.12 | 0.9–1.0 |
| Blank runs of 16 B | 1.24–1.36 | 1.0 | 1.7–1.8 | 1.4–1.7 | 1.2 |
| Blank runs of 64 B | 1.7 | 0.9–1.0 | 3.7–4.2 | 3.9–4.2 | 1.7 |

A hybrid whitespace scan that returns after one compare for a single blank (`blank_run_len_hybrid`) tracked the real-source baseline within noise but not better.

**Adopted: comment SIMD (plus the perfect-hash keywords below).** It never measurably regresses real or short-comment inputs and is 1.5–10× faster on comments of 16 B or more. Whitespace SIMD regressed real sources by about 10%, because real whitespace is mostly single separators and short indentation, so it is not adopted. `memchr` is a benchmark control only (optional `memchr-control` feature); it is faster than `u8x16` for very long comments but not for the 16–64 B range, and adds a dependency to the parser crate. Real comments are short, so the whole-corpus gain is small (about 2–5%).

CLI fresh-process time (hyperfine `-N`, 300 runs, warmup 20), original lexer vs comment SIMD: `run examples/fib.yas` 23.0 ms vs 23.0 ms; `check examples/modules/static/main.yas --jobs 4` 7.4 ms vs 7.3 ms. No measurable change: lexing 55 KB takes under 0.1 ms, far below process startup. The remaining startup cost is not in the lexer. x86-64 has not been measured.

### Keyword perfect hash and front-end share of startup

The 27 keywords are at most 8 bytes, so a word is packed into a zero-padded little-endian `u64` (identifier bytes are never NUL, so packing is unique). `from_packed_word` multiplies by a searched constant, takes the top 6 bits as the slot, and compares one `u64`. The table is built in a `const` block; a slot collision fails the build. A test checks it against the `match` classifier on keywords, near-keywords and every short identifier-like string, both mid-source and at EOF.

Measured relative to the original lexer (two runs agree within about 3%): real `.ya` 1.10× (comment SIMD alone 1.03×), smallest 10 files 1.07×, largest 5 files 1.13×, concatenated 1.12×; generated comment/blank cases gain 1.0–1.2×, with the long-comment cases unchanged. Whole-corpus time went from about 970 ns to 868 ns per file. This is adopted. Whitespace SIMD and the hybrid remain unadopted: they lose 3–10% on real files (they win only on blank runs of 16 B or more).

Front-end cost on the standard library (`std.ya` plus `std/*.ya`: 25 files, 11.7 KB; `cargo +nightly bench -p cyallo-parser --bench startup_share`, same host): lex 22 µs, lex + parse-unit index 24 µs, full `parse` 1,234 µs, `examples/fib.yas` parse 2.9 µs. Against 23 ms for `run examples/fib.yas` and 7 ms for `check`, the lexer is about 0.1% and indexing about 0.01%. Even a complete parse of all std is about 5% of the `run` wall time (an upper bound; the CLI may not parse all of it). Parsing into the AST costs about 50× lexing, so a further front-end speedup would come from the parser/AST, not the lexer.
