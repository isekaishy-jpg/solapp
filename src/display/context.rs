use super::*;
use crate::backend::BackendOps;
use crate::host::{Core, SAStopReason};
use crate::{SAApplication, SAContext, SAContextPhase, SAHostState, SARejected, SAWindowState};
use std::panic::{AssertUnwindSafe, catch_unwind};

impl<A: SAApplication> SAContext<'_, A> {
    /// Queries current driver choices. Every native apply revalidates them.
    pub fn monitors(&self, target: SAWindowTarget) -> Result<Vec<SAMonitorSnapshot>, SAError> {
        self.core.check_owner()?;
        self.window_state(target)?;
        self.core
            .windows
            .get(target.id)?
            .native
            .monitors(self.core.id)
    }
    /// Queries backend-reported mode and native geometry, not GPU readiness.
    pub fn display_observed(&self, target: SAWindowTarget) -> Result<SADisplayObserved, SAError> {
        self.core.check_owner()?;
        self.window_state(target)?;
        Ok(self.core.windows.get(target.id)?.display.observed())
    }
    /// Queries one retained pending, active or last settled transition.
    /// Progress callbacks own every status; this bounded query is not history.
    pub fn display_transition(
        &self,
        receipt: SADisplayReceipt,
    ) -> Result<SADisplayTransition, SAError> {
        self.core.check_owner()?;
        let target = receipt.transition.target();
        self.window_state(target)?;
        let state = &self.core.windows.get(target.id)?.display;
        [state.pending(), state.active(), state.settled()]
            .into_iter()
            .flatten()
            .find(|event| event.receipt == receipt)
            .cloned()
            .ok_or(SAError::StaleIdentity)
    }
    /// Admits serialized settings. Before requesting, end incompatible old
    /// presentation use. Admission is neither native success nor persistence.
    /// Rejection preserves the owned request.
    pub fn request_display(
        &mut self,
        target: SAWindowTarget,
        request: SADisplayRequest,
    ) -> Result<SADisplayReceipt, SARejected<SADisplayRequest>> {
        let result = (|| {
            self.ordinary()?;
            self.require_live_window(target)?;
            self.core
                .display_events
                .try_reserve(2)
                .map_err(|_| SAError::AllocationFailed)?;
            let admission = self
                .core
                .windows
                .get_mut(target.id)?
                .display
                .request(request.clone())?;
            if let Some(event) = admission.superseded {
                self.core.display_events.push_back(event);
            }
            self.core.display_events.push_back(
                self.core
                    .windows
                    .get(target.id)?
                    .display
                    .pending()
                    .unwrap()
                    .clone(),
            );
            Ok(admission.receipt)
        })();
        result.map_err(|error| SARejected::new(request, error))
    }
    /// Reports the exact renderer revision after native apply. A failed report
    /// settles this operation; retained access independently covers final use.
    /// Retirement permits settlement without new admission.
    pub fn report_presentation(
        &mut self,
        receipt: SADisplayReceipt,
        status: SAPresentationStatus,
    ) -> Result<(), SAError> {
        self.core.check_owner()?;
        self.window_state(receipt.transition.target())?;
        self.core
            .display_events
            .try_reserve(1)
            .map_err(|_| SAError::AllocationFailed)?;
        let event = self
            .core
            .windows
            .get_mut(receipt.transition.target().id)?
            .display
            .report(receipt, status)?;
        self.core.display_events.push_back(event);
        Ok(())
    }
    /// Closes one generation's new input/access/display admission. Destruction
    /// waits for retained access release and native acknowledgment. Other windows
    /// and the application runtime continue in the same host.
    pub fn request_close(&mut self, target: SAWindowTarget) -> Result<SAWindowState, SAError> {
        self.ordinary()?;
        if self.window_state(target)? == SAWindowState::Retiring {
            return Ok(SAWindowState::Retiring);
        }
        self.core
            .display_events
            .try_reserve(2)
            .map_err(|_| SAError::AllocationFailed)?;
        self.core.close_target(target)?;
        Ok(SAWindowState::Retiring)
    }
}

impl<A: SAApplication> Core<A> {
    pub(crate) fn close_target(&mut self, target: SAWindowTarget) -> Result<(), SAError> {
        let record = self.windows.get_mut(target.id)?;
        if record.generation != target.generation {
            return Err(SAError::StaleIdentity);
        }
        record.closing = true;
        record.access.begin_retirement();
        record.native.text(None);
        record.input_mode.confirmed.relative_motion = false;
        if let Some(event) = record.display.begin_retirement() {
            self.display_events.push_back(event);
        }
        if let Some(active) = record.display.active().cloned() {
            let event = record
                .display
                .fail(active.receipt, SAError::AdmissionClosed)?;
            self.display_events.push_back(event);
        }
        self.input.retire(target);
        self.text.retire(target);
        self.native_input.retire(target);
        if self.relative_target == Some(target) {
            self.relative_target = None;
        }
        if self.pacing.state.target == Some(target) {
            self.pacing
                .set(None, crate::SAPacingPolicy::Disabled, self.clock.sample());
        }
        self.refresh_cursor_suppression();
        Ok(())
    }

    pub(crate) fn pump_display(&mut self, app: &mut A, operations: BackendOps<'_>) {
        if !self.ordinary_open()
            && (self.state != SAHostState::Stopping || self.application_settled)
        {
            return;
        }
        if self.ordinary_open() {
            let mut allocation_failed = false;
            let mut applied = 0;
            for (_, record) in self.windows.iter_mut() {
                if record.closing
                    || record.display.active().is_some()
                    || record.display.pending().is_none()
                {
                    continue;
                }
                if applied == self.event_budget {
                    break;
                }
                if self.display_events.try_reserve(2).is_err() {
                    allocation_failed = true;
                    break;
                }
                let apply = record.display.start_next().unwrap();
                applied += 1;
                self.display_events.push_back(apply.transition.clone());
                let result = record.native.apply_display(
                    self.id,
                    &apply.transition.request,
                    apply.windowed_placement,
                );
                let observed = record.native.display_observed();
                if let Ok(observed) = observed.as_ref() {
                    record.display.observe(*observed);
                }
                let result = result
                    .and_then(|()| record.display.applied(apply.transition.receipt, observed?));
                let event = match result {
                    Ok(()) => record.display.active().unwrap().clone(),
                    Err(error) => record
                        .display
                        .fail(apply.transition.receipt, error)
                        .unwrap(),
                };
                self.display_events.push_back(event);
            }
            if allocation_failed {
                self.fail(SAError::AllocationFailed, SAStopReason::BackendFailed);
            }
        } else {
            let mut allocation_failed = false;
            for (_, record) in self.windows.iter_mut() {
                if record.display.pending().is_some() {
                    if self.display_events.try_reserve(1).is_err() {
                        allocation_failed = true;
                        break;
                    }
                    if let Some(event) = record.display.begin_retirement() {
                        self.display_events.push_back(event);
                    }
                }
            }
            if allocation_failed {
                self.fail(SAError::AllocationFailed, SAStopReason::BackendFailed);
            }
        }
        let mut cx = SAContext::new(self, operations, SAContextPhase::Display);
        for _ in 0..cx.core.event_budget {
            if cx.core.application_settled {
                break;
            }
            let Some(event) = cx.core.display_events.pop_front() else {
                break;
            };
            let enabled = cx.core.input.enabled;
            cx.core.input.enabled = false;
            cx.core.active_callbacks += 1;
            let result = catch_unwind(AssertUnwindSafe(|| app.display_transition(&mut cx, &event)));
            cx.core.active_callbacks -= 1;
            cx.core.input.enabled = enabled;
            if let Err(payload) = result {
                cx.core.fail(
                    SAError::ApplicationPanicked {
                        phase: SAContextPhase::Display,
                        message: crate::host::panic_message(payload),
                    },
                    SAStopReason::CallbackPanicked,
                );
                break;
            }
        }
    }

    pub(crate) fn reap_closing_windows(&mut self, mut operations: BackendOps<'_>) {
        if !self.ordinary_open() {
            return;
        }
        let any_closing = self.windows.iter().any(|(_, record)| record.closing);
        if !any_closing {
            return;
        }
        let mut failure = None;
        if self.relative_target.is_none()
            && (self.raw_input.confirmed != Some(crate::SARawInputPolicy::Never)
                || self.raw_input.failure.is_some())
        {
            match operations.raw_input(crate::SARawInputPolicy::Never) {
                Ok(()) => {
                    self.raw_input.confirmed = Some(crate::SARawInputPolicy::Never);
                    self.raw_input.failure = None;
                }
                Err(error) => {
                    self.raw_input.failure = Some(error.clone());
                    failure = Some(error);
                }
            }
        }
        for (_, record) in self.windows.iter_mut() {
            if !record.closing {
                continue;
            }
            if record.input_mode.confirmed.confine_pointer || record.confinement_uncertain {
                match record.native.confine(false) {
                    Ok(()) => {
                        record.input_mode.confirmed.confine_pointer = false;
                        record.confinement_uncertain = false;
                    }
                    Err(error) => {
                        record.input_mode.failure = Some(error.clone());
                        failure.get_or_insert(error);
                    }
                }
            }
        }
        if let Some(error) = failure {
            self.fail(error, SAStopReason::BackendFailed);
        } else {
            self.reap_windows();
        }
    }
}
