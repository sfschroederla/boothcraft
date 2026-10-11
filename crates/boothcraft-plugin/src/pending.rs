//! A result that may not be ready yet, polled from the UI frame loop.

use crate::error::HostError;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};

pub enum Pending<T> {
    Ready(Option<Result<T, HostError>>),
    Waiting(Receiver<Result<T, HostError>>),
}

impl<T> Pending<T> {
    pub fn ready(value: Result<T, HostError>) -> Pending<T> {
        Pending::Ready(Some(value))
    }

    /// A pending result completed later (possibly from another thread) through the sender.
    pub fn channel() -> (Sender<Result<T, HostError>>, Pending<T>) {
        let (tx, rx) = channel();
        (tx, Pending::Waiting(rx))
    }

    /// `Some` exactly once, when the result arrives.
    pub fn poll(&mut self) -> Option<Result<T, HostError>> {
        match self {
            Pending::Ready(v) => v.take(),
            Pending::Waiting(rx) => match rx.try_recv() {
                Ok(v) => Some(v),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err(HostError::Load("the plugin loader went away".into())))
                }
            },
        }
    }
}
