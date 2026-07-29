use crate::oneshot;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// The operation producer stopped without publishing a result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompletionClosed;

/// One engine-owned asynchronous operation result.
pub struct Completion<T> {
    receiver: oneshot::Receiver<T>,
}

impl<T> Completion<T> {
    pub(crate) fn new(receiver: oneshot::Receiver<T>) -> Self {
        Self { receiver }
    }

    /// Blocks the current thread until the operation completes.
    pub fn wait(self) -> Option<T> {
        self.receiver.recv()
    }

    /// Takes a completed value without blocking.
    pub fn try_take(&mut self) -> Result<Option<T>, CompletionClosed> {
        self.receiver.try_recv().map_err(|()| CompletionClosed)
    }
}

impl<T> Future for Completion<T> {
    type Output = Option<T>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.receiver).poll(context)
    }
}
