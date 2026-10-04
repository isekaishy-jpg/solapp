//! Host-scoped raw time and a separate continuously rescalable application clock.

use crate::{SAError, SAHostId};
use std::time::{Duration, Instant};

/// Monotonic elapsed time from a particular host's construction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SARawTime {
    pub(crate) host: SAHostId,
    pub(crate) elapsed: Duration,
}
impl SARawTime {
    /// Elapsed monotonic duration; this is not wall time or native event time.
    pub fn elapsed(self) -> Duration {
        self.elapsed
    }
    /// Creates a checked raw deadline in this same clock domain.
    pub fn checked_add(self, duration: Duration) -> Result<SARawDeadline, SAError> {
        Ok(SARawDeadline(Self {
            host: self.host,
            elapsed: self
                .elapsed
                .checked_add(duration)
                .ok_or(SAError::TimeOverflow)?,
        }))
    }
}

/// A deadline bound to one host's raw monotonic clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SARawDeadline(pub(crate) SARawTime);
impl SARawDeadline {
    /// Inspects the deadline without losing its host-clock identity.
    pub fn time(self) -> SARawTime {
        self.0
    }
}

/// A fresh observation of this host's raw monotonic clock.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SAClockSnapshot {
    /// The sample used for timers and receipt/delivery timestamps.
    pub raw: SARawTime,
    /// Separate application time; overflow remains visible without hiding raw time.
    pub application: Result<SAApplicationTime, SAError>,
}

/// Continuous host-scoped application time, independent of physical deadlines.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SAApplicationTime {
    pub(crate) host: SAHostId,
    pub(crate) elapsed: Duration,
}
impl SAApplicationTime {
    /// Elapsed application time. Pausing/rescaling never changes raw timers.
    pub fn elapsed(self) -> Duration {
        self.elapsed
    }
    /// Legacy whole-millisecond projection, truncating submilliseconds and wrapping
    /// at 32 bits. Host scheduling never uses this projection for deadlines.
    pub fn milliseconds_wrapping(self) -> u32 {
        self.elapsed.as_millis() as u32
    }
}

pub(crate) struct Clock {
    host: SAHostId,
    origin: Instant,
    app_raw_anchor: Duration,
    app_anchor: Duration,
    multiplier: f64,
    #[cfg(test)]
    pub(crate) manual: Option<Duration>,
}
impl Clock {
    pub(crate) fn new(host: SAHostId) -> Self {
        Self {
            host,
            origin: Instant::now(),
            app_raw_anchor: Duration::ZERO,
            app_anchor: Duration::ZERO,
            multiplier: 1.0,
            #[cfg(test)]
            manual: None,
        }
    }
    pub(crate) fn sample(&self) -> SARawTime {
        let elapsed = self.origin.elapsed();
        #[cfg(test)]
        let elapsed = self.manual.unwrap_or(elapsed);
        SARawTime {
            host: self.host,
            elapsed,
        }
    }
    pub(crate) fn native_deadline(&self, deadline: SARawDeadline) -> Option<Instant> {
        self.origin.checked_add(deadline.0.elapsed)
    }
    pub(crate) fn application(&self, raw: SARawTime) -> Result<SAApplicationTime, SAError> {
        if raw.host != self.host {
            return Err(SAError::ForeignHost);
        }
        let delta = raw
            .elapsed
            .checked_sub(self.app_raw_anchor)
            .ok_or(SAError::InvalidInput("raw clock moved backwards"))?;
        let scaled = if self.multiplier == 1.0 {
            delta
        } else if self.multiplier == 0.0 {
            Duration::ZERO
        } else {
            Duration::try_from_secs_f64(delta.as_secs_f64() * self.multiplier)
                .map_err(|_| SAError::TimeOverflow)?
        };
        Ok(SAApplicationTime {
            host: self.host,
            elapsed: self
                .app_anchor
                .checked_add(scaled)
                .ok_or(SAError::TimeOverflow)?,
        })
    }
    pub(crate) fn rescale(&mut self, multiplier: f64) -> Result<(), SAError> {
        if !multiplier.is_finite() || multiplier < 0.0 {
            return Err(SAError::InvalidInput(
                "application clock multiplier must be nonnegative and finite",
            ));
        }
        let raw = self.sample();
        let current = self.application(raw)?;
        self.app_anchor = current.elapsed;
        self.app_raw_anchor = raw.elapsed;
        self.multiplier = multiplier;
        Ok(())
    }
}
