# Cyallo concurrency examples

These are Cyallo `.ya` programs, executed by the Cyallo compiler and VM.

```sh
cargo run -- run examples/concurrency/generators/sequence.ya
cargo run -- run examples/concurrency/generators/resume-result.ya --gc-stress
cargo run -- run examples/concurrency/generators/delegation.ya --engine cranelift_speed
cargo test --test generators
```

| Example | Behavior | Expected output |
| --- | --- | --- |
| [Sequence](generators/sequence.ya) | A capturing factory, lazy construction, lexical yields, unit resume, completion, and idempotent close | `10`, `11`, `12`, `complete: 3` |
| [Result input](generators/resume-result.ya) | An ordinary Result is supplied to the suspended yield | `request`, `complete: 42`, `request`, `error: unavailable`, `complete: -1` |
| [Delegation](generators/delegation.ya) | A pure, capture-free child factory; owned `yield from`; forwarded input and completion | `yielded: 7`, `complete: 42` |

Generator factories declare `Generator<Yield, Resume, Return>`. Calling a factory
provides construction parameters and creates a lazy generator. The first
`resume()` starts it; subsequent `resume(value)` calls supply the yield expression's
result. A unit-input generator can keep using `resume()` or `next()`. Steps expose
`is_yielded()`, `is_complete()`, `yielded()`, and `completed()`.

Generator integration is implemented. Language task, channel, and managed
transport integration is still in progress; those examples will be added with
their working language implementations. The
[implementation tracker](../../docs/concurrency-language-progress.md) records the
remaining work.
