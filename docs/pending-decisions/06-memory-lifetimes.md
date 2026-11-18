# 06 — Scope-based cell reuse without GC

**Decided in principle (user).** Plain values (not `gc`) are not garbage collected; their
storage should be reused like a stack and must not leak.

**Current state.** Without GC, cells are owned by the allocating frame and released only when
the frame exits. Mitigations done: patterns read copied payloads by value (no payload cell
per match); overwriting an enum reuses its previous payload cell (`parked_children`). A loop
that matches or overwrites enum values no longer exhausts `max_cells` (1,000,000).

**Still wrong.** Cells created inside a loop body for non-copy payloads, and any other
frame-owned cell created per iteration, still accumulate until the frame exits. The
parked-cell reuse is per parent cell and assumes no live borrow of the old payload.

**To do.** Release cells when their scope ends (block / loop iteration), using the drop
scopes the lowering already records, instead of per frame; remove `parked_children` once
that holds.

**Decision needed.** Whether scope release is done in lowering (explicit release ops) or in
the runtime (scope markers), and how borrows of enum payloads are proven dead before reuse.
