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
    work_closed: bool,
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
            work_closed: false,
            release: None,
            cleanup_visits_after_close: 0,
            cleaned: 0,
            joined: false,
        }
    }
    fn work(&mut self, _: &mut SAContext<'_, Self>, request: SAServiceRequest) -> SAServiceReport {
        if self.work_closed {
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
            self.owner.close();
            route.close().unwrap();
            self.runtime.begin_shutdown();
            self.work_closed = true;
            self.release = Some(request.raw.checked_add(Duration::from_millis(35)).unwrap());
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
        let mut route = self
            .runtime
            .notification_route(move || wake.signal().map_err(std::io::Error::other))
            .map_err(error)?;
        let _owner_binding = route.watch_owner(&self.owner).map_err(error)?;
        let _progress_binding = route.watch_progress().map_err(error)?;
        // Bindings detach on Drop; keep these interests in the route itself.
        self.bindings = vec![_owner_binding, _progress_binding];
        self.route = Some(route);
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
        if Some(request.id) == self.work {
            return self.work(cx, request);
        }
        assert_eq!(Some(request.id), self.clean);
        if self.work_closed {
            self.cleanup_visits_after_close += 1;
            if self
                .release
                .is_some_and(|d| request.raw.elapsed() >= d.time().elapsed())
            {
                self.backing.take();
            }
        }
        self.cleaned += self.cleanup.drain_budget(request.budget.records);
        if self.work_closed
            && self.backing.is_none()
            && self.cleanup.pending() == 0
            && self.route.as_ref().unwrap().is_quiescent()
        {
            // A claimed SW notifier may still signal after close. Keep its SA
            // wake destination alive until those claims have retired.
            if let Some(work) = self.work.take() {
                cx.remove_service(work).unwrap();
            }
            self.joined = self.runtime.try_shutdown().unwrap();
            if self.joined {
                cx.request_stop();
            }
        }
        if self.cleanup.pending() != 0 {
            SAServiceReport::Continue
        } else {
            SAServiceReport::Quiescent
        }
    }
    fn stopping(&mut self, _: &mut SAContext<'_, Self>) -> SAStopProgress {
        self.owner.close();
        if let Some(route) = &mut self.route {
            route.close().unwrap();
        }
        self.backing.take();
        self.cleaned += self.cleanup.drain_budget(1);
        self.runtime.begin_shutdown();
        let route_quiet = self.route.as_ref().is_none_or(SWNotifyRoute::is_quiescent);
        if route_quiet && self.cleanup.pending() == 0 {
            self.joined = self.runtime.try_shutdown().unwrap();
        }
        if self.joined && route_quiet {
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
    assert!(app.cleanup_visits_after_close >= 2 && app.joined);
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
#[test]
fn owner_wake_precedes_final_admission_and_sc_fallback_outlives_worker_route() {
    use crate::backend::{BackendOps, test::TestBackend};
    use crate::host::Core;
    let cleanup = SCCleanupContext::new();
    let mut runtime = runtime();
    let mut app = Demo::new(&mut runtime, &cleanup);
    let mut core = Core::<Demo<'_>>::new().unwrap();
    let mut native = TestBackend::default();
    core.clock.manual = Some(Duration::ZERO);
    core.start(&mut app, BackendOps::Test(&mut native));
    assert!(core.failure.is_none(), "{:?}", core.failure);
    assert!(!app.final_admitted && app.owner.state().is_empty());
    // There is no final completion yet. The owner's own interest admits it.
    core.pump_ordinary(&mut app, BackendOps::Test(&mut native));
    assert!(app.final_admitted);
    let timeout = std::time::Instant::now() + Duration::from_secs(2);
    while !app.work_closed {
        assert!(
            std::time::Instant::now() < timeout,
            "worker publication did not progress"
        );
        core.clock.manual = Some(Duration::from_millis(10));
        core.pump_ordinary(&mut app, BackendOps::Test(&mut native));
        std::thread::yield_now();
    }
    assert_eq!(app.owner.state().as_slice(), [1, 7]);
    assert!(app.backing.is_some() && app.cleaned == 0);
    // The closed route cannot wake this independent final release. Only the
    // cleanup source's raw fallback drives its bounded cleanup and stop.
    core.clock.manual = Some(Duration::from_millis(100));
    core.pump_ordinary(&mut app, BackendOps::Test(&mut native));
    assert_eq!(app.cleaned, 1);
    assert_eq!(app.domain.snapshot().total_declared_bytes, 0);
    let mut millis = 100;
    while !app.joined {
        assert!(
            std::time::Instant::now() < timeout,
            "runtime did not join after quiescence"
        );
        millis += 10;
        core.clock.manual = Some(Duration::from_millis(millis));
        core.pump_ordinary(&mut app, BackendOps::Test(&mut native));
        std::thread::yield_now();
    }
    assert!(app.joined);
    core.poll_stop(&mut app, BackendOps::Test(&mut native));
    assert_eq!(core.state, SAHostState::Retiring);
    core.native_destroyed(native.complete_destruction().unwrap());
    assert_eq!(core.state, SAHostState::Closed);
}
