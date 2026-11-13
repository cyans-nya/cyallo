# Initial GC ownership, suspension, and budget boundaries

The first GC milestone must reclaim unreachable cells and arrays in the current runtime under repeated calls and long REPL sessions before escaping closures are added; each Session has one mutator and an isolated heap, and stop-the-world collection at safepoints is acceptable initially. The managed-memory limit includes collection reserve and collector-owned metadata, and host access initially uses scoped rooted handles and borrows whose addresses cannot survive allocation, collection, or reentrant execution. These boundaries address the existing retention problem while keeping synchronization and address stability manageable; moving versus non-moving collection, the specific algorithm, and handle representation remain open decisions.

The subsequently resolved movement and representation decisions are recorded in
[ADR 0003](0003-moving-compacting-collection.md) and
[ADR 0004](0004-arena-compaction-and-relocatable-references.md).
