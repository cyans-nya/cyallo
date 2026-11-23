# Cyallo concurrency examples

These are Cyallo `.ya` programs, executed by the Cyallo compiler and VM.

```sh
cargo run -- run examples/concurrency/generators/sequence.yas
cargo run -- run examples/concurrency/generators/resume-result.yas --gc-stress
cargo run -- run examples/concurrency/generators/delegation.yas --engine cranelift_speed
cargo test --test generators
cargo run -- run examples/concurrency/channels/mpmc.yas --gc-stress
cargo test --test concurrency
cargo run -- run examples/concurrency/ownership/local-managed.yas --gc-stress
```

| Example | Behavior | Expected output |
| --- | --- | --- |
| [Sequence](generators/sequence.ya) | A capturing factory, lazy construction, lexical yields, unit resume, completion, and idempotent close | `10`, `11`, `12`, `complete: 3` |
| [Result input](generators/resume-result.ya) | An ordinary Result is supplied to the suspended yield | `request`, `complete: 42`, `request`, `error: unavailable`, `complete: -1` |
| [Delegation](generators/delegation.ya) | A pure, capture-free child factory; owned `yield from`; forwarded input and completion | `yielded: 7`, `complete: 42` |
| [Buffered channel](channels/buffered.ya) | Bounded messages, typed full/closed errors, recovery of an unsent value | `true`, `42`, `true`, `99` |
| [Enum payloads](enums.ya) | A structural enum payload crosses a channel, and a task returns a unit variant | `42:worker`, `finished` |
| [Rendezvous](channels/rendezvous.ya) | A capturing procedure sends to its caller over a zero-capacity channel; joining returns its result | `42`, `7` |
| [MPMC](channels/mpmc.ya) | Two producers and two competing consumers use a bounded channel | `21` |
| [Clock](scheduling/clock.ya) | An ordinary procedure parks for a timer | `before`, `after` |
| [Cancellation](scheduling/cancellation.ya) | Cancelling a receiver releases its endpoint; a failed send retains its value | `true`, `true`, `99` |
| [Runtime imports](modules/main.ya) | Independent coroutines load new source files without holding a P | `42` |
| [Local managed values](ownership/local-managed.ya) | Independent `gc`/`rc` storage and explicit/default `@local`, aliases, strong and weak ownership | `42`, `2`, `7`, `true` |

Generator factories declare `Generator<Yield, Resume, Return>`. Calling a factory
provides construction parameters and creates a lazy generator. The first
`start()` starts it; subsequent `resume(value)` calls supply the yield expression's
result. A unit-input generator uses `resume()` after starting, or `next()` to
start or continue. Steps expose
`is_yielded()`, `is_complete()`, `yielded()`, and `completed()`.

`std::task::spawn` accepts a locally constructed or locally bound callable whose
captures and result can cross coroutines. A plain callable parameter carries no
transport proof and cannot be promoted by the accepting function. Managed local
objects and borrows cannot cross. Owned arrays/records are detached from the
sender's heap; endpoint `clone()` creates a separate logical owner.

`gc` and `rc` are local by default; `@local gc` and `@local rc` make that
explicit. `@shared` is parsed independently but currently rejected until shared
managed graphs have runtime support. See [managed memory](../../docs/managed-memory.md).

`Task.join()` consumes its handle and returns a typed outcome. Use
`is_completed()`, `is_failed()`, `is_cancelled()`, `is_stopped()`, and
`completed()`. A returned `Result::Err` remains an ordinary completed result.
Task drop detaches; `cancel()` requests cleanup, and `join()` waits for it.

Channels provide `send`, `try_send`, `recv`, `try_recv`, `close`, `is_closed`,
and explicit endpoint `clone`. `SendError` exposes `is_closed`, `is_full`,
`is_limit`, and `into_value`; `ReceiveError` exposes `is_closed` and `is_empty`.
Clock sleeps take a `u64` millisecond count. Concurrency operations use ordinary
procedures and need no `async`/`await` syntax.

Standard primitives use real [source declarations and scoped imports](../../docs/standard-declarations.md).
Groups, selection, shared managed graphs, and coroutine input capabilities remain
in progress. Child procedures
currently execute VM continuations in the interpreter; the caller can use any
native engine. The
[implementation tracker](../../docs/concurrency-language-progress.md) records the
remaining work.
