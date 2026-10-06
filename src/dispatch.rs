//! Mutation-aware owner-local dispatch with live gap markers per nested call.

use crate::host::SAApplication;
use crate::identity::{Arena, SARecipientId, SASubscriptionId, SlotKey};
use crate::{SAContext, SAError, SAHostId, SAIdentityKind};

/// Finite dispatch priority. Higher values run first; newest equal runs first.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SAPriority(f64);
impl SAPriority {
    /// Validates finite priority and normalizes signed zero as equal.
    pub fn new(value: f64) -> Result<Self, SAError> {
        if !value.is_finite() {
            return Err(SAError::InvalidInput("priority must be finite"));
        }
        Ok(Self(if value == 0.0 { 0.0 } else { value }))
    }
}
impl Default for SAPriority {
    fn default() -> Self {
        Self(0.0)
    }
}

/// Whether routing continues to the next eligible subscriber.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SAPropagation {
    /// Continue this dispatch's traversal.
    Continue,
    /// Stop this dispatch only; enclosing nested dispatch remains independent.
    Stop,
}

/// The family a subscription receives. Local dispatch is owner-only.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SAEventFilter {
    /// All implemented event families.
    All,
    /// Borrowed application-local events.
    Local,
    /// Owned transferable posts addressed to this recipient.
    Posted,
    /// Claimed one-shot timers addressed to this recipient.
    Timer,
    /// Normalized native input records.
    Input,
}

/// A borrowed view of an event; retaining its payload requires explicit copying.
pub enum SAEvent<'event, M, L> {
    /// An owner-local event borrowed for the dispatch call.
    Local(&'event L),
    /// A transferable post; routing completion settles its receipt.
    Posted {
        /// The accepted post's durable receipt.
        receipt: &'event crate::post::SAPostReceipt,
        /// The owned payload, borrowed only during routing.
        message: &'event M,
    },
    /// A claimed timer whose payload remains alive through all handlers.
    Timer {
        /// The timer token; cancelling it now reports Claimed.
        id: crate::identity::SATimerId,
        /// Its original raw deadline.
        deadline: crate::time::SARawDeadline,
        /// One sample shared by every claim in this explicit pump.
        sample: crate::time::SARawTime,
        /// The owner-local payload borrowed for callback routing.
        event: &'event L,
    },
    /// A normalized, owned input record borrowed during delivery.
    Input(&'event crate::input::SAInputRecord),
}

impl<M, L> SAEvent<'_, M, L> {
    pub(crate) fn filter(&self) -> SAEventFilter {
        match self {
            Self::Local(_) => SAEventFilter::Local,
            Self::Posted { .. } => SAEventFilter::Posted,
            Self::Timer { .. } => SAEventFilter::Timer,
            Self::Input(_) => SAEventFilter::Input,
        }
    }
}

/// Function-pointer callback; mutable state lives in the separately borrowed app.
pub type SAHandler<A> = for<'cx, 'event> fn(
    &mut A,
    &mut SAContext<'cx, A>,
    &SAEvent<'event, <A as SAApplication>::Message, <A as SAApplication>::LocalEvent>,
) -> SAPropagation;

/// Result of a completed dispatch; it does not mean domain publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SADispatchReport {
    /// Final propagation decision.
    pub propagation: SAPropagation,
    /// Number of callbacks that returned normally.
    pub callbacks: usize,
}

/// Observed recipient retirement state, including active callback retention.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SARecipientState {
    /// Whether future calls and posts to this generation have been disabled.
    pub retired: bool,
    /// Active nested callbacks that can still refer to application state.
    pub active_callbacks: usize,
}

pub(crate) struct Recipient {
    pub(crate) alive: bool,
    pub(crate) active: usize,
}
pub(crate) struct Subscription<A: SAApplication> {
    pub(crate) recipient: SARecipientId,
    filter: SAEventFilter,
    handler: SAHandler<A>,
    alive: bool,
    active: usize,
}

#[derive(Clone, Copy)]
pub(crate) struct Rank {
    priority: f64,
    sequence: u64,
}
impl Rank {
    fn precedes(self, other: Self) -> bool {
        self.priority > other.priority
            || (self.priority == other.priority && self.sequence > other.sequence)
    }
}

pub(crate) struct Dispatch<A: SAApplication> {
    pub(crate) recipients: Arena<Recipient>,
    subscriptions: Arena<Subscription<A>>,
    order: Vec<(Rank, SlotKey)>,
    markers: Vec<usize>,
    #[cfg(test)]
    pub(crate) fail_next_marker_reservation: bool,
    #[cfg(test)]
    pub(crate) fail_next_order_reservation: bool,
    next_sequence: u64,
    pub(crate) depth: usize,
}

impl<A: SAApplication> Dispatch<A> {
    pub(crate) fn new() -> Self {
        Self {
            recipients: Arena::new(SAIdentityKind::Recipient),
            subscriptions: Arena::new(SAIdentityKind::Subscription),
            order: Vec::new(),
            markers: Vec::new(),
            #[cfg(test)]
            fail_next_marker_reservation: false,
            #[cfg(test)]
            fail_next_order_reservation: false,
            next_sequence: 1,
            depth: 0,
        }
    }
    pub(crate) fn recipient(
        &self,
        host: SAHostId,
        id: SARecipientId,
    ) -> Result<&Recipient, SAError> {
        if id.host != host {
            return Err(SAError::ForeignHost);
        }
        self.recipients.get(id.key).ok_or(SAError::StaleIdentity)
    }
    pub(crate) fn live_recipient(&self, host: SAHostId, id: SARecipientId) -> Result<(), SAError> {
        if !self.recipient(host, id)?.alive {
            return Err(SAError::StaleIdentity);
        }
        Ok(())
    }
    pub(crate) fn subscribe(
        &mut self,
        host: SAHostId,
        recipient: SARecipientId,
        filter: SAEventFilter,
        priority: SAPriority,
        handler: SAHandler<A>,
    ) -> Result<SASubscriptionId, SAError> {
        self.live_recipient(host, recipient)?;
        let sequence = self.next_sequence;
        self.next_sequence = sequence
            .checked_add(1)
            .ok_or(SAError::IdentityExhausted(SAIdentityKind::Subscription))?;
        let key = self.subscriptions.reserve()?;
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_order_reservation) {
            return Err(SAError::AllocationFailed);
        }
        self.order
            .try_reserve(1)
            .map_err(|_| SAError::AllocationFailed)?;
        let rank = Rank {
            priority: priority.0,
            sequence,
        };
        let index = self
            .order
            .partition_point(|(current, _)| current.precedes(rank));
        self.order.insert(index, (rank, key));
        // Native registration skips markers and inserts immediately before the
        // first ordinary record. An insertion at the same gap is AFTER markers.
        for marker in &mut self.markers {
            if index < *marker {
                *marker += 1;
            }
        }
        self.subscriptions.insert(
            key,
            Subscription {
                recipient,
                filter,
                handler,
                alive: true,
                active: 0,
            },
        );
        Ok(SASubscriptionId { host, key })
    }
    pub(crate) fn unsubscribe(
        &mut self,
        host: SAHostId,
        id: SASubscriptionId,
    ) -> Result<(), SAError> {
        if id.host != host {
            return Err(SAError::ForeignHost);
        }
        let record = self
            .subscriptions
            .get_mut(id.key)
            .ok_or(SAError::StaleIdentity)?;
        if !record.alive {
            return Err(SAError::StaleIdentity);
        }
        record.alive = false;
        if let Some(index) = self.order.iter().position(|(_, key)| *key == id.key) {
            self.order.remove(index);
            for marker in &mut self.markers {
                if index < *marker {
                    *marker -= 1;
                }
            }
        }
        if record.active == 0 {
            self.subscriptions.remove(id.key);
        }
        Ok(())
    }
    pub(crate) fn retire_recipient(
        &mut self,
        host: SAHostId,
        id: SARecipientId,
    ) -> Result<SARecipientState, SAError> {
        self.live_recipient(host, id)?;
        let recipient = self
            .recipients
            .get_mut(id.key)
            .ok_or(SAError::StaleIdentity)?;
        recipient.alive = false;
        let active_callbacks = recipient.active;
        // No callback or user destructor runs while traversing this registry.
        loop {
            let key = self
                .subscriptions
                .iter()
                .find(|(_, record)| record.recipient == id && record.alive)
                .map(|(key, _)| key);
            let Some(key) = key else {
                break;
            };
            self.unsubscribe(host, SASubscriptionId { host, key })?;
        }
        if active_callbacks == 0 {
            self.recipients.remove(id.key);
        }
        Ok(SARecipientState {
            retired: true,
            active_callbacks,
        })
    }
    pub(crate) fn prepare_marker(&mut self) -> Result<(), SAError> {
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_marker_reservation) {
            return Err(SAError::AllocationFailed);
        }
        self.markers
            .try_reserve(1)
            .map_err(|_| SAError::AllocationFailed)?;
        Ok(())
    }
    pub(crate) fn begin(&mut self) -> Result<usize, SAError> {
        self.prepare_marker()?;
        let index = self.markers.len();
        self.markers.push(0);
        Ok(index)
    }
    pub(crate) fn end(&mut self, marker: usize) {
        debug_assert_eq!(marker + 1, self.markers.len());
        self.markers.pop();
    }
    pub(crate) fn next(
        &mut self,
        marker: usize,
        target: Option<SARecipientId>,
        filter: SAEventFilter,
    ) -> Option<SlotKey> {
        let start = self.markers[marker];
        let next = self.order[start..]
            .iter()
            .enumerate()
            .find_map(|(offset, (_, key))| {
                let record = self.subscriptions.get(*key)?;
                let eligible = record.alive
                    && (record.filter == SAEventFilter::All || record.filter == filter)
                    && target.is_none_or(|target| record.recipient == target)
                    && self
                        .recipients
                        .get(record.recipient.key)
                        .is_some_and(|recipient| recipient.alive);
                eligible.then_some((start + offset, *key))
            });
        if let Some((index, key)) = next {
            self.markers[marker] = index + 1;
            Some(key)
        } else {
            self.markers[marker] = self.order.len();
            None
        }
    }
    pub(crate) fn claim(&mut self, key: SlotKey) -> (SAHandler<A>, SARecipientId) {
        let record = self
            .subscriptions
            .get_mut(key)
            .expect("selected subscription retained until callback claim");
        record.active += 1;
        let recipient = record.recipient;
        let handler = record.handler;
        self.recipients
            .get_mut(recipient.key)
            .expect("selected recipient is retained")
            .active += 1;
        (handler, recipient)
    }
    pub(crate) fn release(&mut self, key: SlotKey, recipient: SARecipientId) {
        let record = self
            .subscriptions
            .get_mut(key)
            .expect("active subscription cannot be reclaimed");
        record.active -= 1;
        if !record.alive && record.active == 0 {
            self.subscriptions.remove(key);
        }
        let record = self
            .recipients
            .get_mut(recipient.key)
            .expect("active recipient cannot be reclaimed");
        record.active -= 1;
        if !record.alive && record.active == 0 {
            self.recipients.remove(recipient.key);
        }
    }
}
