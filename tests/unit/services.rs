use crate::backend::{BackendOps, test::TestBackend};
use crate::host::Core;
use crate::*;
use std::sync::atomic::Ordering;
use std::time::Duration;

#[derive(Default)]
struct App {
    first: Option<SAServiceId>,
    calls: Vec<SAServiceId>,
    nested: bool,
    remove: bool,
    backlog: bool,
    race: Option<SAWake>,
    frames: usize,
    disable_frame: bool,
    settled: bool,
}
impl SAApplication for App {
    type Message = ();
    type LocalEvent = ();
    fn started(&mut self, _: &mut SAContext<'_, Self>) -> Result<(), SAError> {
        Ok(())
    }
    fn stopping(&mut self, _: &mut SAContext<'_, Self>) -> SAStopProgress {
        self.settled = true;
        SAStopProgress::Settled
    }
    fn service(
        &mut self,
        cx: &mut SAContext<'_, Self>,
        request: SAServiceRequest,
    ) -> SAServiceReport {
        assert!(!self.settled, "application service after Settled");
        self.calls.push(request.id);
        assert!(!cx.core.input.enabled);
        assert_eq!(cx.phase(), SAContextPhase::Service(request.point));
        if self.disable_frame {
            cx.set_pacing(None, SAPacingPolicy::Disabled).unwrap();
        }
        if Some(request.id) == self.first {
            assert!(cx.service_state(request.id).unwrap().active);
            if self.remove {
                self.remove = false;
                let removed = cx.remove_service(request.id).unwrap();
                assert!(removed.active && removed.retired);
                assert!(cx.service_state(request.id).unwrap().active);
                assert_eq!(
                    cx.core.services.counts(),
                    (1, 1),
                    "retired active service remains in shutdown snapshot"
                );
            }
            if self.nested {
                self.nested = false;
                let phase = cx.phase();
                let nested = cx
                    .service_pending(self, SAServicePoint::Explicit, 8)
                    .unwrap();
                assert_eq!(nested.callbacks, 1);
                assert_eq!(cx.phase(), phase);
            }
        }
        if let Some(wake) = self.race.take() {
            std::thread::spawn(move || wake.signal().unwrap())
                .join()
                .unwrap();
        }
        if self.backlog {
            SAServiceReport::Continue
        } else {
            SAServiceReport::Quiescent
        }
    }
    fn frame(&mut self, cx: &mut SAContext<'_, Self>, _: SAFrame) {
        assert_eq!(cx.phase(), SAContextPhase::Frame);
        assert!(!cx.core.input.enabled);
        cx.service_pending(self, SAServicePoint::Explicit, 1)
            .unwrap();
        assert_eq!(cx.phase(), SAContextPhase::Frame);
        self.frames += 1;
    }
}

#[test]
fn application_settled_closes_services_while_native_cleanup_retries_independently() {
    let mut core = Core::<App>::new().unwrap();
    core.clock.manual = Some(Duration::ZERO);
    let mut backend = TestBackend::default();
    let mut app = App::default();
    core.start(&mut app, BackendOps::Test(&mut backend));
    let wake;
    {
        let mut cx = SAContext::new(
            &mut core,
            BackendOps::Test(&mut backend),
            SAContextPhase::Startup,
        );
        cx.create_window(SAWindowSpec::default()).unwrap();
        let id = cx.register_service(spec(SAServicePoints::ALL)).unwrap();
        wake = cx.wake(id).unwrap();
        cx.request_stop();
    }
    backend.fail_raw = true;
    core.pump_retirement(&mut app, BackendOps::Test(&mut backend));
    assert_eq!(app.calls.len(), 1);
    core.poll_stop(&mut app, BackendOps::Test(&mut backend));
    assert!(app.settled);
    assert_eq!(core.state, SAHostState::Stopping);
    core.clock.manual = Some(Duration::from_secs(1));
    core.pump_retirement(&mut app, BackendOps::Test(&mut backend));
    core.poll_stop(&mut app, BackendOps::Test(&mut backend));
    assert_eq!(app.calls.len(), 1);
    assert_eq!(wake.signal(), Err(SAError::AdmissionClosed));
    assert_eq!(
        core.services
            .deadline(SAServicePoint::Retirement, core.clock.sample()),
        None
    );
    backend.fail_raw = false;
    core.poll_stop(&mut app, BackendOps::Test(&mut backend));
    assert_eq!(core.state, SAHostState::Retiring);
    core.native_destroyed(backend.complete_destruction().unwrap());
    assert_eq!(core.state, SAHostState::Closed);
}
fn spec(points: SAServicePoints) -> SAServiceSpec {
    SAServiceSpec {
        points,
        budget: SAServiceBudget { records: 3 },
        fallback_interval: Duration::from_millis(10),
    }
}

#[test]
fn nested_service_skips_active_source_restores_phase_and_retains_self_removed_record() {
    let mut core = Core::<App>::new().unwrap();
    core.clock.manual = Some(Duration::ZERO);
    let mut backend = TestBackend::default();
    let mut app = App {
        nested: true,
        remove: true,
        ..App::default()
    };
    let mut cx = SAContext::new(
        &mut core,
        BackendOps::Test(&mut backend),
        SAContextPhase::Event,
    );
    let first = cx.register_service(spec(SAServicePoints::ALL)).unwrap();
    let first_wake = cx.wake(first).unwrap();
    let second = cx.register_service(spec(SAServicePoints::ALL)).unwrap();
    app.first = Some(first);
    let visit = cx
        .service_pending(&mut app, SAServicePoint::Maintenance, 8)
        .unwrap();
    assert_eq!(visit.callbacks, 1);
    assert_eq!(app.calls, [first, second]);
    assert_eq!(cx.phase(), SAContextPhase::Event);
    assert!(cx.core.input.enabled);
    assert_eq!(cx.service_state(first), Err(SAError::StaleIdentity));
    assert_eq!(first_wake.signal(), Err(SAError::AdmissionClosed));
    let replacement = cx.register_service(spec(SAServicePoints::ALL)).unwrap();
    assert_ne!(replacement, first);
    assert_eq!(cx.remove_service(first), Err(SAError::StaleIdentity));
}

#[test]
fn budget_exhaustion_retains_round_robin_backlog_and_independent_idle_maintenance() {
    let mut core = Core::<App>::new().unwrap();
    core.clock.manual = Some(Duration::ZERO);
    let mut app = App {
        backlog: true,
        ..App::default()
    };
    let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
    let first = cx
        .register_service(spec(SAServicePoints::ORDINARY))
        .unwrap();
    let second = cx
        .register_service(spec(SAServicePoints::ORDINARY))
        .unwrap();
    assert_eq!(
        cx.service_pending(&mut app, SAServicePoint::Maintenance, 3)
            .unwrap(),
        SAServiceVisit {
            callbacks: 3,
            continuation: true
        }
    );
    assert_eq!(app.calls, [first, second, first]);
    assert_eq!(
        cx.service_pending(&mut app, SAServicePoint::Maintenance, 2)
            .unwrap()
            .callbacks,
        2
    );
    assert_eq!(&app.calls[3..], [second, first]);
    app.backlog = false;
    cx.service_pending(&mut app, SAServicePoint::Maintenance, 2)
        .unwrap();
    let before = app.calls.len();
    assert_eq!(
        cx.service_pending(&mut app, SAServicePoint::Maintenance, 2)
            .unwrap()
            .callbacks,
        0
    );
    cx.core.clock.manual = Some(Duration::from_millis(10));
    assert_eq!(
        cx.service_pending(&mut app, SAServicePoint::Maintenance, 2)
            .unwrap()
            .callbacks,
        2
    );
    assert_eq!(app.calls.len(), before + 2);
}

#[test]
fn burst_wakes_coalesce_and_signal_during_callback_survives_final_quiescent_report() {
    let mut core = Core::<App>::new().unwrap();
    core.clock.manual = Some(Duration::ZERO);
    let mut app = App::default();
    let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
    let service = cx
        .register_service(spec(SAServicePoints::ORDINARY))
        .unwrap();
    let wake = cx.wake(service).unwrap();
    cx.service_pending(&mut app, SAServicePoint::Maintenance, 1)
        .unwrap();
    let signal = wake.clone();
    std::thread::spawn(move || {
        for _ in 0..100_000 {
            signal.signal().unwrap();
        }
    })
    .join()
    .unwrap();
    assert_eq!(wake.state.posts.load(Ordering::Relaxed), 1);
    app.race = Some(wake.clone());
    let visit = cx
        .service_pending(&mut app, SAServicePoint::Maintenance, 1)
        .unwrap();
    assert!(visit.continuation);
    assert_eq!(wake.state.posts.load(Ordering::Relaxed), 2);
    assert_eq!(
        cx.service_pending(&mut app, SAServicePoint::Maintenance, 1)
            .unwrap()
            .callbacks,
        1
    );
    assert_eq!(
        cx.service_pending(&mut app, SAServicePoint::Maintenance, 1)
            .unwrap()
            .callbacks,
        0
    );
}

#[test]
fn frame_preupdate_checks_eligibility_then_rechecks_mutated_policy_and_nested_phase() {
    let mut core = Core::<App>::new().unwrap();
    core.clock.manual = Some(Duration::ZERO);
    let mut backend = TestBackend::default();
    let mut app = App::default();
    let (target, unrelated);
    {
        let mut cx = SAContext::new(
            &mut core,
            BackendOps::Test(&mut backend),
            SAContextPhase::Startup,
        );
        target = cx.create_window(SAWindowSpec::default()).unwrap();
        unrelated = cx.create_window(SAWindowSpec::default()).unwrap();
        cx.register_service(spec(SAServicePoints::only(SAServicePoint::PreUpdate)))
            .unwrap();
        cx.set_pacing(
            Some(target),
            SAPacingPolicy::Stock {
                foreground_rate: 30,
                background_rate: 15,
            },
        )
        .unwrap();
    }
    core.state = SAHostState::Running;
    core.frame(&mut app, BackendOps::Test(&mut backend), unrelated);
    assert!(app.calls.is_empty());
    core.frame(&mut app, BackendOps::Test(&mut backend), target);
    assert_eq!(app.frames, 1);
    assert_eq!(app.calls.len(), 1);
    core.clock.manual = Some(Duration::from_millis(1));
    core.frame(&mut app, BackendOps::Test(&mut backend), target);
    assert_eq!(app.calls.len(), 1);
    core.clock.manual = Some(Duration::from_millis(70));
    app.disable_frame = true;
    core.frame(&mut app, BackendOps::Test(&mut backend), target);
    assert_eq!(app.calls.len(), 2);
    assert_eq!(app.frames, 1);
}
