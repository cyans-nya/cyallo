# Strings: library types over slices and vectors

Status: design decided in outline, nothing implemented. The runtime today represents a
string as `Value::String(Arc<str>)`, a literal as `Op::LiteralString(Reg, Arc<str>)`, and
the native backends reach strings only through bridge calls that resolve handles in a host
text heap (`BridgeCall::TextConcat`, `TextCompare`, `TextConstant`).

## Problem

- A string is reference counted without the program saying so. A clone is a refcount bump
  and the free happens at an unpredictable point, although the language treats `string` as an
  owned, move-only value.
- Native code never holds string bytes. Every string operation, even a comparison, leaves
  native code through a bridge call. Concatenation formats into a temporary and copies it
  into a new string, so each `+` costs two allocations and two copies.
- Strings are compiler intrinsics, unlike records and slices, so the optimizer cannot see
  their length or bytes.

## Decisions

1. `&str` and `string` are ordinary types, close to records, not compiler intrinsics.
2. `&str` is `&[u8]` with the guarantee that the bytes are valid UTF-8.
3. `string` is `Vec<u8>` with the same guarantee.
4. Slice and `Vec` are the native primitives that carry everything else:
   - a slice is `(ptr, len)`;
   - a `Vec` is `(ptr, cap, len)`;
   - both live in registers or on the stack, not in a heap object, unless the program asks
     for `gc` or `rc`.

The encoding is fixed to UTF-8 as an invariant of the type, not a parameter. A `u16` form
(Windows, JavaScript) is a separate type or a conversion, added later without changing
`string`.

| Type | Shape | Meaning |
|---|---|---|
| `&[u8]` | `(ptr, len)` | Raw borrowed bytes. |
| `Vec<u8>` | `(ptr, cap, len)` | Raw owned bytes, growable. |
| `&str` | wraps `&[u8]` | Borrowed, valid UTF-8. A literal is a static `&str`. |
| `string` | wraps `Vec<u8>` | Owned, valid UTF-8. Moves; `.clone()` is an explicit copy. |
| `rc string`, `gc string` | as for any `rc`/`gc` value | Cheap shared clone, chosen explicitly. |

## Consequences

### Native code sees the bytes

A string value is plain data in registers: length is a register read, slicing is pointer
arithmetic with a bounds check, equality compares lengths and then `memcmp` (inline for short
strings), and a concat is one allocation of `len(a) + len(b)` followed by two copies. The
host text heap, `intern`, and the `TextConcat`/`TextCompare`/`TextConstant` bridge calls go
away, and the optimizer sees the same slice and vector operations it sees everywhere else.
Operations that stay calls into the runtime are the ones that should: formatting, validation,
Unicode case mapping.

### The UTF-8 guarantee is an invariant of the wrapper

`&str` and `string` have no public way to hold invalid bytes: they are built only by
validation (`from_utf8`), by an explicit `unchecked` constructor that the caller vouches for,
or by operations that preserve validity (concatenation of two valid strings, slicing at a
character boundary). Everything else (find, split, chars, trim) is library code over the
bytes and can assume validity. Byte-level access is `.bytes()` and the underlying `[]u8`, so
ordinary code is never forced down to that level.

### Ownership is explicit

An owned `string` is a `Vec` and has the destructor a `Vec` has, so its free point is known
to the compiler. This replaces the hidden `Arc`. It also means `needs_drop(string)` becomes
true, and routines that hold strings (or records and enums that hold them) need the drop
scopes the HIR/MIR/AIR pipeline does not have yet.

### Concatenation and building

`+` on strings is defined in the library, not the compiler: owned plus borrowed produces an
owned string, appending to an owned `string` reuses its capacity, and a chain `a + b + c`
should become one allocation. A loop that builds a string uses `push_str` on a `string`
(amortized growth), not repeated `+`.

## Open questions

- **Where the buffer lives.** The header `(ptr, cap, len)` is in registers or on the stack,
  but the bytes of a `Vec` need storage. The working assumption is the allocator, owned by
  the `Vec` and freed by its destructor. This needs stating, along with what `gc` and `rc`
  wrap (the header, the buffer, or both) and how the GC finds a buffer that a header on the
  stack points to.
- **Literals.** A literal is a static `&str`, so `let s: string = "x"` needs either a
  conversion (`.to_string()`) or an allowed coercion. Which one?
- **Index unit.** Byte offsets on `str`, with characters iterated separately, is the
  suggestion. There is no `s[i]` by character because it is O(n).
- **Slicing validity.** Slicing a `&str` at a non-boundary offset either fails (a checked
  result or a panic-style error with a typed status) or is a distinct unchecked form. The
  error model must follow the compiler rules on typed errors and statuses.
- **Interning.** Whether static `&str` literals share the `NameId`-style interned text the
  compiler already keeps, or each literal is its own static buffer.
- **Prerequisites.** `Slice` types exist in the type system, but `Vec<u8>` with `(ptr, cap,
  len)` in registers and a destructor does not; strings cannot move until `Vec` and drop
  scopes do.

## Migration order

1. Slice `(ptr, len)` and `Vec` `(ptr, cap, len)` as register-resident values in the verified
   bytecode, with the interpreter and both native backends handling them.
2. Drop scopes in HIR/MIR/AIR, driven by the same may-moved dataflow (`check_moves`) that
   already tracks where a value is empty.
3. `&str` and `string` as library types over them, with literals as static `&str`.
4. Remove `Value::String(Arc<str>)`, `LiteralString`, the host text heap and the `Text*`
   bridge calls.

## Relation to the pipeline

The slices already implemented (strings, records, enums with non-`Copy` payloads) treat
`string` as an owned, move-only value with no destructor. They remain correct until step 3,
and only for payload types without a destructor. Once `string` is a `Vec`, a routine that
holds one must either stay on the existing route or wait for step 2.
