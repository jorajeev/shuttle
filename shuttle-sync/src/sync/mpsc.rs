//! Multi-producer, single-consumer FIFO queue communication primitives.

use shuttle_core::internal::{ExecutionState, TaskId, VectorClock, DEFAULT_INLINE_TASKS, switch};
use crate::sync::{ResourceSignature, ResourceType};
use smallvec::SmallVec;
use std::cell::RefCell;
use std::fmt::Debug;
use std::rc::Rc;
use std::result::Result;
pub use std::sync::mpsc::{RecvError, RecvTimeoutError, SendError, TryRecvError, TrySendError};
use std::sync::Arc;
use std::time::Duration;
use tracing::trace;

const MAX_INLINE_MESSAGES: usize = 32;

/// Create an unbounded channel
#[track_caller]
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let channel = Arc::new(Channel::new(None));
    let sender = Sender {
        inner: Arc::clone(&channel),
    };
    let receiver = Receiver {
        inner: Arc::clone(&channel),
    };
    (sender, receiver)
}

/// Create a bounded channel
#[track_caller]
pub fn sync_channel<T>(bound: usize) -> (SyncSender<T>, Receiver<T>) {
    let channel = Arc::new(Channel::new(Some(bound)));
    let sender = SyncSender {
        inner: Arc::clone(&channel),
    };
    let receiver = Receiver {
        inner: Arc::clone(&channel),
    };
    (sender, receiver)
}

#[derive(Debug)]
struct Channel<T> {
    bound: Option<usize>,
    state: Rc<RefCell<ChannelState<T>>>,
    #[allow(unused)]
    signature: ResourceSignature,
}

struct TimestampedValue<T> {
    value: T,
    clock: VectorClock,
}

impl<T> TimestampedValue<T> {
    fn new(value: T, clock: VectorClock) -> Self {
        Self { value, clock }
    }
}

struct ChannelState<T> {
    messages: SmallVec<[TimestampedValue<T>; MAX_INLINE_MESSAGES]>,
    receiver_clock: Option<SmallVec<[VectorClock; MAX_INLINE_MESSAGES]>>,
    known_senders: usize,
    known_receivers: usize,
    waiting_senders: SmallVec<[TaskId; DEFAULT_INLINE_TASKS]>,
    waiting_receivers: SmallVec<[TaskId; DEFAULT_INLINE_TASKS]>,
}

impl<T> Debug for ChannelState<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Channel {{ ")?;
        write!(f, "num_messages: {} ", self.messages.len())?;
        write!(
            f,
            "known_senders {} known_receivers {} ",
            self.known_senders, self.known_receivers
        )?;
        write!(f, "waiting_senders: [{:?}] ", self.waiting_senders)?;
        write!(f, "waiting_receivers: [{:?}] ", self.waiting_receivers)?;
        write!(f, "}}")
    }
}

impl<T> Channel<T> {
    #[track_caller]
    fn new(bound: Option<usize>) -> Self {
        let receiver_clock = if let Some(bound) = bound {
            let mut s = SmallVec::with_capacity(bound);
            for _ in 0..bound {
                s.push(VectorClock::new());
            }
            Some(s)
        } else {
            None
        };
        Self {
            bound,
            state: Rc::new(RefCell::new(ChannelState {
                messages: SmallVec::new(),
                receiver_clock,
                known_senders: 1,
                known_receivers: 1,
                waiting_senders: SmallVec::new(),
                waiting_receivers: SmallVec::new(),
            })),
            signature: ExecutionState::new_resource_signature(ResourceType::MpscChannel),
        }
    }

    fn try_send(&self, message: T) -> Result<(), TrySendError<T>> {
        self.send_internal(message, false)
    }

    fn send(&self, message: T) -> Result<(), SendError<T>> {
        self.send_internal(message, true).map_err(|e| match e {
            TrySendError::Full(_) => unreachable!(),
            TrySendError::Disconnected(m) => SendError(m),
        })
    }

    fn is_rendezvous(&self) -> bool {
        self.bound == Some(0)
    }

    fn sender_must_block(&self, state: &ChannelState<T>) -> bool {
        let (is_rendezvous, is_full) = if let Some(bound) = self.bound {
            (bound == 0, state.messages.len() >= std::cmp::max(bound, 1))
        } else {
            (false, false)
        };

        is_full || !state.waiting_senders.is_empty() || (is_rendezvous && state.waiting_receivers.is_empty())
    }

    fn send_internal(&self, message: T, can_block: bool) -> Result<(), TrySendError<T>> {
        switch();

        let me = ExecutionState::me();
        let mut state = self.state.borrow_mut();
        let should_block = self.sender_must_block(&state);

        trace!(
            state = ?state,
            "sender {:?} starting send on channel {:p}",
            me,
            self,
        );
        if state.known_receivers == 0 {
            return Err(TrySendError::Disconnected(message));
        }

        if should_block {
            if !can_block {
                return Err(TrySendError::Full(message));
            }

            state.waiting_senders.push(me);
            trace!(
                state = ?state,
                "blocking sender {:?} on channel {:p}",
                me,
                self,
            );
            ExecutionState::with(|s| s.current_mut().block(false));
            drop(state);

            switch();

            state = self.state.borrow_mut();
            trace!(
                state = ?state,
                "unblocked sender {:?} on channel {:p}",
                me,
                self,
            );

            if state.known_receivers == 0 {
                state.waiting_senders.retain(|t| *t != me);
                return Err(TrySendError::Disconnected(message));
            }

            let head = state.waiting_senders.remove(0);
            assert_eq!(head, me);
        }

        ExecutionState::with(|s| {
            let clock = s.increment_clock();
            state.messages.push(TimestampedValue::new(message, clock.clone()));
        });

        if let Some(&tid) = state.waiting_receivers.first() {
            ExecutionState::with(|s| {
                s.get_mut(tid).unblock();

                if self.is_rendezvous() {
                    let recv_clock = s.get_clock(tid).clone();
                    s.update_clock(&recv_clock);
                }
            });
        }
        if let Some(&tid) = state.waiting_senders.first() {
            let bound = self.bound.expect("can't have waiting senders on an unbounded channel");
            if state.messages.len() < bound {
                ExecutionState::with(|s| s.get_mut(tid).unblock());
            }
        }

        if !self.is_rendezvous() {
            if let Some(receiver_clock) = &mut state.receiver_clock {
                let recv_clock = receiver_clock.remove(0);
                ExecutionState::with(|s| s.update_clock(&recv_clock));
            }
        }

        Ok(())
    }

    fn recv(&self) -> Result<T, RecvError> {
        self.recv_internal(true).map_err(|e| match e {
            TryRecvError::Disconnected => RecvError,
            TryRecvError::Empty => unreachable!(),
        })
    }

    fn try_recv(&self) -> Result<T, TryRecvError> {
        self.recv_internal(false)
    }

    fn receiver_must_block(&self, state: &ChannelState<T>) -> bool {
        state.messages.is_empty() || !state.waiting_receivers.is_empty()
    }

    fn recv_internal(&self, can_block: bool) -> Result<T, TryRecvError> {
        switch();

        let me = ExecutionState::me();
        let mut state = self.state.borrow_mut();
        let should_block = self.receiver_must_block(&state);

        trace!(
            state = ?state,
            "starting recv on channel {:p}",
            self,
        );
        if state.messages.is_empty() && state.known_senders == 0 {
            return Err(TryRecvError::Disconnected);
        }

        if self.is_rendezvous() && state.messages.is_empty() {
            if let Some(&tid) = state.waiting_senders.first() {
                ExecutionState::with(|s| s.get_mut(tid).unblock());
            } else if !can_block {
                return Err(TryRecvError::Empty);
            }
        }

        if !self.is_rendezvous() && !can_block && state.waiting_receivers.len() >= state.messages.len() {
            return Err(TryRecvError::Empty);
        }

        ExecutionState::with(|s| {
            let _ = s.increment_clock();
        });

        if should_block {
            state.waiting_receivers.push(me);
            trace!(
                state = ?state,
                "blocking receiver {:?} on channel {:p}",
                me,
                self,
            );
            ExecutionState::with(|s| s.current_mut().block(false));
            drop(state);

            switch();

            state = self.state.borrow_mut();
            trace!(
                state = ?state,
                "unblocked receiver {:?} on channel {:p}",
                me,
                self,
            );

            if state.messages.is_empty() && state.known_senders == 0 {
                state.waiting_receivers.retain(|t| *t != me);
                return Err(TryRecvError::Disconnected);
            }

            let head = state.waiting_receivers.remove(0);
            assert_eq!(head, me);
        }

        let item = state.messages.remove(0);
        if let Some(&tid) = state.waiting_senders.first() {
            let bound = self.bound.expect("can't have waiting senders on an unbounded channel");
            if bound > 0 || !state.waiting_receivers.is_empty() {
                ExecutionState::with(|s| s.get_mut(tid).unblock());
            }
        }
        if let Some(&tid) = state.waiting_receivers.first() {
            if !state.messages.is_empty() {
                ExecutionState::with(|s| s.get_mut(tid).unblock());
            }
        }

        let TimestampedValue { value, clock } = item;
        ExecutionState::with(|s| {
            s.get_clock_mut(me).update(&clock);

            if let Some(receiver_clock) = &mut state.receiver_clock {
                let bound = self.bound.expect("unexpected internal error");
                if bound > 0 {
                    assert!(receiver_clock.len() < bound);
                    receiver_clock.push(s.get_clock(me).clone());
                }
            }
        });
        Ok(value)
    }
}

unsafe impl<T: Send> Send for Channel<T> {}
unsafe impl<T: Send> Sync for Channel<T> {}

/// The receiving half of Rust's [`channel`] (or [`sync_channel`]) type.
#[derive(Debug)]
pub struct Receiver<T> {
    inner: Arc<Channel<T>>,
}

impl<T> Receiver<T> {
    /// Attempts to wait for a value on this receiver, returning an error if the
    /// corresponding channel has hung up.
    pub fn recv(&self) -> Result<T, RecvError> {
        self.inner.recv()
    }

    /// Attempts to wait for a value on this receiver, returning an error if the
    /// corresponding channel has hung up.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.inner.try_recv()
    }

    /// Attempts to wait for a value on this receiver, returning an error if the
    /// corresponding channel has hung up, or if it waits more than timeout.
    pub fn recv_timeout(&self, _timeout: Duration) -> Result<T, RecvTimeoutError> {
        self.inner.recv().map_err(|_| RecvTimeoutError::Disconnected)
    }

    /// Returns an iterator that will block waiting for messages, but never
    /// [`panic!`]. It will return [`None`] when the channel has hung up.
    pub fn iter(&self) -> Iter<'_, T> {
        Iter { rx: self }
    }

    /// Returns an iterator that will attempt to yield all pending values.
    pub fn try_iter(&self) -> TryIter<'_, T> {
        TryIter { rx: self }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        if ExecutionState::should_stop() {
            return;
        }
        let mut state = self.inner.state.borrow_mut();
        assert!(state.known_receivers > 0);
        state.known_receivers -= 1;
        if state.known_receivers == 0 {
            for &tid in state.waiting_senders.iter() {
                ExecutionState::with(|s| s.get_mut(tid).unblock());
            }
        }
    }
}

/// An iterator over messages on a [`Receiver`], created by [`Receiver::iter`].
#[derive(Debug)]
pub struct Iter<'a, T: 'a> {
    rx: &'a Receiver<T>,
}

/// An iterator that attempts to yield all pending values for a [`Receiver`],
/// created by [`Receiver::try_iter`].
#[derive(Debug)]
pub struct TryIter<'a, T: 'a> {
    rx: &'a Receiver<T>,
}

/// An owning iterator over messages on a [`Receiver`],
/// created by [`Receiver::into_iter`].
#[derive(Debug)]
pub struct IntoIter<T> {
    rx: Receiver<T>,
}

impl<T> Iterator for Iter<'_, T> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        self.rx.recv().ok()
    }
}

impl<T> Iterator for TryIter<'_, T> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        self.rx.try_recv().ok()
    }
}

impl<'a, T> IntoIterator for &'a Receiver<T> {
    type Item = T;
    type IntoIter = Iter<'a, T>;
    fn into_iter(self) -> Iter<'a, T> {
        self.iter()
    }
}

impl<T> Iterator for IntoIter<T> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        self.rx.recv().ok()
    }
}

impl<T> IntoIterator for Receiver<T> {
    type Item = T;
    type IntoIter = IntoIter<T>;
    fn into_iter(self) -> IntoIter<T> {
        IntoIter { rx: self }
    }
}

/// The sending-half of Rust's asynchronous [`channel`] type.
#[derive(Debug)]
pub struct Sender<T> {
    inner: Arc<Channel<T>>,
}

impl<T> Sender<T> {
    /// Attempts to send a value on this channel, returning it back if it could
    /// not be sent.
    pub fn send(&self, t: T) -> Result<(), SendError<T>> {
        self.inner.send(t)
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        let mut state = self.inner.state.borrow_mut();
        state.known_senders += 1;
        drop(state);
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        if ExecutionState::should_stop() {
            return;
        }
        let mut state = self.inner.state.borrow_mut();
        assert!(state.known_senders > 0);
        state.known_senders -= 1;
        if state.known_senders == 0 {
            for &tid in state.waiting_receivers.iter() {
                ExecutionState::with(|s| s.get_mut(tid).unblock());
            }
        }
    }
}

/// The sending-half of Rust's synchronous [`sync_channel`] type.
#[derive(Debug)]
pub struct SyncSender<T> {
    inner: Arc<Channel<T>>,
}

impl<T> SyncSender<T> {
    /// Sends a value on this synchronous channel.
    pub fn send(&self, t: T) -> Result<(), SendError<T>> {
        self.inner.send(t)
    }

    /// Attempts to send a value on this channel without blocking.
    pub fn try_send(&self, t: T) -> Result<(), TrySendError<T>> {
        self.inner.try_send(t)
    }
}

impl<T> Clone for SyncSender<T> {
    fn clone(&self) -> Self {
        let mut state = self.inner.state.borrow_mut();
        state.known_senders += 1;
        drop(state);
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Drop for SyncSender<T> {
    fn drop(&mut self) {
        if ExecutionState::should_stop() {
            return;
        }
        let mut state = self.inner.state.borrow_mut();
        assert!(state.known_senders > 0);
        state.known_senders -= 1;
        if state.known_senders == 0 {
            for &tid in state.waiting_receivers.iter() {
                ExecutionState::with(|s| s.get_mut(tid).unblock());
            }
        }
    }
}
