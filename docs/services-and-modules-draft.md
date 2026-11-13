# Stable service lookup and imported extensions — design draft

Status: stages 1–4 implemented, 2026-11-11. The shipped subset, syntax, and limits
are documented in [the module/service reference](services.md). This design refines the service path in
[the shapes/traits draft](shapes-and-traits-draft.md). Syntax is illustrative;
later optimization and full Cordis integration remain proposals.

## Priority: stable typed service slots

Implement the Cordis-style path before general open-object speculation. A known
service name should compile to a typed key and a stable resolution slot. It
should not perform string hashing or an object-shape lookup on every access.

The important distinction is between a service's identity, its resolution in a
particular context, and the current provider:

```text
ServiceKey<T>             immutable declaration identity and service type
(ContextView, ServiceKey) stable resolution slot
slot.current             current provider binding or unavailable state
ProviderBinding          provider instance, dispatch identity, lifecycle
ServiceLease<T>           admitted access retaining a particular provider
```

The slot's metadata identity survives provider removal and replacement. Its
contents change. Metadata IDs do not get recycled while code/caches may refer to
them. Runtime numeric IDs are linked from module/export identities; two modules'
unrelated services do not alias just because both use the spelling `logger`.

This is not a claim that every context shares one slot. Isolation, injection,
shadowing, and context extension can produce different resolutions. A view has
a dense local slot index for each linked service key it uses. On first access,
the resolver establishes that slot and its dependencies. A computed string or
previously unknown key uses the slower lookup protocol to obtain the same form
of binding access.

## Fast path and invalidation

Conceptually, an ordinary typed call performs:

```text
slot = view.service_slots[linked_slot_index]
binding = slot.current
if !view.admits(binding) || !binding.available_for(view): goto slow_path
lease = acquire(binding)
target = lease.dispatch[method_slot]
call target(lease.receiver, args)
release lease
```

The precise representation may combine some of these checks. `acquire` initially
runs on the Session execution thread and needs no cross-thread synchronization.
The admitted provider, receiver, and executable owner remain rooted until the
call finishes, including VM suspension and reentrant execution. The JIT must
not retain an unrooted raw pointer across a collecting helper.

Cache the route to the slot or a resolution handler, not an immortal copy of
the provider. Scope changes invalidate affected routes; ordinary replacement
updates the existing route's contents when the resolution location stays valid.
Resolution metadata must be rooted/owned appropriately, but IC metadata alone
must not keep retired service instances alive forever.

| Change | Runtime response | Compiled-code response |
| --- | --- | --- |
| Add an unrelated service | Populate that service's slots | None for existing service sites |
| Replace provider with same concrete implementation | Publish new binding/receiver | Existing method-target specialization remains usable |
| Replace with another implementation | Publish new binding/dispatch identity | Target guard misses; indirect fallback remains valid |
| Withdraw provider | Publish unavailable/retiring state | Availability path handles access; ordinary slot code stays installed |
| Reprovide after withdrawal | Publish new binding in the existing slot | Reuse existing code and matching target cases |
| Change isolation/injection/shadow resolution | Update the affected view/key route | Re-resolve those routes; unrelated sites remain valid |
| Add/remove an accessor or access hook | Update its policy/handler dependency | Preserve observable callbacks; refresh only affected handlers |

Avoid a global registry epoch in the normal path. Begin with targeted updates
for the affected service's dependent views. Where dependency tracking requires
an epoch, use a view/key or policy-specific epoch. Publish revocation before
running callbacks that may reenter service lookup. Negative lookups also need
dependencies, so later provision can make a previously missing service visible.

An IC miss or local handler refresh is not automatically a whole-function
deoptimization. Emit a native slow-path branch and rejoin the same compiled
routine after resolving the access when its canonical state permits it.

## Optimize the call target, preserve provider indirection

The first optimized tier should keep loading the current receiver from the
binding and specialize on implementation/dispatch identity:

```text
lease = acquire_current_service(view, key)
if lease.implementation == ConsoleLogger:
    ConsoleLogger.info(lease.receiver, message)
else:
    lease.dispatch.info(lease.receiver, message)
```

Changing from one `ConsoleLogger` instance to another does not break this
assumption. A cached instance pointer or provider-generation equality would
invalidate more often. Generations still distinguish provider lifetimes and
prevent stale admissions; they need not be the optimized method-target key.

Maintain a small target PIC, with an indirect fallback, when replacements use
several implementations. Bound the number of cases and compilation requests;
do not repeatedly compile a new function version for every provider instance.
Hot-swapping a target's code uses the existing stable entry-slot convention.

Inlining an implementation requires an implementation/code-version dependency.
Plugin disposal removes its registration, but executable memory referenced by
active invocations cannot be freed until those invocations release their owners.
Changing a code implementation is a different event from replacing an instance
of the same implementation.

Aggressive provider constant-folding is opt-in to a proven stable admission
scope. Even there, an effectful method can mutate the service's fields; binding
stability does not imply value constness or purity.

## Disposal and admitted work

Use the provider lifecycle:

```text
available -> retiring -> released
```

Revocation stops new unadmitted work. Existing admitted dependents and in-flight
calls retain their provider through cleanup. Once they drain, release the old
provider. A replacement is a new binding identity, even if its implementation
type is unchanged. New consumer work can select the replacement without changing
which provider an old dependent's cleanup is permitted to use.

This distinction matches the inspected Cordis tests for dependent cleanup before
provider teardown and for committed injection views. A globally cached latest
value would lose that distinction. See
[service.spec.ts](/Volumes/Dev/x/cordis/packages/core/tests/service.spec.ts) and
[isolate.spec.ts](/Volumes/Dev/x/cordis/packages/core/tests/isolate.spec.ts).

`ServiceRef<T>` is a context-bound live reference: using it acquires through the
current resolution slot. It is not a snapshot of a provider. An explicit
`with_service`/injection scope yields a scoped `ServiceLease<T>` tied to one
admission. Revocation cannot be bypassed by keeping a live reference in a global;
admitted leases obey cleanup and escape restrictions. Unbounded asynchronous work
requires an explicit cancellable admission contract rather than silently keeping
the provider forever.

Retiring provider access remains allowed only to its admitted cleanup scopes.
Arbitrary new root-context calls must not gain access merely because another
consumer still holds a lease. The resolver must preserve access hooks, tracing,
and context rebinding unless an explicit policy contract permits a faster path.

## Modules and imported extensions

Modern source uses case-sensitive Rust-style module paths: `mod logging;`,
`use crate::logging::{Logger, LOGGER, logger}`, and `pub use` reexports.
The executable syntax and visibility rules are maintained in the
[module/service reference](services.md). Complete module sources are in
[examples/services](../examples/services/main.ya); the generic registry example
is in [examples/lookup-services](../examples/lookup-services/main.ya).

Imports expose declarations without activating providers. Provider activation is
an explicit procedure call returning a registration. Extension getters resolve
lexically and retain their ordinary checked bodies. Actual fields and inherent
members take precedence. Standard types and functions live under `std` modules.

## Implementation order

1. Add module identities, declaration interfaces, explicit imports, typed keys,
   and extension lowering. Provide a minimal trait/erased-call path for service
   interfaces; full generic specialization can follow.
2. Implement dense context-view slots, provider bindings, targeted route updates,
   and lifecycle/admission semantics in the interpreter. Preserve hooks and
   isolation before optimizing them.
3. Emit Winch slot-access fast paths and stable erased method calls. Add native
   local miss/rejoin paths and counters. This should already avoid repeated
   string lookup without speculative provider constants.
4. Add Cranelift method-target PICs, guarded devirtualization, and bounded
   implementation-based versions. Keep receiver/provider loads live.
5. Add proven admission-scope hoisting and inlining with dependency/recovery
   metadata. Measure whether this improves workloads over the stable-slot path.
6. Build general open-object shapes, custom hashing, and broader dynamic
   specialization independently. Service lookup should not wait for them.

Acceptance workloads must include stable lookup, same-type replacement,
alternating implementations, unrelated-service churn, repeated withdraw/reprovide,
isolation changes, hooks, reentrant disposal, cleanup using a retired provider,
and one-instruction VM suspension with GC enabled. Count slot hits, resolution
misses, target PIC misses, compilation requests, full deoptimizations, and retained
providers separately. Ordinary disposal/reprovision should require no full
function deoptimization or compilation request unless a stronger optimization
deliberately introduced a corresponding assumption.
