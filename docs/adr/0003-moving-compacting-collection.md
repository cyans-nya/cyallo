# Moving and compacting collection from the first release

The first collector must move live objects and compact managed storage, exercising root publication, relocation, and address invalidation from the beginning. This rules out shipping a solely non-moving collector first, but does not select semispace copying, arena/index compaction, or paged evacuation; all remain subject to the explicit-root, isolated-Session, and total-memory-limit contracts. Nova and V8 are design references, not dependencies or commitments to reproduce their complete collectors.

[ADR 0004](0004-arena-compaction-and-relocatable-references.md) subsequently
selects arena compaction for the first implementation.
