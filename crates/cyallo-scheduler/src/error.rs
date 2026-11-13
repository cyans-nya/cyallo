use cyallo_io::PanicPayload;
use std::{error::Error, fmt, sync::Arc};

#[derive(Debug, Clone)]
pub enum Failure {
    Guest(Arc<dyn Error + Send + Sync>),
    Panic(PanicPayload),
    Destructor(PanicPayload),
    Cleanup {
        cause: Box<Failure>,
        cleanup: Box<Failure>,
    },
}
impl PartialEq for Failure {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Guest(a), Self::Guest(b)) => Arc::ptr_eq(a, b),
            (Self::Panic(a), Self::Panic(b)) | (Self::Destructor(a), Self::Destructor(b)) => a == b,
            (
                Self::Cleanup {
                    cause: a,
                    cleanup: b,
                },
                Self::Cleanup {
                    cause: c,
                    cleanup: d,
                },
            ) => a == c && b == d,
            _ => false,
        }
    }
}
impl Eq for Failure {}
impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Guest(error) => error.fmt(f),
            Self::Panic(payload) => write!(f, "coroutine panicked: {payload}"),
            Self::Destructor(payload) => write!(f, "coroutine destructor panicked: {payload}"),
            Self::Cleanup { cause, cleanup } => write!(f, "{cause}; cleanup failed: {cleanup}"),
        }
    }
}
impl Error for Failure {}

#[derive(Debug)]
pub enum StartError {
    ZeroLimit,
    Host(cyallo_host::StartError),
    Timer(std::io::Error),
    Thread(std::io::Error),
}
impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroLimit => f.write_str("runtime limits must be nonzero"),
            Self::Host(error) => error.fmt(f),
            Self::Timer(error) => write!(f, "could not create timer thread: {error}"),
            Self::Thread(error) => write!(f, "could not create execution thread: {error}"),
        }
    }
}
impl Error for StartError {}
