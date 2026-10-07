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
    Many(BatchBuffer<M>),
}
// A ticket travels with exclusively owned backing; neither can be cloned.
struct BatchTicket(usize);
pub(crate) struct BatchBuffer<M> {
    posts: VecDeque<Post<M>>,
    ticket: Option<BatchTicket>,
}
#[cfg(test)]
impl<M> BatchBuffer<M> {
    pub(crate) fn capacity(&self) -> usize {
        self.posts.capacity()
    }
    pub(crate) fn reserve_empty_for_test(&mut self, capacity: usize) {
        assert!(self.posts.is_empty());
        self.posts.try_reserve_exact(capacity).unwrap();
    }
    pub(crate) fn into_uncached_buffer(self) -> VecDeque<Post<M>> {
        assert!(
            self.ticket.is_none(),
            "complete the checkout before reusing spare backing"
        );
        self.posts
    }
}
#[cfg(test)]
impl<M> Batch<M> {
    pub(crate) fn uncached_for_test(posts: VecDeque<Post<M>>) -> Self {
        Self::Many(BatchBuffer {
            posts,
            ticket: None,
        })
    }
}
// Only empty, idle buffers live here. Outstanding records describe demand,
// independently of allocated slack and of how many buffers are currently idle.
pub(crate) struct BatchCache<M> {
    buffers: [Option<VecDeque<Post<M>>>; 2],
    outstanding: [usize; 2],
    large: usize,
    small: usize,
    #[cfg(test)]
    rebalance_test: RebalanceTest,
}
#[cfg(test)]
#[derive(Default)]
struct RebalanceTest {
    fail_at: Option<usize>,
    overshoot_at: Option<usize>,
    reservations: usize,
}
impl<M> BatchCache<M> {
    const BYTE_LIMIT: usize = 64 * 1024;
    // Post contains nonzero receipt/recipient metadata, even for a ZST message.
    const CAPACITY_LIMIT: usize = Self::BYTE_LIMIT / std::mem::size_of::<Post<M>>();

    pub(crate) fn new() -> Self {
        Self {
            buffers: [None, None],
            outstanding: [0, 0],
            large: 0,
            small: 0,
            #[cfg(test)]
            rebalance_test: RebalanceTest::default(),
        }
    }

    fn capacities_fit(a: usize, b: usize) -> bool {
        a.checked_add(b)
            .and_then(|capacity| capacity.checked_mul(std::mem::size_of::<Post<M>>()))
            .is_some_and(|bytes| bytes <= Self::BYTE_LIMIT)
    }

    fn checkout(
        &mut self,
        count: usize,
        mut fail_reservation: impl FnMut() -> bool,
    ) -> Result<BatchBuffer<M>, SAError> {
        let ticket = self.outstanding.iter().position(|count| *count == 0);
        if count > Self::CAPACITY_LIMIT || ticket.is_none() {
            let mut posts = VecDeque::new();
            if fail_reservation() {
                return Err(SAError::AllocationFailed);
            }
            posts
                .try_reserve(count)
                .map_err(|_| SAError::AllocationFailed)?;
            return Ok(BatchBuffer {
                posts,
                ticket: None,
            });
        }
        let ticket = ticket.unwrap();
        let other = self.outstanding.iter().copied().max().unwrap();
        let mut large = self.large.max(count);
        let mut small = self.small.max(count.min(other));
        if !Self::capacities_fit(large, small) {
            large = count.max(other);
            small = count.min(other);
            if !Self::capacities_fit(large, small) {
                small = 0;
            }
        }
        let selected = self
            .buffers
            .iter()
            .enumerate()
            .filter_map(|(index, buffer)| {
                buffer
                    .as_ref()
                    .filter(|buffer| buffer.capacity() >= count)
                    .map(|buffer| (index, buffer.capacity()))
            })
            .min_by_key(|(_, capacity)| *capacity)
            .or_else(|| {
                self.buffers
                    .iter()
                    .enumerate()
                    .filter_map(|(index, buffer)| {
                        buffer.as_ref().map(|buffer| (index, buffer.capacity()))
                    })
                    .max_by_key(|(_, capacity)| *capacity)
            });
        let mut posts = selected
            .and_then(|(index, _)| self.buffers[index].take())
            .unwrap_or_default();
        if posts.capacity() < count {
            let other_requirement = if count <= small { large } else { small };
            let allowance = Self::CAPACITY_LIMIT.saturating_sub(other_requirement);
            let idle_capacity: usize = self.buffers.iter().flatten().map(VecDeque::capacity).sum();
            let idle_available = Self::CAPACITY_LIMIT.saturating_sub(idle_capacity);
            // Match VecDeque's ordinary minimum geometric capacity across message
            // layouts, including its smaller minimum for very large elements.
            let minimum = if std::mem::size_of::<Post<M>>() <= 1024 {
                4
            } else {
                1
            };
            let geometric = if posts.capacity() == 0 {
                minimum
            } else {
                posts.capacity().saturating_mul(2)
            };
            let target = count.max(geometric.min(allowance).min(idle_available));
            if fail_reservation() || posts.try_reserve_exact(target).is_err() {
                // Failed reservation leaves VecDeque backing intact. Restore its
                // exact original slot; observations and records are still provisional.
                if let Some((index, _)) = selected {
                    self.buffers[index] = Some(posts);
                }
                return Err(SAError::AllocationFailed);
            }
        }
        self.large = large;
        self.small = small;
        self.outstanding[ticket] = count;
        Ok(BatchBuffer {
            posts,
            ticket: Some(BatchTicket(ticket)),
        })
    }

    #[cfg(test)]
    pub(crate) fn recycle(&mut self, batch: Batch<M>) {
        self.complete(batch, true);
    }

    pub(crate) fn complete(&mut self, batch: Batch<M>, retain: bool) {
        let Batch::Many(batch) = batch else {
            assert!(
                matches!(batch, Batch::Empty),
                "post batch must settle before completion"
            );
            return;
        };
        assert!(
            batch.posts.is_empty(),
            "post batch must settle before completion"
        );
        let Some(BatchTicket(ticket)) = batch.ticket else {
            return;
        };
        assert_ne!(
            self.outstanding[ticket], 0,
            "post ticket must complete exactly once"
        );
        self.outstanding[ticket] = 0;
        if retain {
            self.retain(batch.posts);
        }
    }

    fn prepare_replacement(
        &mut self,
        actual: usize,
        target: usize,
    ) -> Result<Option<VecDeque<Post<M>>>, ()> {
        if actual == target {
            return Ok(None);
        }
        let mut replacement = VecDeque::new();
        if target != 0 {
            #[cfg(test)]
            {
                self.rebalance_test.reservations += 1;
                if self.rebalance_test.fail_at == Some(self.rebalance_test.reservations) {
                    return Err(());
                }
            }
            #[cfg(test)]
            let target =
                if self.rebalance_test.overshoot_at == Some(self.rebalance_test.reservations) {
                    Self::CAPACITY_LIMIT + 1
                } else {
                    target
                };
            replacement.try_reserve_exact(target).map_err(|_| ())?;
        }
        Ok(Some(replacement))
    }

    fn retain(&mut self, buffer: VecDeque<Post<M>>) {
        debug_assert!(buffer.is_empty());
        if buffer.capacity() == 0 || !Self::capacities_fit(buffer.capacity(), 0) {
            return;
        }
        let Some(empty) = self.buffers.iter().position(Option::is_none) else {
            return;
        };
        let Some(existing) = self.buffers.iter().position(Option::is_some) else {
            self.buffers[empty] = Some(buffer);
            return;
        };
        let old = self.buffers[existing].as_ref().unwrap().capacity();
        if Self::capacities_fit(buffer.capacity(), old) {
            self.buffers[empty] = Some(buffer);
            return;
        }
        let big = buffer.capacity().max(old);
        let little = buffer.capacity().min(old);
        let target_big = big.min(Self::CAPACITY_LIMIT.saturating_sub(self.small));
        let target_little = little.min(Self::CAPACITY_LIMIT.saturating_sub(target_big));
        let (new_buffer, new_old) = if buffer.capacity() >= old {
            (target_big, target_little)
        } else {
            (target_little, target_big)
        };
        // Prepare all replacements before touching idle backing. Failure drops
        // temporaries and returned storage, and leaves existing idle storage intact.
        let Ok(buffer_replacement) = self.prepare_replacement(buffer.capacity(), new_buffer) else {
            return;
        };
        let Ok(old_replacement) = self.prepare_replacement(old, new_old) else {
            return;
        };
        let buffer_capacity = buffer_replacement
            .as_ref()
            .map_or(buffer.capacity(), VecDeque::capacity);
        let old_capacity = old_replacement.as_ref().map_or(old, VecDeque::capacity);
        if !Self::capacities_fit(buffer_capacity, old_capacity) {
            return;
        }
        let buffer = buffer_replacement.unwrap_or(buffer);
        if let Some(replacement) = old_replacement {
            self.buffers[existing] = (replacement.capacity() != 0).then_some(replacement);
        }
        if buffer.capacity() != 0 {
            self.buffers[empty] = Some(buffer);
        }
    }

    pub(crate) fn clear(&mut self) {
        // An outer drain may still own a ticket when nested work closes the host.
        self.buffers = [None, None];
    }

    #[cfg(test)]
    pub(crate) fn seed_for_test(&mut self, buffer: VecDeque<Post<M>>) {
        assert!(buffer.is_empty());
        let total: usize = self.buffers.iter().flatten().map(VecDeque::capacity).sum();
        if buffer.capacity() != 0
            && Self::capacities_fit(total, buffer.capacity())
            && let Some(slot) = self.buffers.iter().position(Option::is_none)
        {
            self.buffers[slot] = Some(buffer);
        }
    }
    #[cfg(test)]
    pub(crate) fn retained_capacities(&self) -> [usize; 2] {
        self.buffers
            .each_ref()
            .map(|buffer| buffer.as_ref().map_or(0, VecDeque::capacity))
    }
    #[cfg(test)]
    pub(crate) fn state_for_test(&self) -> ([usize; 2], [usize; 2], usize, usize) {
        (
            self.retained_capacities(),
            self.outstanding,
            self.large,
            self.small,
        )
    }
    #[cfg(test)]
    pub(crate) fn set_demand_for_test(&mut self, large: usize, small: usize) {
        assert!(small <= large && Self::capacities_fit(large, small));
        self.large = large;
        self.small = small;
    }
    #[cfg(test)]
    pub(crate) fn configure_rebalance_for_test(
        &mut self,
        fail_at: Option<usize>,
        overshoot_at: Option<usize>,
    ) {
        self.rebalance_test = RebalanceTest {
            fail_at,
            overshoot_at,
            reservations: 0,
        };
    }
    #[cfg(test)]
    pub(crate) fn rebalance_reservations_for_test(&self) -> usize {
        self.rebalance_test.reservations
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
            Self::Many(batch) => batch.posts.pop_front(),
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
        let mut batch = cache.checkout(count, || {
            #[cfg(test)]
            {
                self.fail_next_batch_reservation
                    .swap(false, Ordering::Relaxed)
            }
            #[cfg(not(test))]
            {
                false
            }
        })?;
        let mut state = lock(&self.state);
        for _ in 0..count {
            if let Some(post) = state.queue.pop_front() {
                batch.posts.push_back(post);
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
        Ok(Batch::uncached_for_test(batch))
    }

    // The timing fixture keeps the same accepted posts alive across iterations.
    // Restore ownership outside the timed region, without admitting new posts,
    // settling receipts, invoking destructors or changing accepted capacity.
    #[cfg(test)]
    pub(crate) fn restore_for_measurement(&self, mut batch: Batch<M>) -> Batch<M> {
        if let Batch::Many(buffer) = &batch {
            assert!(
                buffer.ticket.is_none(),
                "complete the real checkout before restoring measurement posts"
            );
        }
        let mut state = lock(&self.state);
        for post in batch.by_ref() {
            state.queue.push_back(post);
        }
        batch
    }
}
