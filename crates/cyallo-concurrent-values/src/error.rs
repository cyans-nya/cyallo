use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind { Sender, Receiver, Clock, Factory, Group, Task, Offer }
impl fmt::Display for ResourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(match self {
        Self::Sender => "Sender", Self::Receiver => "Receiver", Self::Clock => "Clock",
        Self::Factory => "channel Factory", Self::Group => "task Group", Self::Task => "Task", Self::Offer => "channel Offer",
    }) }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceError { Released, MoveOnly, Expected(ResourceKind), TaskAlreadyJoined, OfferAlreadySelected }
impl fmt::Display for ResourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { match self {
        Self::Released => f.write_str("capability was released"),
        Self::MoveOnly => f.write_str("capability is move-only"),
        Self::Expected(kind) => write!(f, "expected a live {kind}"),
        Self::TaskAlreadyJoined => f.write_str("task was already joined"),
        Self::OfferAlreadySelected => f.write_str("offer was already selected"),
    } }
}
impl std::error::Error for ResourceError {}
