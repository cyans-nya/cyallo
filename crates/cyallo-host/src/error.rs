use cyallo_io::PanicPayload;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError { Stopped, QueueFull, Panicked(PanicPayload) }
impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self { Self::Stopped => f.write_str("host pool stopped"), Self::QueueFull => f.write_str("host work queue limit exceeded"), Self::Panicked(payload) => write!(f, "host operation panicked: {payload}") }
    }
}
impl std::error::Error for HostError {}

#[derive(Debug)]
pub enum StartError { ZeroLimit, Thread(std::io::Error) }
impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self { Self::ZeroLimit => f.write_str("host work limits must be nonzero"), Self::Thread(error) => write!(f, "could not create blocking host thread: {error}") }
    }
}
impl std::error::Error for StartError {}
