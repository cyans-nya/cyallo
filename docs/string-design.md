# Strings: `gc string` for most code, `Utf8` for byte-level work

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
| `gc string` | The default. Immutable. Concatenation makes a new value. | A handle to a heap string object (flat, cons, or sliced) | GC-managed, `Copy`, neither `Send` nor `Sync` |
| `Utf8` | Owned, growable, validated UTF-8 buffer (Rust's `String`). | `Vec<u8>`: `(ptr, cap, len)` | Owned and moved; destructor frees the buffer |
| `utf8` | Unsized validated UTF-8 text (Rust's `str`). | `[]u8` that is valid UTF-8, unsized | Never held by value |
| `&utf8` | Borrowed flat view. | `&[]u8`: `(ptr, len)` | In registers or on the stack |

`Utf8` and `utf8` are ordinary library types over `Vec<u8>` and `[]u8`, not compiler
intrinsics. `gc string` is a managed type whose representation (cons and sliced nodes, lazy
flattening) is known to the compiler, the GC, and the backends.

**The spelling is `gc string`.** A bare `string` is rejected, with a diagnostic that names
`gc string` (the shared, immutable default) and `Utf8` (the owned buffer). Writing the kind
keeps the collector visible in the type, as it is for every other collected value, and no
type is implicitly garbage collected. `gc string` is `Copy` through the existing rule that
makes `gc` references `Copy` (`ownership::copy`), so it needs no new ownership machinery.

The encoding is UTF-8 as an invariant of the types. V8's one-byte/two-byte split is not
adopted: it would double every layout and every operation. A `u16` form for Windows or
JavaScript interop is a conversion or a separate type added later.

## `Utf8` and `utf8` (the Rust-style tier)

- `utf8` is `[]u8` with the guarantee that its bytes are valid UTF-8. It is unsized, so it
  appears only behind a reference: `&utf8` is `&[]u8`, a `(ptr, len)` pair in registers, and
  `&mut utf8` is its mutable form.
- `Utf8` is `Vec<u8>` with the same guarantee, `(ptr, cap, len)` in registers or on the
  stack. `Vec<T>` is itself `RawVec<T>` plus a `len` (see below). It moves, `.clone()` is
  an explicit copy, and its destructor frees the buffer.
  `gc Utf8` and `rc Utf8` are available through the existing managed kinds.
- The guarantee is held by the wrapper: the only ways to make a `Utf8` or `&utf8` are
  validation (`from_utf8`), an `unchecked` constructor whose caller vouches for the bytes, or
  operations that preserve validity (push a valid `&utf8`, slice at a character boundary).
  `find`, `split`, `chars`, `trim` and the rest are library code over the bytes and may
  assume validity. `.bytes()` returns the raw `&[]u8` when raw access is wanted.
- `push_str` appends in place with amortized growth. This is the tool for loops that build
  text.

### `RawVec`

`RawVec<T>` is a unique pointer plus a capacity: it owns an allocation and frees it in its
destructor, and it does not know how many elements are initialized. `Vec<T>` is `RawVec<T>`
plus `len`, and `Vec` supplies the safe operations (push, slice, truncate) that keep `len` and
the initialized prefix consistent. `RawVec` is a runtime-internal primitive in
`std::unsafe`. **The host must not provide `std::unsafe` to untrusted guest components:** it
exposes raw allocation and unchecked memory access, so only trusted code may name it, and
untrusted code reaches memory only through the safe `Vec`, slice, `Utf8` and `gc string` APIs.
The gate follows the module graph's existing source-trust levels (a module's trust is already
recorded when it is added); binding `std::unsafe` to the trusted level is part of the work.

This also answers where a `Vec`'s bytes live: in the allocation its `RawVec` owns, which the
destructor frees. `rc Utf8` and `gc Utf8` wrap the whole `Vec` (header and buffer) as for any
managed value; the collector never needs to find a bare buffer from a header on the stack,
because an unmanaged `Vec` is freed by its owner and not by the collector.

## `gc string` (the default tier)

`gc string` is immutable. `a + b` produces a new value and leaves both operands unchanged, so
a `gc string` is shared freely and copying one is copying a handle. It follows V8's cons-string
model, with one encoding.

### Object layout

Every `gc string` is a handle to a heap object whose first word is a header. The layout is
declared here so the interpreter, the verifier, the GC and both native backends agree on it.

| Kind | Fields | Meaning |
|---|---|---|
| Flat | `header, bytes…` | Valid UTF-8 stored inline. |
| Cons | `header, left, right` | The concatenation of two strings, not yet copied. |
| Sliced | `header, parent, offset` | A view into a flat parent. |

The header holds the kind tag, the byte length, for a cons the depth, and the `u32` hash slot
shared by every `gc` object (see "Hashing"), computed on first use. An external kind (bytes
owned by the host) is deliberately left out until something needs it.

### Rules

- **Short concatenation copies.** If the combined length is at most 13 bytes, the result is a
  flat string. Cons nodes are only worth their header for longer text.
- **Longer concatenation allocates a cons node** in O(1) and copies nothing.
- **Flattening.** A linear access to the characters (view, compare, hash, character iteration,
  passing to a `&utf8` parameter) first flattens the string; length alone does not.
  Flattening a cons
  allocates one flat string of the full length, copies the leaves into it, and rewrites the
  node into a one-word forwarding indirection to the flat result, so later accesses on any
  handle to it are fast. The string's contents never change, so the rewrite is invisible to
  the program.
- **Depth limit.** A cons whose depth passes a limit is flattened when it is created, so a
  loop of `s = s + x` cannot build an unboundedly deep rope or overflow the stack of the
  flattening routine (which is iterative regardless).
- **Slicing.** `s.slice(a, b)` on a flat string of at least 13 bytes is a `Sliced` node
  sharing its parent. A shorter result copies, so a small slice does not keep a large parent
  alive. (The limit reuses the concat threshold until measurement says otherwise.) A
  character boundary is required (see "Errors").
- **Equality.** Compare lengths first; if equal, flatten both and compare the bytes. The
  compiler's interned names (`NameId`) are a separate mechanism and are not affected.

### Memory and threads

A `gc string` is a handle to a collected object, and copying it copies the handle. It is not
an implicit `rc`. It has no destructor and never needs a drop scope, and it is not moved, so
holding one never produces the "use of a moved value" error (E118).

A `gc string` is neither `Send` nor `Sync`. It lives on one thread's collected heap and
cannot be shared across threads, so flattening (a write that rewrites a cons into a
forwarding node) is never concurrent and needs no atomics or publication. Passing text
between threads copies it into a `Utf8` or another owned form. The collector traces `Cons`
(both children), `Sliced` (the parent), and forwarding nodes, and treats `Flat` as a leaf.

Literals are static `Flat` objects placed in a read-only region, not allocated on the
collected heap, and are not traced.

## Conversions

| From | To | Cost |
|---|---|---|
| `gc string` | `&utf8` | `view()`: flattens if needed, then `(ptr, len)`. The view borrows the string, which keeps the object alive for the borrow. |
| `&utf8` | `gc string` | `to_string()`: copies into a flat string. If the view came from `string.slice`, the slice node is returned directly. |
| `Utf8` | `gc string` | `into_string()`: copies into a flat string. Adopting the buffer without a copy is a later optimization (needs the buffer on the collected heap). |
| `gc string` | `Utf8` | `to_utf8()`: copies into a new buffer. |
| `&utf8` | `Utf8` | `to_owned()`: copies. |

A string literal has type `gc string` and coerces to `&utf8` where one is expected, since a
literal is always flat and the view needs no flattening. It does not coerce to `Utf8`; that
is an explicit `to_owned()`.

## Operations

- `+` on `gc string` creates a cons (or a flat string of at most 13 bytes). `+` on `&utf8` is
  not defined: use `Utf8` and `push_str`, or convert.
- Length, `==`, `<`, `Hash` and iteration are defined once, on `&utf8`, and `gc string` gets
  them by taking its view. Length alone is read from the header without flattening. Its
  `HashLite` is the value cached in the header (see "Hashing").
- Formatting, validation, and Unicode case mapping are runtime calls. Everything else
  compiles to the inline sequences below.
- Byte offsets index `utf8`. Character iteration is separate (`.chars()`); there is no `s[i]`
  by character because it is O(n).

## Native code

The layout is declared, so native code reads it. The common case is inline and the rare case
is a call:

- **Length** of a `gc string`: load the header and mask. No branch on kind.
- **View** of a `gc string` as `&utf8`: test the tag; Flat yields `(handle + header, len)`,
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

## Hashing

Two library traits, a heap-wide seed, and a header slot that caches the lightweight hash of
every `gc` object.

### `Hash`

`Hash` is the standard trait, as in Rust. A type feeds its value into a `Hasher`
(`fn hash<H: Hasher>(&self, state: &mut H)`), and values that are equal (`Eq`) must hash
equal. `Hasher` is the algorithm: `write`, `write_u8`, ..., and `finish`, which returns a
`u128`. Rust's `finish` returns a `u64`, but fast hashers keep 128 bits of state (for
example the two halves of a 64x64 multiply), so the wider result avoids throwing bits away
before the narrowing below. The default hasher is the seeded function described under
"Seed"; a collection may use another.

- **Writes are typed.** The hash depends on the sequence of writes with their widths, not on
  the concatenated bytes: `write_u8(255); write_u8(255)` differs from `write_u16(65535)`,
  and a `write` of a byte slice includes its length. Every `Hasher` must satisfy this; the
  default hasher mixes the width (and a slice's length) into its state on each write.
- **Strings feed their contents.** `gc string`, `Utf8` and `&utf8` all implement `Hash` by
  one `write` of their bytes. The write carries the length, so a sequence of strings is
  prefix-free with no terminator. Equal text hashes equal across the three, so a collection
  keyed by `gc string` can be looked up with a `&utf8`.
- A user type implements `Hash` by hashing its fields in order, normally derived.

### `HashLite`

`HashLite` is a lightweight hash for runtime tables that do not need a generic hasher.

- `fn hash_lite(&self) -> u32`, with no `Hasher` involved. `u32` is enough to index any
  table under 2^32 slots and fits beside a `u32` length in a one-word header. A table still
  compares keys for equality, so a collision costs a probe, not correctness. A type that
  wants more bits uses `Hash`.
- **The guarantee is per class.** Equal values of the same type hash equal, under the same
  heap seed. Nothing is promised across types: a `gc string` and a `Utf8` with the same text
  may return different values. A value is not stable across heaps, runs or threads, so it
  must not be stored, sent, or used to order anything.
- `HashLite` and `Hash` are independent. A type that has both must keep `Eq` consistent with
  each within its class; the two values need not be related.

Which to use: a generic collection that looks keys up across types (a `gc string` key found
with a `&utf8`) uses `Hash`. A table internal to the runtime or compiler, keyed by one type
(symbol tables, inline caches, dedup tables of `gc string`), uses `HashLite` and gets the
cached value.

### `HashLite` for every `gc` object

Every `gc` object type has `HashLite` without writing it, and the result is cached in a
`u32` slot in the object header. The slot has three states: unset, cached hash, and identity
id. (A separate state is needed because zero is a legitimate hash; the JDK hit the same
problem.) The implementation is chosen by what the type has, in this order:

1. **The type implements `HashLite`:** use it and cache the result.
2. **The type implements `Hash`:** run `Hash` through the heap's seeded hasher, narrow the
   `u128` to `u32` (below), and cache that. Only the heap hasher's value is cached: `Hash`
   called with any other `Hasher` does the full walk, because a value computed under one
   hasher is wrong under another. `gc string` is this case: its `Hash` feeds the contents,
   and its cached `HashLite` is that value under the heap hasher.
3. **The type implements neither:** use an identity hash, a number assigned on first use and
   stored in the same slot. A moving collector cannot use the address, so the id stays with
   the object when it moves.

Conditions that keep this correct:

- **Cache only what cannot change.** The cached value is valid for an immutable object, or
  for a mutable one whose hashed fields are never mutated. A type with mutable hashed fields
  must clear the cache on write or not implement `Hash`. The compiler rejects a derived
  `Hash` over a field that is mutable through a `gc` reference.
- **Identity only if `Eq` is identity.** Rule 3 is sound only when equality is pointer
  equality. A type with structural `Eq` and no `Hash` is a compile error as a hash key, not
  a silent identity hash, because that would break "equal means equal hash".
- **Header cost.** The slot is one `u32`, beside the existing length or tag word, so it
  normally adds no space. Only the owning thread writes it, since `gc` objects are not
  `Send`.
- **Literals** are static objects in a read-only region, so their slot cannot be written
  and a seeded hash cannot be stored ahead of time. A literal's `HashLite` is computed on
  use or kept in a per-heap side cache (open question).

### Narrowing `u128` to `u32`

Truncating drops bits that a weak hasher may have put its entropy in, so the narrowing
folds all of them:

1. Fold the four 32-bit lanes with distinct rotations, so no lane is dropped and equal lanes
   do not cancel (a plain xor of two equal halves is zero).
2. Finish with the Murmur3 32-bit finalizer:
   `x ^= x >> 16; x *= 0x85ebca6b; x ^= x >> 13; x *= 0xc2b2ae35; x ^= x >> 16`.

The finalizer is a bijection, so it adds no entropy; it makes every output bit depend on
every folded bit, so a table can index by mask, low bits or high bits. A hasher with only 64
bits of real state leaves the upper lanes constant, and the result is still valid. Exact
rotation amounts are fixed by measurement.

### Seed

The hash functions are keyed with a random seed chosen when a heap is created, which resists
collision flooding (an attacker who knows the function picks keys that share a bucket).

- **Only `gc` objects cache.** `Utf8` and `&utf8` have no header, so they hash their bytes
  on every use.
- **Order is not reproducible.** Iterating a hash-ordered collection can differ between
  runs. A fixed seed must be selectable (a runtime option) so tests and reproducible builds
  can pin it.
- **A hash cached under one seed is not valid under another.** A `gc` object is copied,
  never shared, between heaps (it is not `Send`), and the copy computes its own.
- **`HashLite` is only as strong as its seed.** A `u32` an attacker can predict can be
  flooded, so it is always computed with the heap seed and never a fixed function of the
  contents.

## Effect on the compiler

- **Verified bytecode.** New value kinds for slice `(ptr, len)`, `Vec` `(ptr, cap, len)`,
  and `gc string` handles, replacing `FrameValue::Text`. New instructions or intrinsics for
  string length, view, concat, and equality. The verifier checks register kinds as it does
  for text today.
- **Pipeline.** A `gc string` is `Copy`, so the strings slice's move tracking applies only to
  `Utf8` and `Vec`. Reading a `gc string` after passing it on becomes legal, and
  `tests/strings.rs`, which states the current move-only rule, must change with it. Enums and
  records that hold only scalars and `gc string` have no destructor, so the non-`Copy` payload
  slice is unaffected for `gc string`, and a `Utf8` payload needs drop scopes.
- **Drop scopes.** `Utf8` and `Vec` have destructors, so routines holding them need drop
  scopes driven by the may-moved dataflow (`check_moves`). Routines that hold only `gc string`
  do not.
- **Old route.** It must either change with the runtime or keep an `Arc<str>` adapter during
  migration; see the order below.

## Migration order

1. Slice `(ptr, len)` and `Vec` `(ptr, cap, len)` as register-resident values in the verified
   bytecode, handled by the interpreter and both native backends.
2. Drop scopes in HIR/MIR/AIR.
3. `utf8`, `&utf8` and `Utf8` as library types over them, with `from_utf8` and `push_str`.
4. The `gc string` heap object (Flat, Cons, Sliced, forwarding), GC tracing, flatten, and the
   inline native sequences. Literals become static Flat objects.
5. Switch `gc string` operations and literals to the new representation. Remove
   `Value::String(Arc<str>)`, `LiteralString`, the host text heap, and the `Text*` bridge
   calls. Update `tests/strings.rs` for `Copy` semantics.

## Decided

- **Spelling.** `gc string`; a bare `string` is rejected.
- **`Vec`.** A wrapper of `RawVec` (unique pointer and capacity, in `std::unsafe`, never given
  to untrusted guests) plus `len`. A guest can therefore be given `Vec` without `RawVec`.
- **Short concat.** Copy when the combined length is at most 13 bytes; otherwise a cons.
- **Flatten.** On linear character access.
- **Threads.** `gc string` is neither `Send` nor `Sync`.
- **Hash.** `Hash` feeds contents and is consistent across `gc string`, `Utf8` and `&utf8`.
  `HashLite` returns a `u32`, is guaranteed only within a class, and every `gc` object
  caches it in its header (a `Hash` result narrowed from `u128`, else an identity id). Both
  are seeded per heap. See "Hashing".
- **Slice and depth limits.** The slice limit reuses 13 bytes and the cons depth limit is set
  when measured; the values are not revisited until then.
- **Adopting a buffer.** Allowed: `Utf8::into_string` may take over the buffer without a
  copy once the buffer can live on the collected heap. Until then it copies.
- **Gating `std::unsafe`.** There is no third trust level. An untrusted guest component is
  `User` code with a narrower allow-list of `Core` modules: `std::unsafe` is a `Core` module
  that the host leaves off that list (and that ordinary `User` code is not given either), so
  untrusted code cannot name `RawVec` or any raw allocation, while it can still use `Vec`,
  slices, `Utf8` and `gc string`. This reuses the existing `User`/`Core` split and the
  per-module allow-list in `import_policy.rs` and the dynamic-module access check.
- **Interning.** Deferred. Runtime deduplication of equal strings is not part of this design;
  the compiler's `NameId` interning is unchanged.

## Open questions

- **Hash function.** The candidate is a rapidhash-class keyed function, the one V8 now uses,
  which is fast on short strings. The decision waits on a measurement here. Whatever is
  chosen, the seed must really be applied: V8's switch to rapidhash shipped constants that
  were not seeded, which was a hash-flooding vulnerability (Node.js advisory for
  CVE-2025-27209).
- **Literal hash cache.** Computing a literal's `HashLite` on every use versus a side cache;
  decide after measurement.
- **Which collection uses which trait.** Whether the standard map picks `HashLite`
  automatically for a key type that has it (so a `gc string`-keyed map gets the cache) or
  there are two map types. The first needs specialization.
