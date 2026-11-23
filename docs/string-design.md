# Strings: `string` for most code, `Utf8` for byte-level work

Status: design. Nothing here is implemented. Today a string is `Value::String(Arc<str>)`, a
literal is `Op::LiteralString(Reg, Arc<str>)`, and the native backends reach strings only
through bridge calls that resolve handles in a host text heap (`BridgeCall::TextConcat`,
`TextCompare`, `TextConstant`).

## Why

- The `Arc` is a hidden reference count, although the language treats `string` as owned and
  move-only.
- Native code never holds string bytes: every operation, even a comparison, is a bridge call,
  and each `+` formats into a temporary and copies it into a new string.
- Strings are compiler intrinsics, so the optimizer cannot see their length or bytes.
- Most string use is "build some text, pass it around, print it", for which an immutable
  shared value with cheap concatenation is the right default. Byte-level work (parsing,
  buffers, I/O) wants an owned, growable, validated buffer. One type cannot be both.

## Overview

Two tiers, both UTF-8 only.

| Type | Role | Shape | Memory |
|---|---|---|---|
| `string` | The default. Immutable. Concatenation makes a new value. | A handle to a heap string object (flat, cons, or sliced) | GC-managed, `Copy` |
| `Utf8` | Owned, growable, validated UTF-8 buffer (Rust's `String`). | `Vec<u8>`: `(ptr, cap, len)` | Owned and moved; destructor frees the buffer |
| `utf8` | Unsized validated UTF-8 text (Rust's `str`). | `[]u8` that is valid UTF-8, unsized | Never held by value |
| `&utf8` | Borrowed flat view. | `&[]u8`: `(ptr, len)` | In registers or on the stack |

`Utf8` and `utf8` are ordinary library types over `Vec<u8>` and `[]u8`, not compiler
intrinsics. `string` is the one built-in managed type, because its representation
(cons and sliced nodes, lazy flattening) has to be known to the compiler, the GC, and the
backends.

The encoding is UTF-8 as an invariant of the types. V8's one-byte/two-byte split is not
adopted: it would double every layout and every operation. A `u16` form for Windows or
JavaScript interop is a conversion or a separate type added later.

## `Utf8` and `utf8` (the Rust-style tier)

- `utf8` is `[]u8` with the guarantee that its bytes are valid UTF-8. It is unsized, so it
  appears only behind a reference: `&utf8` is `&[]u8`, a `(ptr, len)` pair in registers, and
  `&mut utf8` is its mutable form.
- `Utf8` is `Vec<u8>` with the same guarantee, `(ptr, cap, len)` in registers or on the
  stack. It moves, `.clone()` is an explicit copy, and its destructor frees the buffer.
  `gc Utf8` and `rc Utf8` are available through the existing managed kinds.
- The guarantee is held by the wrapper: the only ways to make a `Utf8` or `&utf8` are
  validation (`from_utf8`), an `unchecked` constructor whose caller vouches for the bytes, or
  operations that preserve validity (push a valid `&utf8`, slice at a character boundary).
  `find`, `split`, `chars`, `trim` and the rest are library code over the bytes and may
  assume validity. `.bytes()` returns the raw `&[]u8` when raw access is wanted.
- `push_str` appends in place with amortized growth. This is the tool for loops that build
  text.

## `string` (the default tier)

`string` is immutable. `a + b` produces a new value and leaves both operands unchanged, so a
`string` is shared freely and copying one is copying a handle. It follows V8's cons-string
model, with one encoding.

### Object layout

Every `string` is a handle to a heap object whose first word is a header. The layout is
declared here so the interpreter, the verifier, the GC and both native backends agree on it.

| Kind | Fields | Meaning |
|---|---|---|
| Flat | `header, bytes…` | Valid UTF-8 stored inline. |
| Cons | `header, left, right` | The concatenation of two strings, not yet copied. |
| Sliced | `header, parent, offset` | A view into a flat parent. |

The header holds the kind tag, the byte length, and for a cons the depth. A hash cache may be
added without changing the layout of the kinds above. An external kind (bytes owned by the
host) is deliberately left out until something needs it.

### Rules

- **Short concatenation copies.** If the combined length is below a threshold (V8 uses 13
  bytes; the value here is to be measured), the result is a flat string. Cons nodes are only
  worth their header for longer text.
- **Longer concatenation allocates a cons node** in O(1) and copies nothing.
- **Flattening.** Any operation that needs the bytes (view, compare, hash, character
  iteration, passing to a `&utf8` parameter) first flattens the string. Flattening a cons
  allocates one flat string of the full length, copies the leaves into it, and rewrites the
  node into a one-word forwarding indirection to the flat result, so later accesses on any
  handle to it are fast. The string's contents never change, so the rewrite is invisible to
  the program.
- **Depth limit.** A cons whose depth passes a limit is flattened when it is created, so a
  loop of `s = s + x` cannot build an unboundedly deep rope or overflow the stack of the
  flattening routine (which is iterative regardless).
- **Slicing.** `s.slice(a, b)` on a flat string above a size threshold is a `Sliced` node
  sharing its parent. Below the threshold it copies, so a small slice does not keep a large
  parent alive. A character boundary is required (see the error model below).
- **Equality.** Compare lengths first; if equal, flatten both and compare the bytes. The
  compiler's interned names (`NameId`) are a separate mechanism and are not affected.

### Memory

A `string` is GC-managed and `Copy`: a plain `string` value is a handle to a collected object,
and copying it copies the handle. It is not an implicit `rc`. The language already has a
collected kind (`gc`), and `string` is the one type that uses it implicitly because
immutability and sharing make an owner meaningless. A `string` therefore has no destructor
and never needs a drop scope, and it is not moved, so holding one never produces the "use of a
moved value" error (E118). The collector traces `Cons` (both children), `Sliced` (the parent),
and forwarding nodes, and treats `Flat` as a leaf.

Literals are static `Flat` objects placed in a read-only region, not allocated on the
collected heap, and are not traced.

## Conversions

| From | To | Cost |
|---|---|---|
| `string` | `&utf8` | `view()`: flattens if needed, then `(ptr, len)`. The view borrows the string, which keeps the object alive for the borrow. |
| `&utf8` | `string` | `to_string()`: copies into a flat string. If the view came from `string.slice`, the slice node is returned directly. |
| `Utf8` | `string` | `into_string()`: copies into a flat string. Adopting the buffer without a copy is a later optimization (needs the buffer on the collected heap). |
| `string` | `Utf8` | `to_utf8()`: copies into a new buffer. |
| `&utf8` | `Utf8` | `to_owned()`: copies. |

A string literal has type `string` and coerces to `&utf8` where one is expected, since a
literal is always flat and the view needs no flattening.

## Operations

- `+` on `string` creates a cons (or a flat string below the threshold). `+` on `&utf8` is
  not defined: use `Utf8` and `push_str`, or convert.
- Length, `==`, `<`, hashing and iteration are defined once, on `&utf8`, and `string` gets them by
  taking its view. Length alone is read from the header without flattening.
- Formatting, validation, and Unicode case mapping are runtime calls. Everything else
  compiles to the inline sequences below.
- Byte offsets index `utf8`. Character iteration is separate (`.chars()`); there is no `s[i]`
  by character because it is O(n).

## Native code

The layout is declared, so native code reads it. The common case is inline and the rare case
is a call:

- **Length** of a `string`: load the header and mask. No branch on kind.
- **View** of a `string` as `&utf8`: test the tag; Flat yields `(handle + header, len)`,
  Sliced yields the parent's bytes plus the offset, and a Cons or forwarding node calls the
  runtime `flatten`. Flat strings are the overwhelmingly common case.
- **Equality**: compare header lengths, then `memcmp` (inline for short strings).
- **Concat**: short result: allocate flat and copy; otherwise allocate a three-word cons.
- A `Utf8` or `&utf8` is plain `Vec`/slice code: length is a register, slicing is pointer
  arithmetic with a bounds check, and a copy is `memcpy`.

The host text heap, `intern`, and the `Text*` bridge calls are removed.

## Errors

Operations that can fail on input (`from_utf8`, slicing at a non-boundary offset, an
out-of-range index) return a typed status or result, following the compiler's rules on typed
errors and statuses, not a panic. Whether a checked and an unchecked form of slicing are both
offered is left to the library.

## Effect on the compiler

- **Verified bytecode.** New value kinds for slice `(ptr, len)`, `Vec` `(ptr, cap, len)`,
  and `string` handles, replacing `FrameValue::Text`. New instructions or intrinsics for
  string length, view, concat, and equality. The verifier checks register kinds as it does
  for text today.
- **Pipeline.** A `string` is `Copy`, so the strings slice's move tracking applies only to
  `Utf8` and `Vec`. Reading a `string` after passing it on becomes legal, and
  `tests/strings.rs`, which states the current move-only rule, must change with it. Enums and
  records that hold only scalars and `string` have no destructor, so the non-`Copy` payload
  slice is unaffected for `string`, and a `Utf8` payload needs drop scopes.
- **Drop scopes.** `Utf8` and `Vec` have destructors, so routines holding them need drop
  scopes driven by the may-moved dataflow (`check_moves`). Routines that hold only `string`
  do not.
- **Old route.** It must either change with the runtime or keep an `Arc<str>` adapter during
  migration; see the order below.

## Migration order

1. Slice `(ptr, len)` and `Vec` `(ptr, cap, len)` as register-resident values in the verified
   bytecode, handled by the interpreter and both native backends.
2. Drop scopes in HIR/MIR/AIR.
3. `utf8`, `&utf8` and `Utf8` as library types over them, with `from_utf8` and `push_str`.
4. The `string` heap object (Flat, Cons, Sliced, forwarding), GC tracing, flatten, and the
   inline native sequences. Literals become static Flat objects.
5. Switch `string` operations and literals to the new representation. Remove
   `Value::String(Arc<str>)`, `LiteralString`, the host text heap, and the `Text*` bridge
   calls. Update `tests/strings.rs` for `Copy` semantics.

## Open questions

- **Buffer storage.** The `Vec` header is in registers or on the stack, but its bytes need
  storage. The working assumption is the allocator, freed by the `Vec` destructor. What `gc`
  and `rc` wrap (header, buffer, or both) and how the collector finds a buffer pointed to from
  the stack still need to be stated.
- **Thresholds.** The short-concat copy limit, the slice-sharing limit, and the maximum cons
  depth are to be measured, not copied from V8.
- **Flatten and threads.** Rewriting a cons into a forwarding node is a write to shared
  data. It must be idempotent and published safely if strings cross threads (see
  `docs/concurrency.md`), or strings must stay thread-local.
- **Adopting a buffer.** Moving a `Utf8` into a `string` without a copy needs the buffer on
  the collected heap; whether that is worth a separate allocation path is open.
- **Literal coercion.** A literal coerces from `string` to `&utf8`; whether it also coerces to
  `Utf8` (a copy) is not decided and is currently an explicit `to_owned()`.
- **Hash cache and interning.** Whether `string` caches its hash in the header, and whether
  equal strings are deduplicated, are optimizations to decide after measurement.
