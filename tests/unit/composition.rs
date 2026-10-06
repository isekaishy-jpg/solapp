use super::*;

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
    while app.lifecycle == WorkLifecycle::Running {
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

use crate::backend::{BackendOps, test::TestBackend};
use crate::host::Core;
use std::sync::mpsc;
use std::time::Instant;

const CHILD_MODE: &str = "SOLAPP_COMPOSITION_RETIREMENT_CHILD";

// Release on every unwind path; never strand a real worker during failed proof.
struct Release(Option<mpsc::Sender<()>>);
impl Release {
    fn now(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}
impl Drop for Release {
    fn drop(&mut self) {
        self.now();
    }
}

fn request_stop(core: &mut Core<Demo<'_>>, backend: &mut TestBackend) {
    SAContext::new(core, BackendOps::Test(backend), SAContextPhase::Startup).request_stop();
}

fn finish<'scope>(
    app: &mut Demo<'scope>,
    core: &mut Core<Demo<'scope>>,
    backend: &mut TestBackend,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut millis = 100;
    while core.state == SAHostState::Stopping {
        assert!(
            Instant::now() < deadline,
            "application retirement did not settle"
        );
        millis += 10;
        core.clock.manual = Some(Duration::from_millis(millis));
        core.pump_retirement(app, BackendOps::Test(backend));
        core.poll_stop(app, BackendOps::Test(backend));
        std::thread::yield_now();
    }
    while let Some(key) = backend.complete_destruction() {
        core.native_destroyed(key);
    }
    assert_eq!(core.state, SAHostState::Closed);
    assert!(app.joined);
    assert_eq!(app.cleaned, 1);
    assert_eq!(app.cleanup.pending(), 0);
    assert_eq!(app.domain.snapshot().total_declared_bytes, 0);
}

#[test]
#[ignore = "private child; invoked only by the bounded parent test"]
fn outstanding_worker_retirement_child() {
    assert_eq!(std::env::var(CHILD_MODE).as_deref(), Ok("1"));
    crate::backend::windows::shell::suppress_child_abort_reporting();
    let cleanup = SCCleanupContext::new();
    let mut runtime = runtime();
    let mut app = Demo::new(&mut runtime, &cleanup);
    let mut core = Core::<Demo<'_>>::new().unwrap();
    let mut backend = TestBackend::default();
    core.clock.manual = Some(Duration::ZERO);
    core.start(&mut app, BackendOps::Test(&mut backend));
    assert!(core.failure.is_none());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let mut release = Release(Some(release_tx));
    let (_task, _) = app
        .runtime
        .lane(SWExecutionClass::High)
        .try_spawn(SWSpawnOptions::default(), move || {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        })
        .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    request_stop(&mut core, &mut backend);
    core.poll_stop(&mut app, BackendOps::Test(&mut backend));
    assert_eq!(
        core.state,
        SAHostState::Stopping,
        "outstanding worker must keep stop pending"
    );
    assert!(!app.joined);
    core.clock.manual = Some(Duration::from_millis(20));
    eprintln!("COMPOSITION_CLOSED_ROUTE_RETIREMENT_ENTERED");
    core.pump_retirement(&mut app, BackendOps::Test(&mut backend));
    assert!(!app.final_admitted && app.owner.state().is_empty());
    release.now();
    finish(&mut app, &mut core, &mut backend);
    eprintln!("COMPOSITION_RETIREMENT_JOINED_CLEANED_CLOSED");
}

#[test]
fn outstanding_worker_closed_route_retirement_settles_in_bounded_child() {
    use std::process::{Command, Stdio};
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "composition_tests::tests::outstanding_worker_retirement_child",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_MODE, "1")
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "composition child timed out: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("COMPOSITION_CLOSED_ROUTE_RETIREMENT_ENTERED"),
        "{stderr}"
    );
    assert!(
        output.status.success(),
        "composition child failed: {}\n{stderr}",
        output.status
    );
    assert!(
        stderr.contains("COMPOSITION_RETIREMENT_JOINED_CLEANED_CLOSED"),
        "{stderr}"
    );
    eprintln!("bounded composition child passed: {stderr}");
}

#[test]
fn first_retirement_service_closes_work_before_stopping_or_publication() {
    // Exercise either registered source as the first retirement visit.
    for clean_first in [false, true] {
        let cleanup = SCCleanupContext::new();
        let mut runtime = runtime();
        let mut app = Demo::new(&mut runtime, &cleanup);
        let mut core = Core::<Demo<'_>>::new().unwrap();
        let mut backend = TestBackend::default();
        core.clock.manual = Some(Duration::ZERO);
        core.start(&mut app, BackendOps::Test(&mut backend));
        assert!(core.failure.is_none());
        let first = if clean_first { app.clean } else { app.work }.unwrap();
        request_stop(&mut core, &mut backend);
        let request = SAServiceRequest {
            id: first,
            point: SAServicePoint::Retirement,
            budget: SAServiceBudget { records: 1 },
            raw: core.clock.sample(),
        };
        let mut cx = SAContext::new(
            &mut core,
            BackendOps::Test(&mut backend),
            SAContextPhase::Service(SAServicePoint::Retirement),
        );
        app.service(&mut cx, request);
        assert_eq!(app.lifecycle, WorkLifecycle::Draining);
        assert!(!app.final_admitted && app.owner.state().is_empty());
        assert!(
            app.backing.is_none(),
            "host retirement must release immediately"
        );
        finish(&mut app, &mut core, &mut backend);
    }
}

#[test]
fn retirement_before_stopping_uses_the_hosts_actual_service_pump() {
    let cleanup = SCCleanupContext::new();
    let mut runtime = runtime();
    let mut app = Demo::new(&mut runtime, &cleanup);
    let mut core = Core::<Demo<'_>>::new().unwrap();
    let mut backend = TestBackend::default();
    core.clock.manual = Some(Duration::ZERO);
    core.start(&mut app, BackendOps::Test(&mut backend));
    assert!(core.failure.is_none());
    request_stop(&mut core, &mut backend);
    core.pump_retirement(&mut app, BackendOps::Test(&mut backend));
    assert_eq!(app.lifecycle, WorkLifecycle::Draining);
    assert!(!app.final_admitted && app.owner.state().is_empty());
    finish(&mut app, &mut core, &mut backend);
}

#[test]
fn partial_startup_without_route_or_with_one_binding_cleans_exactly_once() {
    // Genuine startup failures: native acquisition before registration; route
    // capacity after both registrations; binding capacity after route retention.
    for stage in 0..3 {
        let cleanup = SCCleanupContext::new();
        let mut runtime = if stage == 2 {
            SWRuntime::builder(
                SWRuntimeConfig::new(
                    1,
                    [
                        SWWorkerConfig::new(0),
                        SWWorkerConfig::new(0),
                        SWWorkerConfig::new(1),
                    ],
                )
                .unwrap(),
            )
            .with_owned_limits(SWOwnedLimits::new(8, 8, [4; 3], [1; 3]).unwrap())
            .with_notification_limits(SWNotifyLimits {
                routes: 1,
                bindings: 1,
            })
            .build()
            .unwrap()
        } else {
            runtime()
        };
        let mut occupied = if stage == 1 {
            Some(runtime.notification_route(|| Ok(())).unwrap())
        } else {
            None
        };
        let mut app = Demo::new(&mut runtime, &cleanup);
        let mut core = Core::<Demo<'_>>::new().unwrap();
        let mut backend = TestBackend::default();
        if stage == 0 {
            backend.fail_title = Some(SAWindowSpec::default().title);
        }
        core.start(&mut app, BackendOps::Test(&mut backend));
        assert!(
            core.failure.is_some(),
            "startup failure not reached at stage {stage}"
        );
        assert_eq!(app.route.is_some(), stage == 2);
        assert_eq!(app.work.is_some(), stage != 0);
        assert_eq!(app.bindings.len(), usize::from(stage == 2));
        if let Some(route) = occupied.as_mut() {
            route.close().unwrap();
        }
        // Multiple pre-stopping retirement visits are safe even with no route.
        for millis in [0, 10, 20] {
            core.clock.manual = Some(Duration::from_millis(millis));
            core.pump_retirement(&mut app, BackendOps::Test(&mut backend));
        }
        finish(&mut app, &mut core, &mut backend);
        core.poll_stop(&mut app, BackendOps::Test(&mut backend));
        assert_eq!(app.cleaned, 1);
    }
}

#[test]
fn only_work_registration_and_no_route_can_retire_repeatedly() {
    let cleanup = SCCleanupContext::new();
    let mut runtime = runtime();
    let mut app = Demo::new(&mut runtime, &cleanup);
    let mut core = Core::<Demo<'_>>::new().unwrap();
    let mut backend = TestBackend::default();
    // Construct the partial registration state directly. An allocation failure
    // at the second registration can leave precisely this prefix installed.
    let mut cx = SAContext::new(
        &mut core,
        BackendOps::Test(&mut backend),
        SAContextPhase::Startup,
    );
    app.work = Some(
        cx.register_service(SAServiceSpec {
            points: SAServicePoints::ALL,
            budget: SAServiceBudget { records: 1 },
            fallback_interval: Duration::from_millis(10),
        })
        .unwrap(),
    );
    cx.core
        .fail(SAError::AllocationFailed, SAStopReason::StartupFailed);
    for millis in [0, 10, 20] {
        core.clock.manual = Some(Duration::from_millis(millis));
        core.pump_retirement(&mut app, BackendOps::Test(&mut backend));
    }
    finish(&mut app, &mut core, &mut backend);
    assert!(app.work.is_none() && app.clean.is_none() && app.route.is_none());
}

#[test]
fn claimed_notifier_keeps_wake_destination_until_actual_quiescence() {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    let cleanup = SCCleanupContext::new();
    let mut runtime = runtime();
    let mut app = Demo::new(&mut runtime, &cleanup);
    let mut core = Core::<Demo<'_>>::new().unwrap();
    let mut backend = TestBackend::default();
    core.clock.manual = Some(Duration::ZERO);
    core.start(&mut app, BackendOps::Test(&mut backend));
    assert!(core.failure.is_none());
    let work = app.work.unwrap();
    let wake = SAContext::new(
        &mut core,
        BackendOps::Test(&mut backend),
        SAContextPhase::Startup,
    )
    .wake(work)
    .unwrap();
    app.route.take().unwrap().close().unwrap();
    app.bindings.clear();
    let (claimed_tx, claimed_rx) = mpsc::channel();
    let (notifier_tx, notifier_rx) = mpsc::channel();
    let mut notifier_release = Release(Some(notifier_tx));
    let notifier_rx = Mutex::new(notifier_rx);
    let (signaled_tx, signaled_rx) = mpsc::channel();
    let armed = Arc::new(AtomicBool::new(false));
    let callback_armed = Arc::clone(&armed);
    let callback_wake = wake.clone();
    // Test-owned adapter exposes an actual SW claim. The production adapter
    // remains a nonblocking wake.signal() call; no production hook is added.
    app.route = Some(
        app.runtime
            .notification_route(move || {
                if callback_armed.load(Ordering::Acquire) {
                    claimed_tx.send(()).unwrap();
                    notifier_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .unwrap();
                    let result = callback_wake.signal();
                    signaled_tx.send(result.clone()).unwrap();
                    result.map_err(std::io::Error::other)
                } else {
                    Ok(())
                }
            })
            .unwrap(),
    );
    let (worker_tx, worker_rx) = mpsc::channel();
    let mut worker_release = Release(Some(worker_tx));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (task, _) = app
        .runtime
        .lane(SWExecutionClass::High)
        .try_spawn(SWSpawnOptions::default(), move || {
            entered_tx.send(()).unwrap();
            worker_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        })
        .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let route = app.route.as_mut().unwrap();
    app.bindings
        .push(route.watch_completion(&task.completion()).unwrap());
    route.prepare_wait().unwrap();
    armed.store(true, Ordering::Release);
    worker_release.now();
    claimed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    request_stop(&mut core, &mut backend);
    core.pump_retirement(&mut app, BackendOps::Test(&mut backend));
    core.poll_stop(&mut app, BackendOps::Test(&mut backend));
    assert_eq!(core.state, SAHostState::Stopping);
    assert_eq!(app.lifecycle, WorkLifecycle::Draining);
    assert!(!app.route.as_ref().unwrap().is_quiescent());
    assert_eq!(app.work, Some(work));
    assert!(!app.joined);
    assert_eq!(wake.signal(), Ok(()));
    assert_eq!(app.cleaned, 1);
    for millis in [10, 20, 30] {
        core.clock.manual = Some(Duration::from_millis(millis));
        core.pump_retirement(&mut app, BackendOps::Test(&mut backend));
        core.poll_stop(&mut app, BackendOps::Test(&mut backend));
        assert_eq!(app.work, Some(work));
        assert!(!app.final_admitted && app.owner.state().is_empty());
    }
    notifier_release.now();
    assert_eq!(
        signaled_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        Ok(())
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.route.as_ref().unwrap().is_quiescent() {
        assert!(Instant::now() < deadline, "notifier claim did not retire");
        std::thread::yield_now();
    }
    assert!(app.route.as_ref().unwrap().fault().is_none());
    finish(&mut app, &mut core, &mut backend);
    assert!(app.work.is_none());
    assert_eq!(wake.signal(), Err(SAError::AdmissionClosed));
}

#[test]
fn host_stop_overrides_the_normal_delayed_final_release() {
    let cleanup = SCCleanupContext::new();
    let mut runtime = runtime();
    let mut app = Demo::new(&mut runtime, &cleanup);
    let mut core = Core::<Demo<'_>>::new().unwrap();
    let mut backend = TestBackend::default();
    core.clock.manual = Some(Duration::ZERO);
    core.start(&mut app, BackendOps::Test(&mut backend));
    assert!(core.failure.is_none());
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.lifecycle == WorkLifecycle::Running {
        assert!(
            Instant::now() < deadline,
            "normal owner publication did not finish"
        );
        core.clock.manual = Some(Duration::from_millis(10));
        core.pump_ordinary(&mut app, BackendOps::Test(&mut backend));
        std::thread::yield_now();
    }
    assert_eq!(app.owner.state().as_slice(), [1, 7]);
    assert!(app.release.is_some() && app.backing.is_some());
    assert_eq!(app.cleaned, 0);
    request_stop(&mut core, &mut backend);
    // Respect the registration's next recheck while remaining before the
    // demonstration release deadline at 45ms.
    core.clock.manual = Some(Duration::from_millis(20));
    core.pump_retirement(&mut app, BackendOps::Test(&mut backend));
    assert!(app.release.is_none() && app.backing.is_none());
    assert_eq!(app.cleaned, 1);
    finish(&mut app, &mut core, &mut backend);
}
