# 02 — Removing `StorageKind` and the old encodings

**Decided.** Standard types are described by declarations only; no `StorageKind` in the AST,
no parallel compiler-owned type system.

**Done.** Clock, Task, Cell, Location, Path, Allocation, Layout, Vec, VecDeque, Weak and the
channel endpoints are opaque types bound through `crates/cyallo-ir/bindings/std.bindings`.
Option, Result, GeneratorStep, SendError and ReceiveError are real enums at runtime and in
annotations/signatures.

**Still present (and why).**
- `Type::Storage(Option|Result|GeneratorStep|SendError, T)`: built by `lookup::constructor`
  / `lookup::call`, `memory::error`, `intrinsic.rs` result types, `generator::step` and the
  members for upgrade/pop/send, only to be compared with the declared type
  (`memory::same_regions` and `generics` unify bridge the two forms via `enums::core_shape`).
  The sites need an environment to build a real enum, which they do not have.
- `StorageKind::Range`, `ParseError`, `AllocError`, `LoadError`, `ViewError`,
  `TypeDescriptor`: host payloads with no variants; they belong with the opaque-type migration.
- `EnumEncoding` role tags (also used for reserved ids and constant freezing),
  `Value::NativeEnum` / `Value::NativeEnumStorage`, `NativeEnumDeclaration`.
- Intrinsics `OptionSome`, `OptionNone`, `ResultOk`, `ResultErr` and operations
  `Operation::Some/None/ResultNew`: unreachable from source but still used by allocator and
  lookup producers (`ResultNew`) and by the contract tables.

**To do.**
1. For each marker producer, take the type from the declared result and validate its shape
   with `lookup::option_payload` / `result_payload` / `step_payload` / `send_payload`
   (the pattern used for tasks and receivers); then delete the producer.
2. Delete the four `StorageKind` variants, the bridges (`core_shape` uses in `same_regions`
   and `generics`), the role tags that nothing else needs, and the unused intrinsics.
3. Migrate `Range`, `ParseError`, `AllocError`, `LoadError`, `ViewError`, `TypeDescriptor` to
   opaque types with traits (or declared structs where they have fields).

**Decision needed.** Whether `Range` is a data struct (`start`, `end`, `inclusive`,
`reversed`) in `core` or an opaque host type.
