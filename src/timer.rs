//! Indexed removable deadline heap; claimed payloads leave the heap first.

use crate::identity::{Arena, SARecipientId, SATimerId, SlotKey};
use crate::time::{SARawDeadline, SARawTime};
use crate::{SAError, SAHostId, SAIdentityKind};

/// Cancelling cannot revoke an already claimed callback.
pub enum SATimerCancel<L> {
    /// The unclaimed timer was removed; ownership returns to the caller.
    Removed(L),
    /// Its callback was claimed and can still be active.
    Claimed,
    /// This incarnation is no longer a current timer.
    Stale,
}

/// One explicit timer pump's observation. No tie-order promise is made.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SATimerPumpReport {
    /// One raw sample shared by claims in this pump.
    pub sample: SARawTime,
    /// Number of timer records claimed, including obsolete-recipient discards.
    pub claimed: usize,
    /// Due timers remain at this sample after the supplied budget was used.
    pub due_remaining: bool,
}

struct Timer<L> {
    recipient: SARecipientId,
    deadline: SARawDeadline,
    payload: Option<L>,
    heap_index: Option<usize>,
}
pub(crate) struct Claimed<L> {
    pub(crate) id: SATimerId,
    pub(crate) recipient: SARecipientId,
    pub(crate) deadline: SARawDeadline,
    pub(crate) payload: L,
}
pub(crate) struct Timers<L> {
    host: SAHostId,
    records: Arena<Timer<L>>,
    heap: Vec<SlotKey>,
    claimed: usize,
    pub(crate) depth: usize,
    #[cfg(test)]
    pub(crate) fail_next_heap_reservation: bool,
}

impl<L> Timers<L> {
    pub(crate) fn new(host: SAHostId) -> Self {
        Self {
            host,
            records: Arena::new(SAIdentityKind::Timer),
            heap: Vec::new(),
            claimed: 0,
            depth: 0,
            #[cfg(test)]
            fail_next_heap_reservation: false,
        }
    }
    pub(crate) fn schedule(
        &mut self,
        recipient: SARecipientId,
        deadline: SARawDeadline,
        payload: L,
    ) -> Result<SATimerId, (L, SAError)> {
        if deadline.0.host != self.host {
            return Err((payload, SAError::ForeignHost));
        }
        let key = match self.records.reserve() {
            Ok(key) => key,
            Err(error) => return Err((payload, error)),
        };
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_heap_reservation) {
            return Err((payload, SAError::AllocationFailed));
        }
        if self.heap.try_reserve(1).is_err() {
            return Err((payload, SAError::AllocationFailed));
        }
        let index = self.heap.len();
        self.records.insert(
            key,
            Timer {
                recipient,
                deadline,
                payload: Some(payload),
                heap_index: Some(index),
            },
        );
        self.heap.push(key);
        self.up(index);
        Ok(SATimerId {
            host: self.host,
            key,
        })
    }
    pub(crate) fn cancel(&mut self, id: SATimerId) -> Result<SATimerCancel<L>, SAError> {
        if id.host != self.host {
            return Err(SAError::ForeignHost);
        }
        let Some(record) = self.records.get(id.key) else {
            return Ok(SATimerCancel::Stale);
        };
        let Some(index) = record.heap_index else {
            return Ok(SATimerCancel::Claimed);
        };
        self.remove_heap(index);
        let record = self
            .records
            .remove(id.key)
            .expect("unclaimed timer retains its record");
        Ok(SATimerCancel::Removed(
            record.payload.expect("unclaimed timer owns payload"),
        ))
    }
    pub(crate) fn counts(&self) -> (usize, usize) {
        (self.heap.len(), self.claimed)
    }
    #[cfg(test)]
    pub(crate) fn scanned_counts(&self) -> (usize, usize) {
        self.records
            .iter()
            .fold((0, 0), |(pending, claimed), (_, record)| {
                if record.heap_index.is_some() {
                    (pending + 1, claimed)
                } else {
                    (pending, claimed + 1)
                }
            })
    }
    pub(crate) fn next_deadline(&self) -> Option<SARawDeadline> {
        self.heap
            .first()
            .and_then(|key| self.records.get(*key))
            .map(|record| record.deadline)
    }
    pub(crate) fn due(&self, sample: SARawTime) -> bool {
        self.next_deadline()
            .is_some_and(|deadline| deadline.0.elapsed <= sample.elapsed)
    }
    pub(crate) fn claim(&mut self, sample: SARawTime) -> Option<Claimed<L>> {
        if !self.due(sample) {
            return None;
        }
        let key = self.heap[0];
        self.remove_heap(0);
        let record = self.records.get_mut(key).expect("heap record exists");
        let payload = record.payload.take().expect("unclaimed timer owns payload");
        self.claimed += 1;
        Some(Claimed {
            id: SATimerId {
                host: self.host,
                key,
            },
            recipient: record.recipient,
            deadline: record.deadline,
            payload,
        })
    }
    pub(crate) fn release(&mut self, id: SATimerId) {
        // Only an actual claimed-record release changes the count. Repeated or
        // stale tokens cannot release another incarnation or underflow it.
        if id.host == self.host
            && self
                .records
                .get(id.key)
                .is_some_and(|record| record.heap_index.is_none())
            && self.records.remove(id.key).is_some()
        {
            self.claimed -= 1;
        }
    }
    pub(crate) fn cancel_next(&mut self) -> Option<L> {
        let key = *self.heap.first()?;
        match self
            .cancel(SATimerId {
                host: self.host,
                key,
            })
            .expect("host-local heap token")
        {
            SATimerCancel::Removed(payload) => Some(payload),
            _ => unreachable!("heap only contains unclaimed records"),
        }
    }
    fn earlier(&self, left: SlotKey, right: SlotKey) -> bool {
        self.records
            .get(left)
            .expect("heap record")
            .deadline
            .0
            .elapsed
            < self
                .records
                .get(right)
                .expect("heap record")
                .deadline
                .0
                .elapsed
    }
    fn swap(&mut self, left: usize, right: usize) {
        self.heap.swap(left, right);
        self.records
            .get_mut(self.heap[left])
            .expect("heap record")
            .heap_index = Some(left);
        self.records
            .get_mut(self.heap[right])
            .expect("heap record")
            .heap_index = Some(right);
    }
    fn up(&mut self, mut index: usize) {
        while index > 0 {
            let parent = (index - 1) / 2;
            if !self.earlier(self.heap[index], self.heap[parent]) {
                break;
            }
            self.swap(index, parent);
            index = parent;
        }
    }
    fn down(&mut self, mut index: usize) {
        loop {
            let left = index * 2 + 1;
            if left >= self.heap.len() {
                break;
            }
            let right = left + 1;
            let child =
                if right < self.heap.len() && self.earlier(self.heap[right], self.heap[left]) {
                    right
                } else {
                    left
                };
            if !self.earlier(self.heap[child], self.heap[index]) {
                break;
            }
            self.swap(index, child);
            index = child;
        }
    }
    fn remove_heap(&mut self, index: usize) {
        let removed = self.heap[index];
        let last = self.heap.len() - 1;
        if index != last {
            self.swap(index, last);
        }
        self.heap.pop();
        self.records
            .get_mut(removed)
            .expect("removed heap record")
            .heap_index = None;
        if index < self.heap.len() {
            if index > 0 && self.earlier(self.heap[index], self.heap[(index - 1) / 2]) {
                self.up(index);
            } else {
                self.down(index);
            }
        }
    }
}
