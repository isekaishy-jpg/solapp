use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use solcache::{SCAccountingDomain, SCAllocationClass, SCBacking, SCCleanupContext};

use crate::backend::{
    BackendOps,
    test::{TestBackend, Trace},
};
use crate::context::{SAContext, SAContextPhase};
use crate::error::{SAError, SANativeOperation};
use crate::host::{Core, SAApplication, SAHostState, SAStopOutcome, SAStopProgress};
use crate::identity::SAWindowTarget;
use crate::window::{SAWindowSpec, SAWindowState};

enum StartAction {
    Stop,
    FailAcquisition,
    Panic,
    PanicPayload(Arc<AtomicBool>),
}

pub(crate) struct App {
    target: Option<SAWindowTarget>,
    action: StartAction,
    pending: usize,
    polls: usize,
    startup_returned: bool,
    unexpectedly_destroyed: bool,
}

impl App {
    fn new(action: StartAction, pending: usize) -> Self {
        Self {
            target: None,
            action,
            pending,
            polls: 0,
            startup_returned: false,
            unexpectedly_destroyed: false,
        }
    }
}

impl SAApplication for App {
    type Message = String;
    type LocalEvent = u8;
    fn started(&mut self, cx: &mut SAContext<'_, Self>) -> Result<(), SAError> {
        assert_eq!(cx.phase(), SAContextPhase::Startup);
        assert!(cx.admission().ordinary_open);
        self.target = Some(cx.create_window(SAWindowSpec::default()).unwrap());
        match &self.action {
            StartAction::Stop => {
                assert_eq!(cx.request_stop(), SAStopOutcome::Requested);
                assert_eq!(cx.request_stop(), SAStopOutcome::AlreadyRequested);
                assert_eq!(
                    cx.window_state(self.target.unwrap()),
                    Ok(SAWindowState::Retiring)
                );
                let spec = SAWindowSpec {
                    title: String::from("unaccepted"),
                    ..SAWindowSpec::default()
                };
                let (returned, reason) = cx.create_window(spec.clone()).unwrap_err().into_parts();
                assert_eq!(returned, spec);
                assert_eq!(reason, SAError::AdmissionClosed);
                self.startup_returned = true;
                Ok(())
            }
            StartAction::FailAcquisition => {
                let spec = SAWindowSpec {
                    title: String::from("fail"),
                    ..SAWindowSpec::default()
                };
                let (returned, error) = cx.create_window(spec.clone()).unwrap_err().into_parts();
                assert_eq!(returned, spec);
                self.startup_returned = true;
                Err(error)
            }
            StartAction::Panic => panic!("startup panic after native acquisition"),
            StartAction::PanicPayload(dropped) => {
                std::panic::panic_any(DropPanics(Arc::clone(dropped)))
            }
        }
    }

    fn stopping(&mut self, cx: &mut SAContext<'_, Self>) -> SAStopProgress {
        if matches!(
            self.action,
            StartAction::Stop | StartAction::FailAcquisition
        ) {
            assert!(self.startup_returned);
        }
        assert_eq!(cx.phase(), SAContextPhase::Retirement);
        assert!(!cx.admission().ordinary_open);
        assert!(cx.admission().retirement_open);
        assert_eq!(
            cx.window_state(self.target.unwrap()),
            if self.unexpectedly_destroyed {
                Err(SAError::StaleIdentity)
            } else {
                Ok(SAWindowState::Retiring)
            }
        );
        assert_eq!(
            cx.create_window(SAWindowSpec::default())
                .unwrap_err()
                .reason(),
            &SAError::AdmissionClosed
        );
        self.polls += 1;
        if self.polls <= self.pending {
            SAStopProgress::Pending
        } else {
            SAStopProgress::Settled
        }
    }
}

struct DropPanics(Arc<AtomicBool>);
impl Drop for DropPanics {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
        panic!("panic payload destructor");
    }
}

fn complete_native_retirement(core: &mut Core<App>, backend: &mut TestBackend) {
    assert_eq!(core.state, SAHostState::Retiring);
    let key = backend
        .complete_destruction()
        .expect("pending native destruction");
    core.native_destroyed(key);
    assert_eq!(core.state, SAHostState::Closed);
}

#[test]
fn stop_latches_without_reentry_and_waits_for_application_and_native_retirement() {
    let mut core = Core::new().unwrap();
    let host = core.id;
    let mut backend = TestBackend::default();
    let mut app = App::new(StartAction::Stop, 2);
    core.start(&mut app, BackendOps::Test(&mut backend));
    assert_eq!(core.id, host);
    assert_eq!(app.polls, 0);
    core.start(&mut app, BackendOps::Test(&mut backend));
    for _ in 0..2 {
        core.poll_stop(&mut app, BackendOps::Test(&mut backend));
        assert_eq!(core.state, SAHostState::Stopping);
        assert_eq!(
            backend.trace.borrow().as_slice(),
            &[Trace::Created(String::from("Solapp"))]
        );
    }
    core.poll_stop(&mut app, BackendOps::Test(&mut backend));
    assert_eq!(app.polls, 3);
    assert_eq!(
        backend.trace.borrow().last(),
        Some(&Trace::DestroyRequested(String::from("Solapp")))
    );
    assert_eq!(core.state, SAHostState::Retiring);
    // A request and an extra visit cannot fabricate native destruction.
    core.poll_stop(&mut app, BackendOps::Test(&mut backend));
    assert_eq!(app.polls, 3);
    complete_native_retirement(&mut core, &mut backend);
    assert_eq!(core.id, host);
}

#[test]
fn partial_native_acquisition_failure_keeps_successful_window_until_cleanup_settles() {
    let mut core = Core::new().unwrap();
    let mut backend = TestBackend::default();
    backend.fail_title = Some(String::from("fail"));
    let mut app = App::new(StartAction::FailAcquisition, 1);
    core.start(&mut app, BackendOps::Test(&mut backend));
    assert!(matches!(
        core.failure,
        Some(SAError::Native {
            operation: SANativeOperation::CreateWindow,
            ..
        })
    ));
    core.poll_stop(&mut app, BackendOps::Test(&mut backend));
    assert_eq!(core.native_windows.len(), 1);
    assert_eq!(core.state, SAHostState::Stopping);
    core.poll_stop(&mut app, BackendOps::Test(&mut backend));
    complete_native_retirement(&mut core, &mut backend);
    assert!(core.failure.is_some());
}

#[test]
fn startup_panics_retire_acquisitions_even_when_payload_destructor_would_panic() {
    for action in [
        StartAction::Panic,
        StartAction::PanicPayload(Arc::new(AtomicBool::new(false))),
    ] {
        let mut core = Core::new().unwrap();
        let mut backend = TestBackend::default();
        let mut app = App::new(action, 0);
        core.start(&mut app, BackendOps::Test(&mut backend));
        assert!(matches!(
            core.failure,
            Some(SAError::ApplicationPanicked {
                phase: SAContextPhase::Startup,
                ..
            })
        ));
        core.poll_stop(&mut app, BackendOps::Test(&mut backend));
        complete_native_retirement(&mut core, &mut backend);
        if let StartAction::PanicPayload(dropped) = &app.action {
            assert!(!dropped.load(Ordering::Relaxed));
        }
    }
}

#[test]
fn unexpected_destruction_during_stopping_invalidates_generation_without_second_native_access() {
    let mut core = Core::new().unwrap();
    let mut backend = TestBackend::default();
    let mut app = App::new(StartAction::Stop, 1);
    core.start(&mut app, BackendOps::Test(&mut backend));
    core.native_destroyed(core.native_windows[0].0);
    app.unexpectedly_destroyed = true;
    assert!(core.failure.is_some());
    core.poll_stop(&mut app, BackendOps::Test(&mut backend));
    core.poll_stop(&mut app, BackendOps::Test(&mut backend));
    assert_eq!(core.state, SAHostState::Closed);
    assert!(backend.complete_destruction().is_none());
    assert_eq!(backend.trace.borrow().len(), 1);
}

#[test]
fn invalid_input_and_invalid_context_return_original_spec_before_acquisition() {
    let mut core = Core::new().unwrap();
    let mut backend = TestBackend::default();
    let mut cx = SAContext::<App>::new(
        &mut core,
        BackendOps::Test(&mut backend),
        SAContextPhase::Startup,
    );
    for spec in [
        SAWindowSpec {
            title: String::from("bad\0title"),
            ..SAWindowSpec::default()
        },
        SAWindowSpec {
            size: crate::SAPhysicalSize {
                width: 0,
                height: 1,
            },
            ..SAWindowSpec::default()
        },
        SAWindowSpec {
            size: crate::SAPhysicalSize {
                width: u32::MAX,
                height: 1,
            },
            ..SAWindowSpec::default()
        },
    ] {
        let (returned, reason) = cx.create_window(spec.clone()).unwrap_err().into_parts();
        assert_eq!(returned, spec);
        assert!(matches!(reason, SAError::InvalidInput(_)));
    }
    let mut cx = SAContext::<App>::new(
        &mut core,
        BackendOps::Test(&mut backend),
        SAContextPhase::Retirement,
    );
    assert_eq!(
        cx.create_window(SAWindowSpec::default())
            .unwrap_err()
            .reason(),
        &SAError::InvalidContext(SAContextPhase::Retirement)
    );
    assert!(backend.trace.borrow().is_empty());
}

#[test]
fn borrowed_non_send_application_drains_actual_externally_scoped_sc_cleanup() {
    struct BorrowedApp<'scope> {
        cleanup: &'scope SCCleanupContext<Rc<Cell<usize>>>,
        backing: Option<SCBacking<'scope, Rc<Cell<usize>>>>,
        local: Rc<Cell<usize>>,
    }
    impl SAApplication for BorrowedApp<'_> {
        type Message = String;
        type LocalEvent = u8;
        fn started(&mut self, cx: &mut SAContext<'_, Self>) -> Result<(), SAError> {
            self.local.set(1);
            drop(self.backing.take());
            cx.request_stop();
            Err(SAError::application("partial domain startup"))
        }
        fn stopping(&mut self, _: &mut SAContext<'_, Self>) -> SAStopProgress {
            assert_eq!(self.cleanup.drain_budget(1), 1);
            self.local.set(2);
            SAStopProgress::Settled
        }
    }
    let cleanup = SCCleanupContext::new();
    let local = Rc::new(Cell::new(0));
    let domain = SCAccountingDomain::new();
    let backing = cleanup
        .try_acquire(Rc::clone(&local), &domain, 1, SCAllocationClass::Resident)
        .unwrap();
    let mut app = BorrowedApp {
        cleanup: &cleanup,
        backing: Some(backing),
        local: Rc::clone(&local),
    };
    let mut core = Core::new().unwrap();
    let mut backend = TestBackend::default();
    core.start(&mut app, BackendOps::Test(&mut backend));
    assert_eq!(cleanup.pending(), 1);
    core.poll_stop(&mut app, BackendOps::Test(&mut backend));
    assert_eq!(core.state, SAHostState::Closed);
    assert_eq!(cleanup.pending(), 0);
    assert_eq!(local.get(), 2);
    assert_eq!(domain.snapshot().total_declared_bytes, 0);
}
