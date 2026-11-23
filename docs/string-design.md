# Strings: ownership, encoding, and slicing (proposal)

Status: proposal, undecided. Nothing here is implemented. The runtime today represents a
string as `Value::String(Arc<str>)` and a literal as `Op::LiteralString(Reg, Arc<str>)`.

## Problem

A string value is currently reference counted without the program saying so. A clone is a
refcount bump and the memory is freed at an unpredictable point. That conflicts with the rest
of the language, which has move semantics, borrowed references, slices, and explicit `rc` and
`gc` types. The type system already treats `string` as an owned, move-only value (it is not
`Copy`), so the `Arc` is an implementation detail that has leaked into behavior. It also
means `needs_drop(string)` is false only because of that hidden count, which will not hold
once a string should be freed at a defined point.

## Options considered

1. **Keep `Arc<str>`.** Cheap, and the language-level semantics are already move-only.
   Document the `Arc` as an implementation detail and forbid observable sharing. Rejected as
   the long-term answer: the lifetime of a string stays implicit.
2. **Encode the encoding in the type** (`&[]u8` or `&[]u16` depending on encoding). Rejected:
   every string function becomes generic over the encoding, and string manipulation, the
   common case, becomes the hard case.
3. **Make strings garbage collected.** Rejected: it would make a string a shared reference,
   which contradicts the move semantics the pipeline checks (E118).
4. **Split by ownership, fix the encoding** (below). Proposed.

## Proposal

Strings are valid UTF-8. The encoding is an invariant of the type, not a parameter.

| Type | Meaning |
|---|---|
| `&str` | Borrowed, validated UTF-8 view. Slicing costs nothing. A string literal is a static `&str`. |
| `string` | Owned, uniquely held buffer with a destructor. Moves like a record. `.clone()` is an explicit copy. |
| `[]u8` | Raw bytes. Conversion to `str` is validated (`from_utf8`) or explicitly unchecked. |
| `rc string`, `gc string` | Cheap shared clone, chosen explicitly. |

### Consequences

- **Cheap slice and clone.** A slice is a borrowed `&str`. Cheap sharing is opt-in through
  `rc` or `gc`, so no refcount is hidden.
- **Manipulation stays easy.** Methods (find, split, chars, trim, concatenation) work on
  validated `str`, so none of them re-checks encoding. Byte-level work uses `.bytes()` and
  `[]u8`; nothing forces ordinary code down to that level.
- **Deterministic drops.** An owned `string` has a destructor, so `needs_drop(string)` becomes
  true. Strings, and records or enums that hold them, then need the drop scopes that the
  HIR/MIR/AIR pipeline does not have yet.

### Costs

- A literal is `&str`, so `let s: string = "x"` needs a conversion (`.to_string()`) or an
  allowed coercion. This is the main ergonomic cost.
- The runtime value representation changes, and so does the existing route. The pipeline
  changes too: drop scopes, and a `LiteralString` that borrows instead of sharing an `Arc`.

## Open questions

- **Literal coercion.** Should a literal coerce to `string` where one is expected, or is the
  conversion always explicit?
- **Index unit.** Byte offsets on `str`, with characters iterated separately, is the
  suggestion. Character indexing is O(n) and is not offered as `s[i]`.
- **Wide strings.** `u16` interop (Windows, JavaScript) is left out. A distinct `wstr`, or
  conversion functions, can be added later without changing `string`.
- **Interning.** Whether `NameId`-style interning in the compiler should also back static
  `&str` literals at runtime.
- **Migration.** Whether the old route and the pipeline switch together, or the pipeline
  keeps `Arc<str>` behind the same type until drop scopes exist.

## Relation to the pipeline

The slices already implemented (strings, records, enums with non-`Copy` payloads) treat
`string` as an owned, move-only value with no destructor. They remain correct under this
proposal as long as the payload types have no destructor. Once `string` gains one, routines
that hold strings must either leave the pipeline or wait for drop scopes, driven by the same
may-moved dataflow (`check_moves`) that already tracks where a value is empty.

## Decision needed

Before any runtime change: choose between option 1 (document the `Arc`) and option 4
(this proposal), and settle literal coercion. Everything else follows from those two.
