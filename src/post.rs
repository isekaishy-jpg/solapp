//! Bounded owned cross-thread intake. Queue admission and close share one lock.

use std::collections::{HashSet, VecDeque};
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::identity::{SAPostId, SARecipientId};
use crate::{SAError, SAHostId, SAIdentityKind, SARejected};

/// Why an accepted payload was destroyed without recipient routing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SAPostDiscardReason {
    /// Its recipient generation retired after admission.
    RecipientRetired,
    /// Host stop closed ordinary routing.
    HostStopped,
}

/// Monotonic accepted-post status. Delivered means routing returned, not domain
/// publication or completion of work a handler started.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SAPostOutcome {
    /// Accepted ownership is still queued or being routed.
    Pending,
    /// Routing returned normally, even when no subscription matched.
    Delivered {
        /// Number of handlers which returned normally.
        callbacks: usize,
    },
    /// Accepted ownership was reclaimed on the owner thread without routing.
    Discarded(SAPostDiscardReason),
    /// Routing faulted; payload ownership was still reclaimed on the owner.
    Faulted(SAError),
}

/// A transferable observer containing no payload or application reference.
/// Dropping the observer does not cancel accepted ownership.
#[derive(Clone)]
pub struct SAPostReceipt {
    inner: Arc<Receipt>,
}
struct Receipt {
    id: SAPostId,
    outcome: Mutex<SAPostOutcome>,
}
impl SAPostReceipt {
    /// Host-scoped identity of this accepted post.
    pub fn id(&self) -> SAPostId {
        self.inner.id
    }
    /// Observes a durable status; terminal states never change.
    pub fn outcome(&self) -> SAPostOutcome {
        lock(&self.inner.outcome).clone()
    }
    pub(crate) fn settle(&self, outcome: SAPostOutcome) {
        let mut current = lock(&self.inner.outcome);
        if matches!(*current, SAPostOutcome::Pending) {
            *current = outcome;
        }
    }
}
impl std::fmt::Debug for SAPostReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SAPostReceipt")
            .field("id", &self.id())
            .field("outcome", &self.outcome())
            .finish()
    }
}

/// Cross-thread entry point for owned `Send + 'static` messages.
/// Rejection returns the original message on the calling thread. Accepted
/// messages are dropped by the host owner after routing or terminal discard.
pub struct SAProxy<M: Send + 'static> {
    pub(crate) transport: Arc<Transport<M>>,
}
impl<M: Send + 'static> Clone for SAProxy<M> {
    fn clone(&self) -> Self {
        Self {
            transport: Arc::clone(&self.transport),
        }
    }
}
impl<M: Send + 'static> SAProxy<M> {
    /// Atomically admits or rejects a message addressed to an exact recipient
    /// generation. The configured capacity includes detached/active posts.
    /// Standard-library shared-handle allocation can still process-abort on OOM.
    pub fn try_post(
        &self,
        recipient: SARecipientId,
        message: M,
    ) -> Result<SAPostReceipt, SARejected<M>> {
        match self.transport.enqueue(recipient, message) {
            Ok(receipt) => Ok(receipt),
            Err((message, error)) => Err(SARejected::new(message, error)),
        }
    }
}

pub(crate) struct Post<M> {
    pub(crate) recipient: SARecipientId,
    pub(crate) message: M,
    pub(crate) receipt: SAPostReceipt,
}
// Detached ownership is independent of shared transport storage, including the
// common singleton frontier. Iteration never consults the transport queue.
pub(crate) enum Batch<M> {
    Empty,
    One(Post<M>),
    Many(VecDeque<Post<M>>),
}
impl<M> Iterator for Batch<M> {
    type Item = Post<M>;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Empty => None,
            Self::One(_) => match std::mem::replace(self, Self::Empty) {
                Self::One(post) => Some(post),
                _ => unreachable!(),
            },
            Self::Many(posts) => posts.pop_front(),
        }
    }
}
struct State<M> {
    open: bool,
    capacity: usize,
    accepted: usize,
    recipients: HashSet<SARecipientId>,
    queue: VecDeque<Post<M>>,
}
pub(crate) struct Transport<M> {
    host: SAHostId,
    next: AtomicU64,
    state: Mutex<State<M>>,
    wake: crate::backend::wake::NativeWake,
    #[cfg(test)]
    pub(crate) fail_next_batch_reservation: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl<M: Send + 'static> Transport<M> {
    pub(crate) fn new(
        host: SAHostId,
        capacity: usize,
        wake: crate::backend::wake::NativeWake,
    ) -> Result<Self, SAError> {
        let mut queue = VecDeque::new();
        queue
            .try_reserve(capacity)
            .map_err(|_| SAError::AllocationFailed)?;
        Ok(Self {
            host,
            next: AtomicU64::new(1),
            state: Mutex::new(State {
                open: true,
                capacity,
                accepted: 0,
                recipients: HashSet::new(),
                queue,
            }),
            wake,
            #[cfg(test)]
            fail_next_batch_reservation: AtomicBool::new(false),
        })
    }
    pub(crate) fn register(&self, recipient: SARecipientId) -> Result<(), SAError> {
        let mut state = lock(&self.state);
        if !state.open {
            return Err(SAError::AdmissionClosed);
        }
        state
            .recipients
            .try_reserve(1)
            .map_err(|_| SAError::AllocationFailed)?;
        state.recipients.insert(recipient);
        Ok(())
    }
    pub(crate) fn retire(&self, recipient: SARecipientId) {
        lock(&self.state).recipients.remove(&recipient);
    }
    pub(crate) fn accepted(&self) -> usize {
        lock(&self.state).accepted
    }
    pub(crate) fn close(&self) {
        lock(&self.state).open = false;
    }
    fn enqueue(&self, recipient: SARecipientId, message: M) -> Result<SAPostReceipt, (M, SAError)> {
        if recipient.host != self.host {
            return Err((message, SAError::ForeignHost));
        }
        let sequence = match self
            .next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            }) {
            Ok(sequence) => sequence,
            Err(_) => return Err((message, SAError::IdentityExhausted(SAIdentityKind::Post))),
        };
        // Receipt ownership exists before queue acceptance. Counter ordering
        // supplies uniqueness only; mutex release publishes payload and status.
        let receipt = SAPostReceipt {
            inner: Arc::new(Receipt {
                id: SAPostId {
                    host: self.host,
                    sequence,
                },
                outcome: Mutex::new(SAPostOutcome::Pending),
            }),
        };
        let mut state = lock(&self.state);
        let rejection = if !state.open {
            Some(SAError::AdmissionClosed)
        } else if !state.recipients.contains(&recipient) {
            Some(SAError::StaleIdentity)
        } else if state.accepted >= state.capacity {
            Some(SAError::CapacityFull)
        } else {
            None
        };
        if let Some(error) = rejection {
            drop(state);
            return Err((message, error));
        }
        state.queue.push_back(Post {
            recipient,
            message,
            receipt: receipt.clone(),
        });
        state.accepted += 1;
        drop(state);
        // Upstream's wake is a hint; the host also uses a finite recheck.
        let _ = self.wake.post();
        Ok(receipt)
    }
    pub(crate) fn len(&self) -> usize {
        lock(&self.state).queue.len()
    }
    pub(crate) fn pop(&self) -> Option<Post<M>> {
        lock(&self.state).queue.pop_front()
    }
    pub(crate) fn detach(&self, budget: usize) -> Result<Batch<M>, SAError> {
        let count = self.len().min(budget);
        if count == 0 {
            return Ok(Batch::Empty);
        }
        if count == 1 {
            return Ok(match lock(&self.state).queue.pop_front() {
                Some(post) => Batch::One(post),
                None => Batch::Empty,
            });
        }
        #[cfg(test)]
        if self
            .fail_next_batch_reservation
            .swap(false, Ordering::Relaxed)
        {
            return Err(SAError::AllocationFailed);
        }
        let mut batch = VecDeque::new();
        batch
            .try_reserve(count)
            .map_err(|_| SAError::AllocationFailed)?;
        let mut state = lock(&self.state);
        for _ in 0..count {
            if let Some(post) = state.queue.pop_front() {
                batch.push_back(post);
            }
        }
        Ok(Batch::Many(batch))
    }
    pub(crate) fn settled(&self) {
        let mut state = lock(&self.state);
        state.accepted -= 1;
    }
}
