use std::{any::Any, fmt};

/// Preserve the original Rust panic payload without turning runtime failures
/// into formatted error strings. Formatting happens through Display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanicPayload {
    Static(&'static str),
    Owned(String),
    Opaque,
}
impl From<Box<dyn Any + Send>> for PanicPayload {
    fn from(payload: Box<dyn Any + Send>) -> Self {
        match payload.downcast::<&'static str>() {
            Ok(message) => Self::Static(*message),
            Err(payload) => match payload.downcast::<String>() {
                Ok(message) => Self::Owned(*message),
                Err(_) => Self::Opaque,
            },
        }
    }
}
impl fmt::Display for PanicPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Static(message) => f.write_str(message),
            Self::Owned(message) => f.write_str(message),
            Self::Opaque => f.write_str("non-text panic payload"),
        }
    }
}
