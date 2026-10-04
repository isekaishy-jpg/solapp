//! Shutdown observations are not application/provider completion proofs.

use std::time::Duration;

use crate::backend::{BackendOps, test::TestBackend};
use crate::host::Core;
use crate::*;

#[derive(Default)]
struct App {
    target: Option<SAWindowTarget>,
    snapshots: Vec<SAShutdownSnapshot>,
    receipts: Vec<SAPostReceipt>,
    stops: usize,
}

impl SAApplication for App {
    type Message = ();
    type LocalEvent = ();

    fn started(&mut self, cx: &mut SAContext<'_, Self>) -> Result<(), SAError> {
        self.snapshots.push(cx.shutdown_snapshot());
        let target = cx.create_window(SAWindowSpec::default()).unwrap();
        self.target = Some(target);
        let recipient = cx.create_recipient().unwrap();
        cx.subscribe(
            recipient,
            SAEventFilter::All,
            SAPriority::default(),
            handler,
        )
        .unwrap();
        for _ in 0..2 {
            self.receipts
                .push(cx.proxy().try_post(recipient, ()).unwrap());
        }
        cx.register_service(SAServiceSpec {
            points: SAServicePoints::ALL,
            budget: SAServiceBudget { records: 1 },
            fallback_interval: Duration::from_millis(10),
        })
        .unwrap();
        let now = cx.clock().raw;
        cx.schedule_timer(recipient, now.checked_add(Duration::ZERO).unwrap(), ())
            .unwrap();
        cx.schedule_timer(
            recipient,
            now.checked_add(Duration::from_secs(1)).unwrap(),
            (),
        )
        .unwrap();
        cx.core.input.enabled = false;
        cx.receive_input(
            self,
            target,
            SAInputEvent::PointerMoved(SAPhysicalPosition { x: 3.0, y: 7.0 }),
            SAInputOrigin::NativeWindow,
            None,
        )
        .unwrap();
        cx.core.input.enabled = true;
        cx.service_timers(self, 1).unwrap();
        Ok(())
    }

    fn service(
        &mut self,
        cx: &mut SAContext<'_, Self>,
        request: SAServiceRequest,
    ) -> SAServiceReport {
        cx.remove_service(request.id).unwrap();
        self.snapshots.push(cx.shutdown_snapshot());
        SAServiceReport::Quiescent
    }

    fn stopping(&mut self, cx: &mut SAContext<'_, Self>) -> SAStopProgress {
        self.stops += 1;
        self.snapshots.push(cx.shutdown_snapshot());
        if self.stops == 1 {
            SAStopProgress::Pending
        } else {
            SAStopProgress::Settled
        }
    }
}

fn handler(
    app: &mut App,
    cx: &mut SAContext<'_, App>,
    event: &SAEvent<'_, (), ()>,
) -> SAPropagation {
    if matches!(event, SAEvent::Timer { .. }) {
        cx.service_pending(app, SAServicePoint::Maintenance, 1)
            .unwrap();
    } else if matches!(event, SAEvent::Posted { .. }) {
        app.snapshots.push(cx.shutdown_snapshot());
    }
    SAPropagation::Continue
}

#[test]
fn snapshots_separate_nested_active_work_application_settlement_and_native_retirement() {
    let mut core = Core::<App>::new().unwrap();
    core.clock.manual = Some(Duration::ZERO);
    let mut backend = TestBackend::default();
    let mut app = App::default();
    core.start(&mut app, BackendOps::Test(&mut backend));
    assert_eq!(app.snapshots[0].active_callbacks, 1);
    let nested = app.snapshots[1];
    assert_eq!(nested.active_callbacks, 3); // startup -> timer -> removed service
    assert_eq!(nested.active_services, 1);
    assert_eq!(nested.retirement_services, 0);
    assert_eq!(nested.accepted_posts, 2);
    assert_eq!(nested.pending_timers, 1);
    assert_eq!(nested.claimed_timers, 1);
    assert_eq!(nested.queued_input, 1);
    assert_eq!(core.shutdown_snapshot().active_callbacks, 0);

    let lease;
    {
        let mut cx = SAContext::new(
            &mut core,
            BackendOps::Test(&mut backend),
            SAContextPhase::Event,
        );
        lease = cx.acquire_window(app.target.unwrap()).unwrap();
        cx.drain_posts(&mut app, 2).unwrap();
        assert_eq!(app.snapshots[2].accepted_posts, 2); // detached + active retained
        assert_eq!(app.snapshots[3].accepted_posts, 1);
        assert_eq!(cx.shutdown_snapshot().accepted_posts, 0);
        cx.request_stop();
    }
    backend.fail_raw = true;
    core.poll_stop(&mut app, BackendOps::Test(&mut backend));
    let pending = core.shutdown_snapshot();
    assert!(!pending.ordinary_open);
    assert!(!pending.application_settled);
    assert!(pending.native_input_pending);
    assert_eq!(pending.pending_timers, 0);
    assert_eq!(pending.queued_input, 0);
    assert_eq!(app.snapshots.last().unwrap().active_callbacks, 1);
    core.poll_stop(&mut app, BackendOps::Test(&mut backend));
    let settled = core.shutdown_snapshot();
    assert!(settled.application_settled);
    assert_eq!(settled.state, SAHostState::Stopping);
    assert_eq!(settled.external_window_leases, 1);
    assert_eq!(settled.retained_window_roots, 1);
    assert_eq!(settled.pending_native_destructions, 0);
    assert!(
        SAContext::new(
            &mut core,
            BackendOps::Unavailable,
            SAContextPhase::Retirement
        )
        .fault()
        .is_some()
    );

    backend.fail_raw = false;
    core.poll_stop(&mut app, BackendOps::Test(&mut backend));
    assert_eq!(core.shutdown_snapshot().state, SAHostState::Retiring);
    assert_eq!(app.stops, 2);
    std::thread::spawn(move || drop(lease)).join().unwrap();
    core.poll_stop(&mut app, BackendOps::Test(&mut backend));
    let awaiting_native = core.shutdown_snapshot();
    assert_eq!(awaiting_native.retained_window_roots, 0);
    assert_eq!(awaiting_native.external_window_leases, 0);
    assert_eq!(awaiting_native.pending_native_destructions, 1);
    assert!(!awaiting_native.native_input_pending);
    assert_eq!(awaiting_native.active_callbacks, 0);
    core.native_destroyed(backend.complete_destruction().unwrap());
    let closed = core.shutdown_snapshot();
    assert_eq!(closed.state, SAHostState::Closed);
    assert_eq!(closed.pending_native_destructions, 0);
    assert_eq!(closed.accepted_posts, 0);
    assert_eq!(closed.claimed_timers, 0);
    assert_eq!(app.stops, 2);
}

#[test]
fn callback_unwind_restores_observation_before_required_stopping() {
    struct Panics;
    impl SAApplication for Panics {
        type Message = ();
        type LocalEvent = ();
        fn started(&mut self, cx: &mut SAContext<'_, Self>) -> Result<(), SAError> {
            assert_eq!(cx.shutdown_snapshot().active_callbacks, 1);
            panic!("startup fixture");
        }
        fn stopping(&mut self, cx: &mut SAContext<'_, Self>) -> SAStopProgress {
            assert_eq!(cx.shutdown_snapshot().active_callbacks, 1);
            assert!(cx.fault().is_some());
            SAStopProgress::Settled
        }
    }
    let mut core = Core::<Panics>::new().unwrap();
    let mut backend = TestBackend::default();
    core.start(&mut Panics, BackendOps::Test(&mut backend));
    assert_eq!(core.shutdown_snapshot().active_callbacks, 0);
    core.poll_stop(&mut Panics, BackendOps::Test(&mut backend));
    assert_eq!(core.shutdown_snapshot().active_callbacks, 0);
    assert_eq!(core.state, SAHostState::Closed);
}
