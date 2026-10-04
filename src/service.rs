//! Owner-local service registration and owned cross-thread wake destinations.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::identity::{Arena, SlotKey};
use crate::{SAError, SAHostId, SAIdentityKind, SARawDeadline, SARawTime};

/// Host-scoped service incarnation. Reusing storage never revives an old token.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SAServiceId {
    pub(crate) host: SAHostId,
    pub(crate) key: SlotKey,
}

/// Legal application service boundary; it does not imply an SW phase value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SAServicePoint {
    /// Before an eligible application frame update.
    PreUpdate,
    /// Ordinary host maintenance, including idle and minimized windows.
    Maintenance,
    /// An application-selected nested continuation boundary.
    Explicit,
    /// Required work after ordinary admission closes.
    Retirement,
}
impl SAServicePoint {
    fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// Explicit service eligibility. Combine masks using `union`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SAServicePoints(u8);
impl SAServicePoints {
    /// Ordinary maintenance plus explicit safe service.
    pub const ORDINARY: Self = Self(2 | 4);
    /// Required retirement service.
    pub const RETIREMENT: Self = Self(8);
    /// Every supported boundary.
    pub const ALL: Self = Self(15);
    /// One selected boundary.
    pub const fn only(point: SAServicePoint) -> Self {
        Self(1 << point as u8)
    }
    /// Combines eligibility without changing application execution phases.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
    pub(crate) fn contains(self, point: SAServicePoint) -> bool {
        self.0 & point.bit() != 0
    }
}

/// Maximum selected owner records per application service callback. This is
/// not a wall-time bound or preemption of arbitrary destructors/user code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SAServiceBudget {
    /// Application-interpreted record count; independent of SW/SC capacity.
    pub records: usize,
}

/// Registration stores scheduling facts, never an application-borrowing closure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SAServiceSpec {
    /// Legal callback boundaries, including retirement when needed.
    pub points: SAServicePoints,
    /// Selected workload budget passed into each callback.
    pub budget: SAServiceBudget,
    /// Independent maintenance/failure recheck interval in `(0, 1 second]`.
    /// This covers sources such as SC final release with no SW notification.
    pub fallback_interval: Duration,
}

/// A callback's exact registration, boundary, budget and raw sample.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SAServiceRequest {
    /// The claimed service generation.
    pub id: SAServiceId,
    /// The current legal boundary.
    pub point: SAServicePoint,
    /// The registration's selected-work budget.
    pub budget: SAServiceBudget,
    /// Raw sample at this callback's claim.
    pub raw: SARawTime,
}

/// Source state after the application performed bounded legal work. A report
/// observes progress; it never certifies domain/provider/device final access.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SAServiceReport {
    /// Quiescent for now; independent maintenance remains scheduled.
    Quiescent,
    /// Actionable work or budget exhaustion requires a subsequent turn.
    Continue,
    /// The application promises no work is eligible before this raw deadline.
    /// This intentionally overrides periodic fallback; a source that can change
    /// sooner must use `AwaitWake` or `Quiescent` instead.
    WaitUntil(SARawDeadline),
    /// Await this service's owned wake, with an independent finite recheck.
    AwaitWake {
        /// Exact service owning the durable wake destination.
        source: SAServiceId,
        /// Latest permitted recheck; registration fallback may be earlier.
        fallback_deadline: SARawDeadline,
    },
}

/// A bounded explicit service visit. Exhaustion retains scheduling state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SAServiceVisit {
    /// Number of application service callbacks completed.
    pub callbacks: usize,
    /// An eligible service is immediately actionable after the visit.
    pub continuation: bool,
}

/// Snapshot of registration and active callback retention.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SAServiceState {
    /// Future callbacks have been removed.
    pub retired: bool,
    /// This registration is currently borrowed by an application callback.
    pub active: bool,
    /// Last report, absent before the first completed callback.
    pub report: Option<SAServiceReport>,
}

pub(crate) struct WakeState {
    pending: AtomicBool,
    closed: AtomicBool,
    native: crate::backend::wake::NativeWake,
    #[cfg(test)]
    pub(crate) posts: std::sync::atomic::AtomicUsize,
}

/// Owned signal destination suitable for a worker/provider notification route.
/// It stores no application reference and performs no demand, delivery or wait.
/// Native posting is a hint; durable pending state and fallback service remain.
#[derive(Clone)]
pub struct SAWake {
    pub(crate) state: Arc<WakeState>,
}
impl SAWake {
    pub(crate) fn new(native: crate::backend::wake::NativeWake) -> Self {
        Self {
            state: Arc::new(WakeState {
                pending: AtomicBool::new(true),
                closed: AtomicBool::new(false),
                native,
                #[cfg(test)]
                posts: std::sync::atomic::AtomicUsize::new(0),
            }),
        }
    }
    /// Requests a recheck. A failed native post still leaves the durable signal.
    /// Keep this destination alive through an external route's actual quiescence.
    pub fn signal(&self) -> Result<(), SAError> {
        if self.state.closed.load(Ordering::Acquire) {
            return Err(SAError::AdmissionClosed);
        }
        if self.state.pending.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        #[cfg(test)]
        self.state.posts.fetch_add(1, Ordering::Relaxed);
        self.state.native.post()
    }
    pub(crate) fn consume(&self) {
        self.state.pending.swap(false, Ordering::AcqRel);
    }
    pub(crate) fn pending(&self) -> bool {
        self.state.pending.load(Ordering::Acquire)
    }
    pub(crate) fn close(&self) {
        self.state.closed.store(true, Ordering::Release);
    }
}

pub(crate) struct Service {
    pub(crate) alive: bool,
    pub(crate) active: bool,
    pub(crate) spec: SAServiceSpec,
    pub(crate) wake: SAWake,
    pub(crate) next: SARawDeadline,
    pub(crate) report: Option<SAServiceReport>,
}

pub(crate) struct Services {
    records: Arena<Service>,
    order: Vec<SlotKey>,
    cursor: usize,
    pub(crate) depth: usize,
}
impl Services {
    pub(crate) fn new() -> Self {
        Self {
            records: Arena::new(SAIdentityKind::Service),
            order: Vec::new(),
            cursor: 0,
            depth: 0,
        }
    }
    pub(crate) fn register(
        &mut self,
        host: SAHostId,
        spec: SAServiceSpec,
        raw: SARawTime,
        native: crate::backend::wake::NativeWake,
    ) -> Result<SAServiceId, SAError> {
        if spec.budget.records == 0
            || spec.fallback_interval.is_zero()
            || spec.fallback_interval > Duration::from_secs(1)
        {
            return Err(SAError::InvalidInput(
                "service budget must be positive and fallback in (0, 1 second]",
            ));
        }
        self.order
            .try_reserve(1)
            .map_err(|_| SAError::AllocationFailed)?;
        let key = self.records.reserve()?;
        self.records.insert(
            key,
            Service {
                alive: true,
                active: false,
                spec,
                wake: SAWake::new(native),
                next: raw.checked_add(Duration::ZERO)?,
                report: None,
            },
        );
        self.order.push(key);
        Ok(SAServiceId { host, key })
    }
    pub(crate) fn get(&self, host: SAHostId, id: SAServiceId) -> Result<&Service, SAError> {
        if id.host != host {
            return Err(SAError::ForeignHost);
        }
        self.records.get(id.key).ok_or(SAError::StaleIdentity)
    }
    pub(crate) fn remove(
        &mut self,
        host: SAHostId,
        id: SAServiceId,
    ) -> Result<SAServiceState, SAError> {
        self.get(host, id)?;
        let record = self.records.get_mut(id.key).unwrap();
        record.alive = false;
        record.wake.close();
        let state = SAServiceState {
            retired: true,
            active: record.active,
            report: record.report,
        };
        if !record.active {
            self.reclaim(id.key);
        }
        Ok(state)
    }
    fn reclaim(&mut self, key: SlotKey) {
        self.records.remove(key);
        if let Some(index) = self.order.iter().position(|candidate| *candidate == key) {
            self.order.remove(index);
            if index < self.cursor {
                self.cursor -= 1;
            }
        }
    }
    fn due(record: &Service, point: SAServicePoint, raw: SARawTime) -> bool {
        record.alive
            && !record.active
            && record.spec.points.contains(point)
            && (record.wake.pending() || record.next.time().elapsed <= raw.elapsed)
    }
    pub(crate) fn next(
        &mut self,
        host: SAHostId,
        point: SAServicePoint,
        raw: SARawTime,
    ) -> Option<SAServiceRequest> {
        for _ in 0..self.order.len() {
            self.cursor %= self.order.len();
            let key = self.order[self.cursor];
            self.cursor += 1;
            let record = self.records.get_mut(key).unwrap();
            if Self::due(record, point, raw) {
                record.active = true;
                // Drain before application arm/recheck, never after its final check.
                record.wake.consume();
                return Some(SAServiceRequest {
                    id: SAServiceId { host, key },
                    point,
                    budget: record.spec.budget,
                    raw,
                });
            }
        }
        None
    }
    pub(crate) fn release(
        &mut self,
        host: SAHostId,
        request: SAServiceRequest,
        report: Option<SAServiceReport>,
    ) -> Result<(), SAError> {
        let record = self.records.get_mut(request.id.key).unwrap();
        record.active = false;
        if !record.alive {
            self.reclaim(request.id.key);
            return Ok(());
        }
        if let Some(report) = report {
            let fallback = request.raw.checked_add(record.spec.fallback_interval)?;
            let next = match report {
                SAServiceReport::Quiescent => fallback,
                SAServiceReport::Continue => request.raw.checked_add(Duration::ZERO)?,
                SAServiceReport::WaitUntil(deadline) => deadline,
                SAServiceReport::AwaitWake {
                    source,
                    fallback_deadline,
                } => {
                    if source != request.id {
                        return Err(SAError::StaleIdentity);
                    }
                    if fallback_deadline.time().host != host {
                        return Err(SAError::ForeignHost);
                    }
                    if fallback_deadline.time().elapsed < fallback.time().elapsed {
                        fallback_deadline
                    } else {
                        fallback
                    }
                }
            };
            if next.time().host != host {
                return Err(SAError::ForeignHost);
            }
            record.next = next;
            record.report = Some(report);
        }
        Ok(())
    }
    pub(crate) fn pending(&self, point: SAServicePoint, raw: SARawTime) -> bool {
        self.records
            .iter()
            .any(|(_, record)| Self::due(record, point, raw))
    }
    pub(crate) fn deadline(&self, point: SAServicePoint, raw: SARawTime) -> Option<SARawDeadline> {
        self.records
            .iter()
            .filter(|(_, record)| {
                record.alive && !record.active && record.spec.points.contains(point)
            })
            .map(|(_, record)| {
                if record.wake.pending() {
                    SARawDeadline(raw)
                } else {
                    record.next
                }
            })
            .min_by_key(|deadline| deadline.time().elapsed)
    }
    pub(crate) fn counts(&self) -> (usize, usize) {
        let active = self
            .records
            .iter()
            .filter(|(_, record)| record.active)
            .count();
        let retirement = self
            .records
            .iter()
            .filter(|(_, record)| {
                record.alive && record.spec.points.contains(SAServicePoint::Retirement)
            })
            .count();
        (active, retirement)
    }
    pub(crate) fn close_all(&mut self) {
        for key in &self.order {
            if let Some(record) = self.records.get_mut(*key) {
                record.alive = false;
                record.wake.close();
            }
        }
    }
}
