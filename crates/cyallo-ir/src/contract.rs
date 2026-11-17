//! One contract record per engine intrinsic.
//!
//! Effects, ownership, safepoint behaviour and migration direction used to be
//! spread over `consumes_argument`, `builtins::pure`, `lookup::Operation::writes`,
//! the ownership match and per-op metadata. A contract states what an operation can
//! do, independently of how it is currently lowered. Optimizers and backends consult
//! it to decide what may be inlined, reordered, removed or specialized; a
//! registration API validates embedder-supplied operations against the same shape.
//!
//! Documented in docs/intrinsic-contracts.md; keep that page in step with this file.
//! Values describe the operation's semantics, not today's lowering: for example a
//! scalar cast cannot reach a collection safepoint even though it currently runs
//! through the interpreter helper.
use crate::intrinsic::Intrinsic;
/// Where an operation should live as the library moves into SDKs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Disposition {
    /// Needs engine knowledge of values, memory or the GC; backends lower it.
    EnginePrimitive,
    /// Crosses to the host (I/O, clock, scheduler, loader); an opaque call.
    HostCall,
    /// Pure computation that ordinary Cyallo source can express over primitives.
    SourceCandidate,
}
/// How an operation can interact with collection and guest re-entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Safepoint {
    /// Cannot collect or run guest code: safe to inline between checks.
    Never,
    /// May allocate and therefore collect, but never runs guest code.
    MayCollect,
    /// May run guest code or park the coroutine: all state must be rooted and
    /// resumable.
    MayReenter,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Effects {
    pub reads_memory: bool,
    pub writes_memory: bool,
    pub allocates: bool,
    /// Produces a value with a new identity (cell, channel, clock, symbol, key).
    pub fresh_identity: bool,
    pub nondeterministic: bool,
    pub io: bool,
    /// Can stop the caller with a runtime failure (not a returned `Result`).
    pub may_fail: bool,
    /// Can suspend the coroutine and resume it later.
    pub may_park: bool,
    /// Can call guest code (callbacks, destructors, allocators).
    pub calls_guest: bool,
}
impl Effects {
    pub const NONE: Self = Self {
        reads_memory: false,
        writes_memory: false,
        allocates: false,
        fresh_identity: false,
        nondeterministic: false,
        io: false,
        may_fail: false,
        may_park: false,
        calls_guest: false,
    };
    /// Observable only through its result: removable when unused, foldable when
    /// its operands are constant, reorderable with other pure operations.
    pub fn is_pure(&self) -> bool {
        !(self.writes_memory
            || self.fresh_identity
            || self.nondeterministic
            || self.io
            || self.may_park
            || self.calls_guest)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IntrinsicContract {
    pub disposition: Disposition,
    pub effects: Effects,
    pub safepoint: Safepoint,
    /// Operands are moved into the operation rather than borrowed.
    pub consumes_arguments: bool,
}
impl IntrinsicContract {
    /// A backend may emit this inline between guard checks: it cannot collect,
    /// re-enter, park or have side effects beyond memory it writes in place.
    pub fn inlinable(&self) -> bool {
        self.safepoint == Safepoint::Never && !self.effects.may_park && !self.effects.calls_guest
    }
}
const fn contract(
    disposition: Disposition,
    safepoint: Safepoint,
    effects: Effects,
    consumes_arguments: bool,
) -> IntrinsicContract {
    IntrinsicContract { disposition, effects, safepoint, consumes_arguments }
}
use Disposition::{EnginePrimitive as Engine, HostCall as Host, SourceCandidate as Source};
use Safepoint::{MayCollect as Collect, MayReenter as Reenter, Never};
const NONE: Effects = Effects::NONE;
const FAILS: Effects = Effects { may_fail: true, ..NONE };
const READS: Effects = Effects { reads_memory: true, ..NONE };
const READS_FAILS: Effects = Effects { reads_memory: true, may_fail: true, ..NONE };
const ALLOCATES: Effects = Effects { allocates: true, ..NONE };
const ALLOCATES_FAILS: Effects = Effects { allocates: true, may_fail: true, ..NONE };
const READS_ALLOCATES: Effects = Effects { reads_memory: true, allocates: true, ..NONE };
const READS_ALLOCATES_FAILS: Effects =
    Effects { reads_memory: true, allocates: true, may_fail: true, ..NONE };
const WRITES: Effects = Effects { writes_memory: true, ..NONE };
const FRESH: Effects = Effects { allocates: true, fresh_identity: true, ..NONE };
impl Intrinsic {
    /// The operation's contract. The match has no wildcard: adding an intrinsic
    /// requires deciding its contract.
    pub fn contract(self) -> IntrinsicContract {
        use Intrinsic::*;
        match self {
            // Scalar arithmetic and conversion. No heap, no collection.
            // `may_fail` is conservative where overflow or domain errors are
            // possible; per-operation tightening is future work.
            Abs | Min | Max | Clamp | NumAbs | NumMin | NumMax | NumClamp | Round | Floor
            | Ceil | Sqrt | Pow | FloatRound | FloatFloor | FloatCeil | FloatSqrt | FloatPow
            | Truncate | CharFromCode | CastI8 | CastI16 | CastI32 | CastI64 | CastU8
            | CastU16 | CastU32 | CastU64 | CastIsize | CastUsize | CastF32 | CastF64 => {
                contract(Engine, Never, FAILS, false)
            }
            LowerChar | UpperChar | CharCode => contract(Source, Never, NONE, false),
            // Lengths read the operand's header.
            Length | StringLength | ArrayLength => contract(Engine, Never, READS, false),
            // Strings: pure, source-expressible over string index/slice primitives.
            Contains | StartsWith | EndsWith | IsNumeric => contract(Source, Never, READS, false),
            Left | Right | Mid => contract(Source, Collect, READS_ALLOCATES_FAILS, false),
            Trim | Upper | Lower | Replace | StringFrom => {
                contract(Source, Collect, READS_ALLOCATES, false)
            }
            ParseLegacy => contract(Source, Never, READS_FAILS, false),
            Parse => contract(Source, Collect, READS_ALLOCATES, false),
            // Sparse-array and numeric-collection helpers.
            Keys | Values => contract(Source, Collect, READS_ALLOCATES, false),
            Count => contract(Source, Never, READS, false),
            Sum | ArraySum => contract(Source, Never, READS_FAILS, false),
            // Constructors of aggregate values.
            OptionNone => contract(Engine, Never, NONE, false),
            OptionSome | ResultOk | ResultErr => contract(Engine, Collect, ALLOCATES, true),
            VecLiteral | DequeLiteral => contract(Engine, Collect, ALLOCATES, true),
            VecFrom | DequeFrom | ArrayFrom => contract(Engine, Collect, READS_ALLOCATES, false),
            ArrayEmpty => contract(Engine, Collect, ALLOCATES, false),
            // Memory, reference counting and the GC.
            Allocate => contract(
                Engine,
                Collect,
                Effects { writes_memory: true, allocates: true, may_fail: true, ..NONE },
                true,
            ),
            Deallocate => contract(Engine, Collect, Effects { may_fail: true, ..WRITES }, true),
            LayoutNew | AllocErrorNew => contract(Engine, Collect, ALLOCATES_FAILS, false),
            RcTryNewIn => contract(
                Engine,
                Reenter,
                Effects { writes_memory: true, allocates: true, calls_guest: true, ..NONE },
                true,
            ),
            RcClone | RcDowngrade | RcUpgrade => contract(Engine, Never, Effects { reads_memory: true, ..WRITES }, false),
            RcStrongCount | RcWeakCount => contract(Engine, Never, READS, false),
            GcClone => contract(Engine, Collect, READS_ALLOCATES, false),
            CloneValue => contract(Engine, Collect, READS_ALLOCATES_FAILS, false),
            DropValue => contract(
                Engine,
                Reenter,
                Effects { writes_memory: true, calls_guest: true, may_fail: true, ..NONE },
                true,
            ),
            CellNew => contract(Engine, Collect, FRESH, true),
            FieldLocation | MissingPath => contract(Engine, Collect, READS_ALLOCATES, false),
            LookupPath => contract(Engine, Collect, READS_ALLOCATES_FAILS, true),
            MappedLookup => contract(
                Engine,
                Reenter,
                Effects { reads_memory: true, allocates: true, calls_guest: true, may_fail: true, ..NONE },
                false,
            ),
            Erase => contract(Engine, Collect, ALLOCATES, true),
            Checked => contract(Engine, Collect, ALLOCATES_FAILS, true),
            FreshSymbol => contract(Engine, Collect, FRESH, false),
            ObjectView => contract(Engine, Collect, READS_ALLOCATES, false),
            ObjectFreeze => contract(Engine, Never, Effects { reads_memory: true, ..WRITES }, false),
            ContextNew | ServiceKeyNew => contract(Engine, Collect, FRESH, false),
            // Host boundary.
            ModuleLoad => contract(
                Host,
                Reenter,
                Effects { io: true, nondeterministic: true, may_park: true, may_fail: true, allocates: true, ..NONE },
                false,
            ),
            ChannelBounded | ChannelUnbounded | ClockNew => {
                contract(Host, Collect, Effects { may_fail: true, ..FRESH }, false)
            }
            TaskSpawn => contract(
                Host,
                Collect,
                Effects { nondeterministic: true, may_fail: true, ..FRESH },
                true,
            ),
            Random => contract(Host, Never, Effects { nondeterministic: true, may_fail: true, ..NONE }, false),
            Print => contract(
                Host,
                Reenter,
                Effects { io: true, may_park: true, may_fail: true, reads_memory: true, ..NONE },
                false,
            ),
            Input => contract(
                Host,
                Reenter,
                Effects { io: true, may_park: true, may_fail: true, writes_memory: true, ..NONE },
                false,
            ),
        }
    }
    /// Ownership is part of each intrinsic declaration's parameter contract.
    pub fn consumes_argument(self, _index: usize) -> bool {
        self.contract().consumes_arguments
    }
}

/// How one operand reaches a member operation.
pub use crate::declarations::NativeOperandMode as Operand;
/// Operand modes by position: the receiver (index 0), the remaining operands, and
/// positional exceptions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OperandModes {
    pub receiver: Operand,
    pub rest: Operand,
    pub exceptions: &'static [(usize, Operand)],
}
impl OperandModes {
    pub fn mode(&self, index: usize) -> Operand {
        if let Some((_, mode)) = self.exceptions.iter().find(|(i, _)| *i == index) {
            return *mode;
        }
        if index == 0 { self.receiver } else { self.rest }
    }
}
/// The contract of a member intrinsic: the same effect record as an intrinsic plus
/// how each operand is passed. Member identities are independent of the source
/// member's name and owner path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MemberContract {
    pub disposition: Disposition,
    pub effects: Effects,
    pub safepoint: Safepoint,
    pub operands: OperandModes,
}
impl MemberContract {
    pub fn inlinable(&self) -> bool {
        self.safepoint == Safepoint::Never && !self.effects.may_park && !self.effects.calls_guest
    }
    /// True when any operand is moved into the operation.
    pub fn consumes(&self) -> bool {
        self.operands.rest == Operand::Transfer
            || self.operands.receiver == Operand::Transfer
            || self.operands.exceptions.iter().any(|(_, m)| *m == Operand::Transfer)
    }
}
const fn member(
    disposition: Disposition,
    safepoint: Safepoint,
    effects: Effects,
    operands: OperandModes,
) -> MemberContract {
    MemberContract { disposition, effects, safepoint, operands }
}
const fn modes(receiver: Operand, rest: Operand) -> OperandModes {
    OperandModes { receiver, rest, exceptions: &[] }
}
/// All operands read (copy or borrow).
const READ_ALL: OperandModes = modes(Operand::Read, Operand::Read);
/// The receiver is moved in; the rest are read.
const TAKES_SELF: OperandModes = modes(Operand::Transfer, Operand::Read);
/// A mutable receiver; the rest are moved in (push, generator resume).
const MUTATES_SELF: OperandModes = modes(Operand::MutableAddress, Operand::Transfer);
const SHARED_SELF: OperandModes = modes(Operand::SharedAddress, Operand::Read);
impl crate::internal::MemberIntrinsic {
    /// The member's contract. No wildcard arm: a new member needs a decision.
    pub fn contract(self) -> MemberContract {
        use crate::internal::MemberIntrinsic::*;
        let reenter_guest = |extra: Effects| Effects { calls_guest: true, may_fail: true, ..extra };
        match self {
            // Generators run guest code on their own frame; frame-coupled.
            GeneratorStart | GeneratorResume | GeneratorNext => member(
                Engine,
                Reenter,
                reenter_guest(Effects { reads_memory: true, writes_memory: true, allocates: true, ..NONE }),
                MUTATES_SELF,
            ),
            GeneratorClose => member(
                Engine,
                Reenter,
                reenter_guest(Effects { writes_memory: true, ..NONE }),
                MUTATES_SELF,
            ),
            // Thin readers over a step, outcome or error value: source-expressible.
            GeneratorIsComplete | GeneratorIsYielded | ResultIsOk | ResultIsErr | OptionIsSome
            | OptionIsNone | SendErrorClosed | SendErrorFull | SendErrorLimit | LoadErrorNotFound
            | LoadErrorDenied | LoadErrorCheck | ReceiveErrorClosed | ReceiveErrorEmpty
            | TaskIsCompleted | TaskIsFailed | TaskIsCancelled | TaskIsStopped => {
                member(Source, Never, READS, READ_ALL)
            }
            // Extraction that fails on the wrong variant (consumes the value).
            GeneratorYielded | GeneratorCompleted | ResultUnwrap | ResultUnwrapErr | OptionUnwrap
            | OptionOrFail | SendErrorValue | TaskCompleted => member(
                Source,
                Never,
                Effects { reads_memory: true, may_fail: true, ..NONE },
                TAKES_SELF,
            ),
            LayoutSize | LayoutAlign => member(Engine, Never, READS, READ_ALL),
            // Reference counts and clones.
            RcClone | RcDowngrade | RcUpgrade => {
                member(Engine, Never, Effects { reads_memory: true, writes_memory: true, ..NONE }, READ_ALL)
            }
            RcStrongCount | RcWeakCount => member(Engine, Never, READS, READ_ALL),
            GcClone => member(Engine, Collect, Effects { reads_memory: true, allocates: true, ..NONE }, READ_ALL),
            // Slices and collections.
            Slice => member(Engine, Collect, Effects { reads_memory: true, allocates: true, ..NONE }, SHARED_SELF),
            MutableSlice => member(
                Engine,
                Collect,
                Effects { reads_memory: true, allocates: true, ..NONE },
                modes(Operand::MutableAddress, Operand::Read),
            ),
            CollectionPushBack | CollectionPushFront => member(
                Engine,
                Collect,
                Effects { reads_memory: true, writes_memory: true, allocates: true, may_fail: true, ..NONE },
                MUTATES_SELF,
            ),
            CollectionPopBack | CollectionPopFront => member(
                Engine,
                Collect,
                Effects { reads_memory: true, writes_memory: true, allocates: true, ..NONE },
                MUTATES_SELF,
            ),
            CollectionLength => member(Engine, Never, READS, SHARED_SELF),
            RangeReverse => member(Source, Collect, Effects { reads_memory: true, allocates: true, ..NONE }, READ_ALL),
            // Cells and locations. Storing drops the old value, which can run a
            // destructor; replacing hands the old value back.
            CellReplace => member(
                Engine,
                Never,
                Effects { reads_memory: true, writes_memory: true, ..NONE },
                OperandModes {
                    receiver: Operand::SharedAddress,
                    rest: Operand::Read,
                    exceptions: &[(1, Operand::Transfer)],
                },
            ),
            LocationLoad => member(
                Engine,
                Collect,
                Effects { reads_memory: true, allocates: true, may_fail: true, ..NONE },
                READ_ALL,
            ),
            LocationStore => member(
                Engine,
                Reenter,
                Effects { reads_memory: true, writes_memory: true, calls_guest: true, may_fail: true, ..NONE },
                OperandModes {
                    receiver: Operand::Read,
                    rest: Operand::Read,
                    exceptions: &[(1, Operand::Transfer)],
                },
            ),
            StoragePathField | StoragePathElement | StoragePathTarget => member(
                Engine,
                Collect,
                Effects { reads_memory: true, allocates: true, may_fail: true, ..NONE },
                TAKES_SELF,
            ),
            // Channels, clocks and tasks cross to the scheduler.
            ChannelClone | ClockClone => member(
                Host,
                Collect,
                Effects { writes_memory: true, allocates: true, ..NONE },
                READ_ALL,
            ),
            ChannelClose | TaskCancel => {
                member(Host, Never, Effects { writes_memory: true, nondeterministic: true, ..NONE }, READ_ALL)
            }
            ChannelIsClosed | TaskIsFinished => {
                member(Host, Never, Effects { reads_memory: true, nondeterministic: true, ..NONE }, READ_ALL)
            }
            ChannelSend => member(
                Host,
                Reenter,
                Effects { writes_memory: true, nondeterministic: true, may_park: true, may_fail: true, allocates: true, ..NONE },
                OperandModes { receiver: Operand::Read, rest: Operand::Read, exceptions: &[(1, Operand::Transfer)] },
            ),
            ChannelTrySend => member(
                Host,
                Collect,
                Effects { writes_memory: true, nondeterministic: true, allocates: true, ..NONE },
                OperandModes { receiver: Operand::Read, rest: Operand::Read, exceptions: &[(1, Operand::Transfer)] },
            ),
            ChannelReceive => member(
                Host,
                Reenter,
                Effects { writes_memory: true, nondeterministic: true, may_park: true, may_fail: true, allocates: true, ..NONE },
                READ_ALL,
            ),
            ChannelTryReceive => member(
                Host,
                Collect,
                Effects { writes_memory: true, nondeterministic: true, allocates: true, ..NONE },
                READ_ALL,
            ),
            ClockSleep => member(
                Host,
                Reenter,
                Effects { nondeterministic: true, may_park: true, ..NONE },
                READ_ALL,
            ),
            TaskJoin => member(
                Host,
                Reenter,
                Effects { writes_memory: true, nondeterministic: true, may_park: true, may_fail: true, allocates: true, ..NONE },
                TAKES_SELF,
            ),
            // Error message text.
            LoadErrorMessage | ViewErrorMessage => {
                member(Source, Collect, Effects { reads_memory: true, allocates: true, ..NONE }, READ_ALL)
            }
            // Service contexts.
            ContextExtend | ContextIsolate => member(
                Engine,
                Collect,
                Effects { allocates: true, fresh_identity: true, ..NONE },
                READ_ALL,
            ),
            ContextService => member(
                Engine,
                Collect,
                Effects { reads_memory: true, allocates: true, may_fail: true, ..NONE },
                READ_ALL,
            ),
            ContextAvailable => member(Engine, Never, READS, READ_ALL),
            ContextProvide => member(
                Engine,
                Collect,
                Effects { reads_memory: true, writes_memory: true, allocates: true, may_fail: true, ..NONE },
                OperandModes { receiver: Operand::Read, rest: Operand::Read, exceptions: &[(2, Operand::Transfer)] },
            ),
            ContextInject => member(
                Engine,
                Reenter,
                reenter_guest(Effects { reads_memory: true, writes_memory: true, ..NONE }),
                OperandModes { receiver: Operand::Read, rest: Operand::Read, exceptions: &[(2, Operand::Transfer)] },
            ),
            ContextOnDispose => member(
                Engine,
                Collect,
                Effects { writes_memory: true, allocates: true, ..NONE },
                OperandModes { receiver: Operand::Read, rest: Operand::Read, exceptions: &[(1, Operand::Transfer)] },
            ),
            ContextDispose | RegistrationDispose => member(
                Engine,
                Reenter,
                reenter_guest(Effects { writes_memory: true, ..NONE }),
                READ_ALL,
            ),
        }
    }
}

/// What the optimizer and GC metadata need to know about a lowered lookup
/// operation. It is derived from the intrinsic or member the operation implements;
/// operations with no single origin get the most conservative contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OperationContract {
    pub safepoint: Safepoint,
    pub effects: Effects,
}
impl OperationContract {
    /// Anything may happen: collects, runs guest code, fails.
    pub const CONSERVATIVE: Self = Self {
        safepoint: Safepoint::MayReenter,
        effects: Effects {
            reads_memory: true,
            writes_memory: true,
            allocates: true,
            fresh_identity: false,
            nondeterministic: false,
            io: false,
            may_fail: true,
            may_park: false,
            calls_guest: true,
        },
    };
}
impl From<IntrinsicContract> for OperationContract {
    fn from(c: IntrinsicContract) -> Self {
        Self { safepoint: c.safepoint, effects: c.effects }
    }
}
impl From<MemberContract> for OperationContract {
    fn from(c: MemberContract) -> Self {
        Self { safepoint: c.safepoint, effects: c.effects }
    }
}
impl crate::concurrent::Operation {
    fn contract(self) -> OperationContract {
        use crate::concurrent::Operation as C;
        use crate::internal::MemberIntrinsic as M;
        let member = |m: M| OperationContract::from(m.contract());
        let intrinsic = |i: Intrinsic| OperationContract::from(i.contract());
        match self {
            C::Bounded => intrinsic(Intrinsic::ChannelBounded),
            C::Unbounded => intrinsic(Intrinsic::ChannelUnbounded),
            C::Clock => intrinsic(Intrinsic::ClockNew),
            C::Spawn => intrinsic(Intrinsic::TaskSpawn),
            C::Clone => member(M::ChannelClone),
            C::Close => member(M::ChannelClose),
            C::IsClosed => member(M::ChannelIsClosed),
            C::Send => member(M::ChannelSend),
            C::TrySend => member(M::ChannelTrySend),
            C::Receive => member(M::ChannelReceive),
            C::TryReceive => member(M::ChannelTryReceive),
            C::SendErrorValue => member(M::SendErrorValue),
            C::SendErrorClosed => member(M::SendErrorClosed),
            C::SendErrorFull => member(M::SendErrorFull),
            C::SendErrorLimit => member(M::SendErrorLimit),
            C::ReceiveErrorClosed => member(M::ReceiveErrorClosed),
            C::ReceiveErrorEmpty => member(M::ReceiveErrorEmpty),
            C::Sleep => member(M::ClockSleep),
            C::Join => member(M::TaskJoin),
            C::Cancel => member(M::TaskCancel),
            C::IsFinished => member(M::TaskIsFinished),
            C::Completed => member(M::TaskCompleted),
            C::IsCompleted => member(M::TaskIsCompleted),
            C::IsFailed => member(M::TaskIsFailed),
            C::IsCancelled => member(M::TaskIsCancelled),
            C::IsStopped => member(M::TaskIsStopped),
        }
    }
}
impl crate::lookup::Operation {
    pub fn contract(self) -> OperationContract {
        use crate::internal::MemberIntrinsic as M;
        use crate::lookup::Operation as O;
        let member = |m: M| OperationContract::from(m.contract());
        let intrinsic = |i: Intrinsic| OperationContract::from(i.contract());
        match self {
            O::ObjectView => intrinsic(Intrinsic::ObjectView),
            O::ObjectFreeze => intrinsic(Intrinsic::ObjectFreeze),
            O::ViewErrorMessage => member(M::ViewErrorMessage),
            O::ModuleLoad => intrinsic(Intrinsic::ModuleLoad),
            O::LoadErrorNotFound => member(M::LoadErrorNotFound),
            O::LoadErrorDenied => member(M::LoadErrorDenied),
            O::LoadErrorCheck => member(M::LoadErrorCheck),
            O::LoadErrorMessage => member(M::LoadErrorMessage),
            O::Concurrent(operation) => operation.contract(),
            O::GeneratorStart => member(M::GeneratorStart),
            O::GeneratorResume(_) => member(M::GeneratorResume),
            O::GeneratorNext => member(M::GeneratorNext),
            O::GeneratorClose => member(M::GeneratorClose),
            O::GeneratorIsComplete => member(M::GeneratorIsComplete),
            O::GeneratorIsYielded => member(M::GeneratorIsYielded),
            O::GeneratorYielded => member(M::GeneratorYielded),
            O::GeneratorCompleted => member(M::GeneratorCompleted),
            O::LayoutNew => intrinsic(Intrinsic::LayoutNew),
            O::LayoutSize => member(M::LayoutSize),
            O::LayoutAlign => member(M::LayoutAlign),
            O::Allocate => intrinsic(Intrinsic::Allocate),
            O::Deallocate => intrinsic(Intrinsic::Deallocate),
            O::AllocErrorNew => intrinsic(Intrinsic::AllocErrorNew),
            O::ResultNew(_) => intrinsic(Intrinsic::ResultOk),
            O::RcNewIn { .. } => intrinsic(Intrinsic::RcTryNewIn),
            O::RcClone => member(M::RcClone),
            O::RcDowngrade => member(M::RcDowngrade),
            O::RcUpgrade => member(M::RcUpgrade),
            O::RcStrongCount => member(M::RcStrongCount),
            O::RcWeakCount => member(M::RcWeakCount),
            O::GcClone => member(M::GcClone),
            O::DropValue => intrinsic(Intrinsic::DropValue),
            O::CloneValue => intrinsic(Intrinsic::CloneValue),
            O::Parse(_) => intrinsic(Intrinsic::Parse),
            O::IsOk => member(M::ResultIsOk),
            O::IsErr => member(M::ResultIsErr),
            O::UnwrapOk => member(M::ResultUnwrap),
            O::UnwrapErr => member(M::ResultUnwrapErr),
            O::Slice(_) => member(M::Slice),
            O::CollectionLiteral(_) => intrinsic(Intrinsic::VecLiteral),
            O::CollectionFrom(_) => intrinsic(Intrinsic::VecFrom),
            O::CollectionPush(_) => member(M::CollectionPushBack),
            O::CollectionPop(_) => member(M::CollectionPopBack),
            O::CollectionLength => member(M::CollectionLength),
            O::RangeReverse => member(M::RangeReverse),
            O::ArrayFrom => intrinsic(Intrinsic::ArrayFrom),
            O::CellNew => intrinsic(Intrinsic::CellNew),
            O::Empty => intrinsic(Intrinsic::ArrayEmpty),
            O::CellReplace => member(M::CellReplace),
            O::LocationLoad => member(M::LocationLoad),
            O::LocationStore => member(M::LocationStore),
            O::Some => intrinsic(Intrinsic::OptionSome),
            O::None => intrinsic(Intrinsic::OptionNone),
            O::IsSome => member(M::OptionIsSome),
            O::IsNone => member(M::OptionIsNone),
            O::Unwrap => member(M::OptionUnwrap),
            O::Erase => intrinsic(Intrinsic::Erase),
            O::Checked => intrinsic(Intrinsic::Checked),
            O::Symbol => intrinsic(Intrinsic::FreshSymbol),
            O::Field(_) | O::Element | O::Target => member(M::StoragePathField),
            O::Missing => intrinsic(Intrinsic::MissingPath),
            O::Mapped => intrinsic(Intrinsic::MappedLookup),
            // No single intrinsic or member: layout for Rc, managed construction,
            // ranges, raw cell reads/writes, native callables, storage paths.
            O::NativeCallable(_)
            | O::LayoutForRc
            | O::ManagedNew(..)
            | O::Range(_)
            | O::CellRead
            | O::CellWrite
            | O::StoragePathNew => OperationContract::CONSERVATIVE,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use Intrinsic::*;
    /// The previous hand-written lists, kept as an oracle for the migration.
    fn old_consumes(i: Intrinsic) -> bool {
        matches!(
            i,
            TaskSpawn | CellNew | Allocate | Deallocate | ResultOk | ResultErr | RcTryNewIn
                | VecLiteral | DequeLiteral | OptionSome | Erase | Checked | LookupPath | DropValue
        )
    }
    fn old_pure(i: Intrinsic) -> bool {
        !matches!(
            i,
            Random | Print | Input | FreshSymbol | CellNew | ContextNew | ServiceKeyNew | MappedLookup
        )
    }
    #[test]
    fn every_intrinsic_has_a_contract_matching_the_previous_ownership_list() {
        assert_eq!(Intrinsic::ALL.len(), 100);
        for &i in Intrinsic::ALL {
            assert_eq!(i.contract().consumes_arguments, old_consumes(i), "{i}");
            assert_eq!(i.consumes_argument(0), old_consumes(i), "{i}");
        }
    }
    #[test]
    fn purity_is_never_weaker_than_the_previous_builtin_rule() {
        // Intrinsics the old rule called pure but whose contract writes or runs
        // guest code. They lower through `Op::Lookup`, never `Op::Builtin`, so the
        // old rule was never consulted for them. `builtins::pure` now answers from
        // the contract; only `Op::Builtin` consults it.
        let lookup_lowered = [
            Allocate, Deallocate, RcTryNewIn, RcClone, RcDowngrade, RcUpgrade, DropValue,
            ObjectFreeze, ChannelBounded, ChannelUnbounded, ClockNew, TaskSpawn, ModuleLoad,
        ];
        for &i in Intrinsic::ALL {
            let pure = i.contract().effects.is_pure();
            if pure {
                assert!(old_pure(i), "{i} is pure in its contract but not in the old rule");
            } else if old_pure(i) {
                assert!(lookup_lowered.contains(&i), "{i} became impure without a reason");
            }
        }
    }
    #[test]
    fn contracts_are_internally_consistent() {
        for &i in Intrinsic::ALL {
            let c = i.contract();
            let e = c.effects;
            // Parking or guest calls imply re-entry; collection implies allocation
            // or reentry.
            if e.may_park || e.calls_guest {
                assert_eq!(c.safepoint, Safepoint::MayReenter, "{i}");
            }
            if e.allocates {
                assert_ne!(c.safepoint, Safepoint::Never, "{i} allocates at a safepoint");
            }
            // Source candidates are pure computation.
            if c.disposition == Disposition::SourceCandidate {
                assert!(e.is_pure() && c.safepoint != Safepoint::MayReenter, "{i}");
            }
            // Host calls are never inlinable; io and nondeterminism are impure.
            if e.io || e.nondeterministic {
                assert!(!e.is_pure(), "{i}");
            }
            if c.disposition == Disposition::HostCall && e.may_park {
                assert!(!c.inlinable(), "{i}");
            }
        }
    }
    #[test]
    fn first_native_lowering_targets_are_inlinable() {
        for i in [CastI8, CastI64, CastU32, CastF64, Length, StringLength, ArrayLength, RcStrongCount, OptionNone] {
            assert!(i.contract().inlinable(), "{i}");
        }
        for i in [OptionSome, VecFrom, Print, TaskSpawn, DropValue, MappedLookup, ModuleLoad] {
            assert!(!i.contract().inlinable(), "{i}");
        }
        assert!(Intrinsic::ALL.iter().filter(|i| i.contract().inlinable()).count() >= 25);
    }
    #[test]
    fn dispositions_partition_the_catalogue_as_audited() {
        let count = |d| Intrinsic::ALL.iter().filter(|i| i.contract().disposition == d).count();
        let (engine, host, source) = (
            count(Disposition::EnginePrimitive),
            count(Disposition::HostCall),
            count(Disposition::SourceCandidate),
        );
        assert_eq!(engine + host + source, 100);
        assert_eq!(host, 8); // ModuleLoad, channels (2), ClockNew, TaskSpawn, Random, Print, Input
        // Strings, chars, Sum/Count/Keys/Values and parsing are source candidates.
        // Numeric math and casts stay engine primitives: they need float/integer
        // primitives the optimizer lowers directly (the audit's "~40" counted them).
        assert_eq!(source, 22);
    }
    /// The previous hand-written operand table for members, kept as an oracle.
    fn old_member_mode(member: crate::internal::MemberIntrinsic, index: usize) -> Operand {
        use Operand::*;
        {
            use crate::internal::MemberIntrinsic::*;
            match member {
                GeneratorStart | GeneratorResume | GeneratorNext | GeneratorClose
                | CollectionPushBack | CollectionPushFront | CollectionPopBack
                | CollectionPopFront => {
                    if index == 0 {
                        MutableAddress
                    } else {
                        Transfer
                    }
                }
                MutableSlice if index == 0 => MutableAddress,
                Slice | CollectionLength if index == 0 => SharedAddress,
                GeneratorYielded | GeneratorCompleted | ResultUnwrap | ResultUnwrapErr
                | OptionUnwrap | OptionOrFail | StoragePathField | StoragePathElement
                | StoragePathTarget | SendErrorValue | TaskJoin | TaskCompleted => {
                    if index == 0 {
                        Transfer
                    } else {
                        Read
                    }
                }
                CellReplace if index == 0 => SharedAddress,
                CellReplace | LocationStore | ContextOnDispose => {
                    if index == 1 {
                        Transfer
                    } else {
                        Read
                    }
                }
                ContextProvide | ContextInject => {
                    if index == 2 {
                        Transfer
                    } else {
                        Read
                    }
                }
                GeneratorIsComplete | GeneratorIsYielded | LayoutSize | LayoutAlign | RcClone
                | RcDowngrade | RcUpgrade | RcStrongCount | RcWeakCount | GcClone | ResultIsOk
                | ResultIsErr | Slice | MutableSlice | CollectionLength | RangeReverse
                | LocationLoad | OptionIsSome | OptionIsNone | ChannelClone | ChannelClose
                | ChannelIsClosed | ChannelSend | ChannelTrySend | ChannelReceive
                | ChannelTryReceive | SendErrorClosed | SendErrorFull | SendErrorLimit
                | LoadErrorNotFound | LoadErrorDenied | LoadErrorCheck | ViewErrorMessage
                | LoadErrorMessage | ReceiveErrorClosed | ReceiveErrorEmpty | ClockClone
                | ClockSleep | TaskCancel | TaskIsFinished | TaskIsCompleted | TaskIsFailed
                | TaskIsCancelled | TaskIsStopped | ContextExtend | ContextIsolate
                | ContextService | ContextAvailable | ContextDispose | RegistrationDispose => {
                    if matches!(member, ChannelSend | ChannelTrySend) && index == 1 {
                        Transfer
                    } else {
                        Read
                    }
                }
            }
        }
    }

    #[test]
    fn member_operand_modes_match_the_previous_table() {
        use crate::internal::MemberIntrinsic;
        assert_eq!(MemberIntrinsic::ALL.len(), 75);
        for &m in MemberIntrinsic::ALL {
            for index in 0..5 {
                assert_eq!(m.contract().operands.mode(index), old_member_mode(m, index), "{m:?} operand {index}");
            }
        }
    }
    #[test]
    fn member_contracts_are_internally_consistent() {
        use crate::internal::MemberIntrinsic;
        for &m in MemberIntrinsic::ALL {
            let c = m.contract();
            let e = c.effects;
            if e.may_park || e.calls_guest {
                assert_eq!(c.safepoint, Safepoint::MayReenter, "{m:?}");
            }
            if e.allocates {
                assert_ne!(c.safepoint, Safepoint::Never, "{m:?} allocates at a safepoint");
            }
            if c.disposition == Disposition::SourceCandidate {
                assert!(e.is_pure() && c.safepoint != Safepoint::MayReenter, "{m:?}");
            }
            if e.io || e.nondeterministic {
                assert!(!e.is_pure(), "{m:?}");
            }
            // Mutating a receiver in place needs a mutable address.
            if e.writes_memory && c.operands.receiver == Operand::Read {
                // Writes through a handle (channels, tasks, locations, contexts) are
                // allowed; they never go through a value-receiver copy.
                assert!(
                    c.disposition != Disposition::SourceCandidate,
                    "{m:?}: a source candidate cannot write"
                );
            }
        }
    }
    #[test]
    fn member_dispositions_and_inlinable_set_are_pinned() {
        use crate::internal::MemberIntrinsic;
        let count = |d| MemberIntrinsic::ALL.iter().filter(|m| m.contract().disposition == d).count();
        let (engine, host, source) = (
            count(Disposition::EnginePrimitive),
            count(Disposition::HostCall),
            count(Disposition::SourceCandidate),
        );
        assert_eq!(engine + host + source, 75);
        assert_eq!((engine, host, source), (34, 12, 29));
        for m in [MemberIntrinsic::RcStrongCount, MemberIntrinsic::CollectionLength, MemberIntrinsic::LayoutSize, MemberIntrinsic::OptionIsSome, MemberIntrinsic::CellReplace] {
            assert!(m.contract().inlinable(), "{m:?}");
        }
        for m in [MemberIntrinsic::GeneratorResume, MemberIntrinsic::ChannelSend, MemberIntrinsic::TaskJoin, MemberIntrinsic::ContextDispose, MemberIntrinsic::LocationStore] {
            assert!(!m.contract().inlinable(), "{m:?}");
        }
    }

    #[test]
    fn lookup_operations_inherit_their_origin_contracts() {
        use crate::internal::MemberIntrinsic as M;
        use crate::lookup::Operation as O;
        // Shared origins agree: an operation implemented by both an intrinsic and a
        // member must not get a different safepoint depending on the route.
        for (i, m) in [
            (Intrinsic::RcClone, M::RcClone),
            (Intrinsic::RcDowngrade, M::RcDowngrade),
            (Intrinsic::RcUpgrade, M::RcUpgrade),
            (Intrinsic::RcStrongCount, M::RcStrongCount),
            (Intrinsic::RcWeakCount, M::RcWeakCount),
            (Intrinsic::GcClone, M::GcClone),
        ] {
            assert_eq!(i.contract().safepoint, m.contract().safepoint, "{i}");
            assert_eq!(i.contract().effects, m.contract().effects, "{i}");
        }
        // Operations with no single origin are fully conservative.
        for op in [O::LayoutForRc, O::Range(false), O::CellRead, O::CellWrite, O::StoragePathNew, O::ManagedNew(cyallo_ast::MemoryKind::Gc, false)] {
            assert_eq!(op.contract(), OperationContract::CONSERVATIVE, "{op:?}");
        }
        // Operations that cannot collect, so metadata need not treat them as safepoints.
        for op in [O::RcStrongCount, O::CollectionLength, O::IsSome, O::IsNone, O::LayoutSize, O::CellReplace, O::GeneratorIsComplete, O::LoadErrorNotFound] {
            assert_eq!(op.contract().safepoint, Safepoint::Never, "{op:?}");
        }
        for op in [O::Some, O::CollectionPush(false), O::GeneratorResume(crate::lookup::ResumeInput::Unit), O::DropValue, O::Mapped, O::ModuleLoad] {
            assert_ne!(op.contract().safepoint, Safepoint::Never, "{op:?}");
        }
    }
}
