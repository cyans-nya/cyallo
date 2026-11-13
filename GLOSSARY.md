# Cyallo runtime

Terms used when discussing the execution and lifetime of language data.

## Execution

**Session**:
A running language execution with globals, invocation state, input, and output
that persist across pauses and resumptions.

## Managed lifetime

**Session heap**:
The managed language data belonging to one Session. Managed objects are not
shared between Sessions.

**Managed-memory limit**:
The maximum storage assigned to a Session's managed data and its collector,
including collection reserve and collector bookkeeping.
_Avoid_: Live-data limit, process memory limit.

**Host borrow**:
Scoped host access to a managed object's current address, valid only while
allocation, collection, and reentrant language execution cannot occur.
_Avoid_: Pinned pointer.

**Native root handle**:
An explicitly registered strong root that keeps a managed object reachable for
native execution. An ordinary managed reference is not automatically a native root.
_Avoid_: Raw native pointer, when referring to a registered root.

**Managed object reference**:
A reference to Session-owned language data whose representation may change when
collection moves the object. A native or host temporary holding one does not
automatically keep the object reachable.
_Avoid_: Root handle, stable object ID.

**Invocation root scope**:
The roots retained for a language invocation, including its suspended state.
Its lifetime includes budget pauses, input suspension, and execution-tier changes.
_Avoid_: Native call scope.

**Collecting operation**:
A runtime operation permitted to run the Session's collector or reenter language
execution that can collect. A collecting operation does not by itself imply a
language-visible procedure effect.

## Language storage

**Cell<T>**: Owned interior-mutable storage. Sharing a cell requires an explicit
managed owner, for example `@gc std::cell::Cell<T>`.

**Location<T>**: A strong retained reference to a statically checked field.
It remains valid after the source binding leaves scope.

**StoragePath<T>**: A relative recipe evaluated against a receiver. It does not
retain a collection element's address.

**Intrinsic identity**: Opaque identity attached to a standard declaration during
resolution. Type analysis instantiates its contract before emitting typed IR.
Source spellings and diagnostic labels do not select backend operations.
