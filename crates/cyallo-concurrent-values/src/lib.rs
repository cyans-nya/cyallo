//! Heap-independent payloads and logical capability ownership for guest Cs.
//! A payload never contains a Session-relative cell index or borrowed VM pointer.
mod error;
pub use cyallo_channel::ReceiveError;
use cyallo_channel::{Factory, Offer, Receiver, Sender};
pub use cyallo_channel::{SendError, SendFailure};
pub use cyallo_scheduler::Outcome as TaskOutcome;
use cyallo_scheduler::{Clock, Group, Task};
pub use error::{ResourceError, ResourceKind};
use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, Mutex},
};

#[derive(Debug)]
pub enum Payload {
    Unit,
    Number(cyallo_ast::Number),
    Boolean(bool),
    Char(char),
    String(Arc<str>),
    Array(Vec<Payload>),
    Sparse(BTreeMap<u32, Payload>, u64),
    Function(usize, Vec<Payload>),
    Optional(Option<Box<Payload>>),
    Result(Result<Box<Payload>, Box<Payload>>),
    Erased(cyallo_ast::Type, Box<Payload>),
    Resource(OwnedHandle),
}

/// Logical ownership, distinct from incidental shallow copies of VM Values.
/// Moving a payload transfers this lease; discarding it releases the capability.
#[derive(Debug)]
pub struct OwnedHandle(Option<Handle>);
impl OwnedHandle {
    pub fn new(handle: Handle) -> Self {
        Self(Some(handle))
    }
    pub fn into_handle(mut self) -> Handle {
        self.0.take().unwrap()
    }
}
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.release();
        }
    }
}

pub enum Resource {
    Sender(Sender<Payload>),
    Receiver(Receiver<Payload>),
    Task(Option<Task<Payload>>),
    Group(Arc<Group>),
    Clock(Clock),
    Factory(Factory),
    Offer(Option<Offer<Payload>>),
}

/// Clone is a storage alias, not an extra logical owner. Language clone creates
/// an independent endpoint via duplicate; explicit guest drop calls release.
#[derive(Clone)]
pub struct Handle(Arc<Mutex<Option<Resource>>>);
impl PartialEq for Handle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for Handle {}
impl fmt::Debug for Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<capability {:p}>", Arc::as_ptr(&self.0))
    }
}
impl Handle {
    pub fn new(resource: Resource) -> Self {
        Self(Arc::new(Mutex::new(Some(resource))))
    }
    pub fn release(&self) {
        let resource = self.0.lock().unwrap().take();
        // Endpoint drop can wake another C; never run it under the handle lock.
        drop(resource);
    }
    pub fn duplicate(&self) -> Result<Self, ResourceError> {
        let resource = self.0.lock().unwrap();
        Ok(Self::new(
            match resource.as_ref().ok_or(ResourceError::Released)? {
                Resource::Sender(sender) => Resource::Sender(sender.clone()),
                Resource::Receiver(receiver) => Resource::Receiver(receiver.clone()),
                Resource::Group(group) => Resource::Group(group.clone()),
                Resource::Clock(clock) => Resource::Clock(clock.clone()),
                Resource::Factory(factory) => Resource::Factory(factory.clone()),
                Resource::Task(_) | Resource::Offer(_) => return Err(ResourceError::MoveOnly),
            },
        ))
    }
    pub fn sender(&self) -> Result<Sender<Payload>, ResourceError> {
        match self.0.lock().unwrap().as_ref() {
            Some(Resource::Sender(sender)) => Ok(sender.clone()),
            _ => Err(ResourceError::Expected(ResourceKind::Sender)),
        }
    }
    pub fn receiver(&self) -> Result<Receiver<Payload>, ResourceError> {
        match self.0.lock().unwrap().as_ref() {
            Some(Resource::Receiver(receiver)) => Ok(receiver.clone()),
            _ => Err(ResourceError::Expected(ResourceKind::Receiver)),
        }
    }
    pub fn clock(&self) -> Result<Clock, ResourceError> {
        match self.0.lock().unwrap().as_ref() {
            Some(Resource::Clock(clock)) => Ok(clock.clone()),
            _ => Err(ResourceError::Expected(ResourceKind::Clock)),
        }
    }
    pub fn factory(&self) -> Result<Factory, ResourceError> {
        match self.0.lock().unwrap().as_ref() {
            Some(Resource::Factory(factory)) => Ok(factory.clone()),
            _ => Err(ResourceError::Expected(ResourceKind::Factory)),
        }
    }
    pub fn group(&self) -> Result<Arc<Group>, ResourceError> {
        match self.0.lock().unwrap().as_ref() {
            Some(Resource::Group(group)) => Ok(group.clone()),
            _ => Err(ResourceError::Expected(ResourceKind::Group)),
        }
    }
    pub fn take_task(&self) -> Result<Task<Payload>, ResourceError> {
        match self.0.lock().unwrap().as_mut() {
            Some(Resource::Task(task)) => task.take().ok_or(ResourceError::TaskAlreadyJoined),
            _ => Err(ResourceError::Expected(ResourceKind::Task)),
        }
    }
    pub fn cancel_task(&self) -> Result<(), ResourceError> {
        match self.0.lock().unwrap().as_ref() {
            Some(Resource::Task(Some(task))) => {
                task.cancel();
                Ok(())
            }
            _ => Err(ResourceError::Expected(ResourceKind::Task)),
        }
    }
    pub fn task_finished(&self) -> Result<bool, ResourceError> {
        match self.0.lock().unwrap().as_ref() {
            Some(Resource::Task(Some(task))) => Ok(task.is_finished()),
            _ => Err(ResourceError::Expected(ResourceKind::Task)),
        }
    }
    pub fn take_offer(&self) -> Result<Offer<Payload>, ResourceError> {
        match self.0.lock().unwrap().as_mut() {
            Some(Resource::Offer(offer)) => offer.take().ok_or(ResourceError::OfferAlreadySelected),
            _ => Err(ResourceError::Expected(ResourceKind::Offer)),
        }
    }
}
