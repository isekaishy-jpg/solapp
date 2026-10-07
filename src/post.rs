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
// Only idle, empty buffers live here. Active drains own their backing, so a
// callback can nest without holding a cache borrow or sharing a frontier.
pub(crate) struct BatchCache<M> {
    buffers: [Option<VecDeque<Post<M>>>; 2],
}
impl<M> BatchCache<M> {
    const BYTE_LIMIT: usize = 64 * 1024;

    pub(crate) fn new() -> Self {
        Self {
            buffers: [None, None],
        }
    }

    fn take(&mut self, count: usize) -> VecDeque<Post<M>> {
        // An oversized frontier cannot return its backing to this cache. Keep
        // the idle small buffers instead of growing and then discarding one.
        if count
            .checked_mul(std::mem::size_of::<Post<M>>())
            .is_none_or(|bytes| bytes > Self::BYTE_LIMIT)
        {
            return VecDeque::new();
        }
        // Prefer the smallest sufficient buffer, then the largest one to grow.
        let sufficient = self
            .buffers
            .iter()
            .enumerate()
            .filter_map(|(index, buffer)| {
                buffer.as_ref().and_then(|buffer| {
                    (buffer.capacity() >= count).then_some((index, buffer.capacity()))
                })
            })
            .min_by_key(|(_, capacity)| *capacity);
        let selected = sufficient.or_else(|| {
            self.buffers
                .iter()
                .enumerate()
                .filter_map(|(index, buffer)| {
                    buffer.as_ref().map(|buffer| (index, buffer.capacity()))
                })
                .max_by_key(|(_, capacity)| *capacity)
        });
        selected
            .and_then(|(index, _)| self.buffers[index].take())
            .unwrap_or_default()
    }

    fn growth_target(&self, current: usize, required: usize) -> usize {
        // Preserve ordinary geometric growth when it fits alongside idle
        // buffers. Near the combined cap, reserve only the required frontier
        // instead of growing a reusable buffer into an immediately discarded one.
        let idle_capacity: usize = self.buffers.iter().flatten().map(VecDeque::capacity).sum();
        // Post always contains nonzero-sized receipt/recipient metadata.
        let available =
            (Self::BYTE_LIMIT / std::mem::size_of::<Post<M>>()).saturating_sub(idle_capacity);
        let doubled = current.saturating_mul(2);
        if doubled <= available {
            doubled.max(required)
        } else {
            required
        }
    }

    pub(crate) fn recycle(&mut self, batch: Batch<M>) {
        match batch {
            Batch::Empty => (),
            Batch::Many(buffer) => {
                assert!(buffer.is_empty(), "post batch must settle before recycling");
                self.retain(buffer);
            }
            Batch::One(_) => unreachable!("post batch must settle before recycling"),
        }
    }

    fn retain(&mut self, buffer: VecDeque<Post<M>>) {
        debug_assert!(buffer.is_empty());
        if buffer.capacity() == 0 {
            return;
        }
        let Some(slot) = self.buffers.iter().position(Option::is_none) else {
            return;
        };
        let bytes = self
            .buffers
            .iter()
            .flatten()
            .try_fold(buffer.capacity(), |capacity, cached| {
                capacity.checked_add(cached.capacity())
            })
            .and_then(|capacity| capacity.checked_mul(std::mem::size_of::<Post<M>>()));
        if bytes.is_some_and(|bytes| bytes <= Self::BYTE_LIMIT) {
            self.buffers[slot] = Some(buffer);
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buffers = [None, None];
    }

    #[cfg(test)]
    pub(crate) fn retained_capacities(&self) -> [usize; 2] {
        self.buffers
            .each_ref()
            .map(|buffer| buffer.as_ref().map_or(0, VecDeque::capacity))
    }
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
    pub(crate) fn detach(
        &self,
        budget: usize,
        cache: &mut BatchCache<M>,
    ) -> Result<Batch<M>, SAError> {
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
        let mut batch = cache.take(count);
        if batch.capacity() < count {
            #[cfg(test)]
            if self
                .fail_next_batch_reservation
                .swap(false, Ordering::Relaxed)
            {
                cache.retain(batch);
                return Err(SAError::AllocationFailed);
            }
            let reservation = if batch.capacity() == 0 {
                batch.try_reserve(count)
            } else {
                let target = cache.growth_target(batch.capacity(), count);
                batch.try_reserve_exact(target)
            };
            if reservation.is_err() {
                cache.retain(batch);
                return Err(SAError::AllocationFailed);
            }
        }
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

    // Exact baseline detachment path for the ignored performance fixture. It
    // has no cache lookup/recycling; admission and receipts are still current.
    #[cfg(test)]
    pub(crate) fn detach_uncached_for_measurement(
        &self,
        budget: usize,
    ) -> Result<Batch<M>, SAError> {
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

    // The timing fixture keeps the same accepted posts alive across iterations.
    // Restore ownership outside the timed region, without admitting new posts,
    // settling receipts, invoking destructors or changing accepted capacity.
    #[cfg(test)]
    pub(crate) fn restore_for_measurement(&self, mut batch: Batch<M>) -> Batch<M> {
        let mut state = lock(&self.state);
        for post in batch.by_ref() {
            state.queue.push_back(post);
        }
        batch
    }
}
