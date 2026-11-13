//! C/T/P execution: coroutines own continuations, OS threads execute them, and
//! processors grant permission to run. Waiting never reserves a processor.
//!
//! This module accepts owned `Send` continuations. Guest heap references must
//! pass the language's transport checks before being placed in one.
mod scheduler;
mod error;
pub use error::{Failure, StartError};
pub use cyallo_host::HostError;
pub use cyallo_io::PanicPayload;
pub use cyallo_clock::{Clock, Sleep};
pub use cyallo_io::{Cancelled, Completer, Operation, OperationId, OperationRegistry};
pub use scheduler::{
    Cancellation, Config, Context, Coroutine, CoroutineId, ExecutionPermit, Group, GroupWait, Outcome, Runtime,
    SpawnError, Spawner, Stats, Task,
};
