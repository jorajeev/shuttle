//! A one-shot channel for sending a single value between asynchronous tasks.

use futures::channel::oneshot;
use shuttle::current::causal::{self, Snapshot};
use shuttle::future;
use std::future::Future;
use std::pin::{pin, Pin};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tracing::trace;

#[derive(Debug)]
struct Shared {
    sender_closed: Mutex<Snapshot>,
    receiver_closed: Mutex<Snapshot>,
}

/// Sends a value to the associated [`Receiver`].
#[derive(Debug)]
pub struct Sender<T> {
    inner: Option<oneshot::Sender<(T, Snapshot)>>,
    shared: Arc<Shared>,
}

/// Receives a value from the associated [`Sender`].
#[derive(Debug)]
pub struct Receiver<T> {
    inner: oneshot::Receiver<(T, Snapshot)>,
    shared: Arc<Shared>,
    closed: bool,
}

pub mod error {
    //! Oneshot error types.
    use std::fmt;

    /// Error returned by the `Future` implementation for `Receiver`.
    #[derive(Debug, Eq, PartialEq, Clone)]
    pub struct RecvError(pub(super) ());

    /// Error returned by the `try_recv` function on `Receiver`.
    #[derive(Debug, Eq, PartialEq, Clone)]
    pub enum TryRecvError {
        /// The send half of the channel has not yet sent a value.
        Empty,

        /// The send half of the channel was dropped without sending a value.
        Closed,
    }

    impl fmt::Display for RecvError {
        fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(fmt, "channel closed")
        }
    }

    impl std::error::Error for RecvError {}

    impl fmt::Display for TryRecvError {
        fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                TryRecvError::Empty => write!(fmt, "channel empty"),
                TryRecvError::Closed => write!(fmt, "channel closed"),
            }
        }
    }

    impl std::error::Error for TryRecvError {}
}
use self::error::{RecvError, TryRecvError};

/// Creates a new one-shot channel for sending a single value.
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = oneshot::channel();
    let shared = Arc::new(Shared {
        sender_closed: Mutex::new(Snapshot::new()),
        receiver_closed: Mutex::new(Snapshot::new()),
    });
    (
        Sender {
            inner: Some(tx),
            shared: Arc::clone(&shared),
        },
        Receiver {
            inner: rx,
            shared,
            closed: false,
        },
    )
}

impl<T> Sender<T> {
    /// Attempts to send a value to the associated receiver.
    pub fn send(self, value: T) -> Result<(), T> {
        trace!("sending message on oneshot {:p}", self.inner.as_ref().unwrap());
        shuttle::thread::yield_now();
        let snapshot = causal::capture();
        self.send_captured(value, snapshot)
    }

    pub(crate) fn send_with_snapshot(self, value: T, snapshot: Snapshot) -> Result<(), T> {
        shuttle::thread::yield_now();
        self.send_captured(value, snapshot)
    }

    fn send_captured(mut self, value: T, snapshot: Snapshot) -> Result<(), T> {
        let sender = self.inner.take().unwrap();
        match sender.send((value, snapshot)) {
            Ok(()) => Ok(()),
            Err((value, _)) => {
                let closed = self.shared.receiver_closed.lock().unwrap().clone();
                causal::join_without_increment(&closed);
                Err(value)
            }
        }
    }

    /// Waits for the associated receiver to close.
    pub async fn closed(&mut self) {
        trace!("sender waiting for oneshot receiver closure {:p}", self);
        self.inner.as_mut().unwrap().cancellation().await;
        future::yield_now().await;
        let closed = self.shared.receiver_closed.lock().unwrap().clone();
        causal::join(&closed);
    }

    /// Returns `true` if the associated receiver has been dropped.
    pub fn is_closed(&self) -> bool {
        self.inner.as_ref().unwrap().is_canceled()
    }

    /// Polls until the associated receiver has closed.
    pub fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        let result = self.inner.as_mut().unwrap().poll_canceled(cx);
        if result.is_ready() {
            let closed = self.shared.receiver_closed.lock().unwrap().clone();
            causal::join(&closed);
        }
        result
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        if self.inner.is_some() {
            if let Some(snapshot) = causal::try_capture() {
                self.shared.sender_closed.lock().unwrap().update(&snapshot);
            }
        }
    }
}

impl<T> Receiver<T> {
    /// Prevents the associated sender from sending a value.
    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        shuttle::thread::yield_now();
        let snapshot = causal::capture();
        self.shared.receiver_closed.lock().unwrap().update(&snapshot);
        self.inner.close();
        self.closed = true;
    }

    /// Attempts to receive a value.
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        shuttle::thread::yield_now();
        match self.inner.try_recv() {
            Ok(Some((value, snapshot))) => {
                causal::join(&snapshot);
                Ok(value)
            }
            Ok(None) => Err(TryRecvError::Empty),
            Err(_) => {
                let closed = self.shared.sender_closed.lock().unwrap().clone();
                causal::join(&closed);
                Err(TryRecvError::Closed)
            }
        }
    }

    /// Blocking receive to call outside of asynchronous contexts.
    pub fn blocking_recv(self) -> Result<T, RecvError> {
        future::block_on(self)
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        if !self.closed {
            if let Some(snapshot) = causal::try_capture() {
                self.shared.receiver_closed.lock().unwrap().update(&snapshot);
            }
        }
    }
}

impl<T> Future for Receiver<T> {
    type Output = Result<T, RecvError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let receiver = pin!(&mut self.inner);
        trace!("polling oneshot receiver {:p}", receiver);
        let result = match receiver.poll(cx) {
            Poll::Ready(Ok((value, snapshot))) => {
                shuttle::thread::yield_now();
                causal::join(&snapshot);
                Poll::Ready(Ok(value))
            }
            Poll::Ready(Err(_)) => {
                shuttle::thread::yield_now();
                let closed = self.shared.sender_closed.lock().unwrap().clone();
                causal::join(&closed);
                Poll::Ready(Err(RecvError(())))
            }
            Poll::Pending => Poll::Pending,
        };
        result
    }
}
