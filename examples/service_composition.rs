//! Real SW owner publication and independent, borrowed SC cleanup. This uses
//! controlled CPU data, not a renderer/provider final-use certification.

#[cfg(test)]
use crate as solapp;
use solapp::*;
use solcache::{SCAccountingDomain, SCAllocationClass, SCBacking, SCCleanupContext};
use solworker::{
    SWExecutionClass, SWNotifyBinding, SWNotifyLimits, SWNotifyRoute, SWOwnedLimits, SWOwner,
    SWPhase, SWPumpBudget, SWRuntime, SWRuntimeConfig, SWSpawnOptions, SWTaskStatus,
    SWWorkerConfig,
};
use std::{cell::Cell, num::NonZeroUsize, rc::Rc, time::Duration};

fn error(value: impl std::fmt::Debug) -> SAError {
    SAError::application(format!("{value:?}"))
}
fn runtime() -> SWRuntime {
    let config = SWRuntimeConfig::new(
        1,
        [
            SWWorkerConfig::new(0),
            SWWorkerConfig::new(0),
            SWWorkerConfig::new(1),
        ],
    )
    .unwrap();
    SWRuntime::builder(config)
        .with_owned_limits(SWOwnedLimits::new(8, 8, [4; 3], [1; 3]).unwrap())
        .with_notification_limits(SWNotifyLimits {
            routes: 1,
            bindings: 3,
        })
        .build()
        .unwrap()
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkLifecycle {
    Running,
    Draining,
}

struct Demo<'scope> {
    runtime: &'scope mut SWRuntime,
    cleanup: &'scope SCCleanupContext<Rc<Cell<usize>>>,
    backing: Option<SCBacking<'scope, Rc<Cell<usize>>>>,
    domain: SCAccountingDomain,
    owner: SWOwner<Vec<u32>>,
    route: Option<SWNotifyRoute>,
    bindings: Vec<SWNotifyBinding>,
    work: Option<SAServiceId>,
    clean: Option<SAServiceId>,
    final_admitted: bool,
    lifecycle: WorkLifecycle,
    release: Option<SARawDeadline>,
    cleanup_visits_after_close: usize,
    cleaned: usize,
    joined: bool,
}
impl<'scope> Demo<'scope> {
    fn new(
        runtime: &'scope mut SWRuntime,
        cleanup: &'scope SCCleanupContext<Rc<Cell<usize>>>,
    ) -> Self {
        let domain = SCAccountingDomain::new();
        let backing = cleanup
            .try_acquire(
                Rc::new(Cell::new(11)),
                &domain,
                8,
                SCAllocationClass::Resident,
            )
            .unwrap();
        let mut owner = runtime
            .owner(Vec::new(), NonZeroUsize::new(2).unwrap())
            .unwrap();
        owner.set_phase(SWPhase(1)).unwrap();
        Self {
            runtime,
            cleanup,
            backing: Some(backing),
            domain,
            owner,
            route: None,
            bindings: Vec::new(),
            work: None,
            clean: None,
            final_admitted: false,
            lifecycle: WorkLifecycle::Running,
            release: None,
            cleanup_visits_after_close: 0,
            cleaned: 0,
            joined: false,
        }
    }
    fn begin_draining(&mut self, release: Option<SARawDeadline>) {
        if self.lifecycle == WorkLifecycle::Running {
            // Close admission and ordinary publication before closing their route.
            self.lifecycle = WorkLifecycle::Draining;
            self.release = release;
            self.owner.close();
            if let Some(route) = &mut self.route {
                route.close().unwrap();
            }
            self.runtime.begin_shutdown();
        } else if release.is_none() {
            // Host retirement overrides the demonstration's delayed final release.
            self.release = None;
        }
    }

    fn retire(&mut self, cx: &mut SAContext<'_, Self>, raw: SARawTime, budget: usize) -> bool {
        if self
            .release
            .is_none_or(|d| raw.elapsed() >= d.time().elapsed())
        {
            self.backing.take();
        }
        self.cleaned += self.cleanup.drain_budget(budget);
        let route_quiet = self.route.as_ref().is_none_or(SWNotifyRoute::is_quiescent);
        if route_quiet {
            // A claimed notifier can still signal after close. Keep its SA wake
            // destination until the route proves all such claims have retired.
            if let Some(work) = self.work.take() {
                cx.remove_service(work).unwrap();
            }
            self.joined = self.runtime.try_shutdown().unwrap();
        }
        self.backing.is_none() && self.cleanup.pending() == 0 && route_quiet && self.joined
    }

    fn work(&mut self, _: &mut SAContext<'_, Self>, request: SAServiceRequest) -> SAServiceReport {
        if self.lifecycle == WorkLifecycle::Draining {
            return SAServiceReport::Quiescent;
        }
        // SA has consumed its wake before entry. Arm SW, inspect/pump its
        // authoritative state, and recheck the epoch before reporting a wait.
        let route = self.route.as_mut().unwrap();
        let stamp = route.prepare_wait().unwrap();
        let report = self
            .owner
            .pump(SWPhase(1), SWPumpBudget::new(request.budget.records))
            .unwrap();
        if !self.final_admitted && self.owner.state().as_slice() == [1] {
            // The intermediate owner publication preceded this final task's
            // admission; watching only its completion could not wake that owner.
            self.final_admitted = true;
            let (task, _) = self
                .runtime
                .lane(SWExecutionClass::High)
                .try_spawn(SWSpawnOptions::default(), || 7_u32)
                .unwrap();
            let shared = task.into_shared();
            let completion = shared.completion();
            self.bindings
                .push(route.watch_completion(&completion).unwrap());
            self.owner
                .on_ready(&completion, SWPhase(1), move |state, status| {
                    assert_eq!(status, SWTaskStatus::Succeeded);
                    assert!(shared.try_result().is_some());
                    state.push(7);
                })
                .unwrap();
        }
        if self.owner.state().as_slice() == [1, 7] {
            let release = request.raw.checked_add(Duration::from_millis(35)).unwrap();
            self.begin_draining(Some(release));
            // This source is finished. A separate SC source will make progress
            // without SW notifications or redraws after a later final release.
            return SAServiceReport::Quiescent;
        }
        if route.changed_since(stamp).unwrap() || report.processed() == request.budget.records {
            SAServiceReport::Continue
        } else {
            SAServiceReport::AwaitWake {
                source: request.id,
                fallback_deadline: request.raw.checked_add(Duration::from_millis(10)).unwrap(),
            }
        }
    }
}
impl SAApplication for Demo<'_> {
    type Message = ();
    type LocalEvent = ();
    fn started(&mut self, cx: &mut SAContext<'_, Self>) -> Result<(), SAError> {
        cx.create_window(SAWindowSpec {
            visible: false,
            ..SAWindowSpec::default()
        })
        .map_err(|r| r.into_parts().1)?;
        let spec = SAServiceSpec {
            points: SAServicePoints::ALL,
            budget: SAServiceBudget { records: 1 },
            fallback_interval: Duration::from_millis(10),
        };
        let work = cx.register_service(spec)?;
        self.work = Some(work);
        self.clean = Some(cx.register_service(spec)?);
        let wake = cx.wake(work)?;
        self.route = Some(
            self.runtime
                .notification_route(move || wake.signal().map_err(std::io::Error::other))
                .map_err(error)?,
        );
        // Retain the route and each installed binding before the next fallible
        // step, so startup rollback can still observe notifier quiescence.
        let route = self.route.as_mut().unwrap();
        self.bindings
            .push(route.watch_owner(&self.owner).map_err(error)?);
        self.bindings.push(route.watch_progress().map_err(error)?);
        let prepared = self
            .owner
            .prepare_delivery(SWPhase(1), |state| state.push(1))
            .map_err(error)?;
        prepared.ticket.ready();
        Ok(())
    }
    fn service(
        &mut self,
        cx: &mut SAContext<'_, Self>,
        request: SAServiceRequest,
    ) -> SAServiceReport {
        let is_work = Some(request.id) == self.work;
        if !is_work {
            assert_eq!(Some(request.id), self.clean);
        }
        // Native host retirement services can run before the first stopping poll.
        // Either service must close ordinary work before doing anything else.
        if request.point == SAServicePoint::Retirement {
            self.begin_draining(None);
        }
        if self.lifecycle == WorkLifecycle::Draining {
            if !is_work {
                self.cleanup_visits_after_close += 1;
            }
            if self.retire(cx, request.raw, request.budget.records) {
                cx.request_stop();
            }
        } else if is_work {
            return self.work(cx, request);
        } else {
            self.cleaned += self.cleanup.drain_budget(request.budget.records);
        }
        if self.cleanup.pending() != 0 {
            SAServiceReport::Continue
        } else {
            SAServiceReport::Quiescent
        }
    }
    fn stopping(&mut self, cx: &mut SAContext<'_, Self>) -> SAStopProgress {
        self.begin_draining(None);
        let raw = cx.clock().raw;
        if self.retire(cx, raw, 1) {
            SAStopProgress::Settled
        } else {
            SAStopProgress::Pending
        }
    }
}

#[cfg(not(test))]
fn main() {
    let cleanup = SCCleanupContext::new();
    let mut runtime = runtime();
    let mut app = Demo::new(&mut runtime, &cleanup);
    let mut host = SAHost::<Demo<'_>>::new(SAHostConfig::default()).unwrap();
    let exit = host.run(&mut app).unwrap();
    assert_eq!(app.owner.state().as_slice(), [1, 7]);
    assert_eq!(app.cleaned, 1);
    assert!(app.joined);
    assert_eq!(app.domain.snapshot().total_declared_bytes, 0);
    assert_eq!(exit.windows_retired, 1);
    // SA observations describe its own obligations. The separate assertions
    // above establish the application's actual SW join and SC cleanup.
    let shutdown = host.shutdown_snapshot();
    assert_eq!(shutdown.state, solapp::SAHostState::Closed);
    assert!(shutdown.application_settled);
    assert_eq!(shutdown.external_window_leases, 0);
    assert_eq!(shutdown.pending_native_destructions, 0);
    println!(
        "SW owner publications [1, 7]; independent SC cleanup {}; native retirement {}; runtime joined",
        app.cleaned, exit.windows_retired
    );
    println!("Host shutdown observations: {shutdown:?}");
}

#[cfg(test)]
#[path = "../tests/unit/composition.rs"]
mod tests;
