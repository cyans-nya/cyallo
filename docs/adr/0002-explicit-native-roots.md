# Explicit native root handles

Native execution exposes every managed reference needed across a collecting safepoint through explicitly registered native root handles; the collector does not discover roots by scanning the machine stack or registers. Canonical VM slots can back registered roots, but storing reference bits in a slot alone does not establish rooting, and native code reloads addresses after a collecting operation before resuming in its current tier. This chooses an explicit compiler/runtime rooting protocol over platform stack-scanning machinery; the representation of managed references and the ownership/lifetime of native root scopes remain separate decisions.

Those separate decisions are now recorded in
[ADR 0004](0004-arena-compaction-and-relocatable-references.md) and
[ADR 0005](0005-resumable-root-scopes-and-gc-capabilities.md).
