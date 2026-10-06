use crate::backend::{BackendOps, test::TestBackend};
use crate::host::Core;
use crate::*;

#[derive(Default)]
struct App {
    events: Vec<SADisplayTransition>,
    ready: bool,
}
impl SAApplication for App {
    type Message = ();
    type LocalEvent = ();
    fn started(&mut self, _: &mut SAContext<'_, Self>) -> Result<(), SAError> {
        Ok(())
    }
    fn stopping(&mut self, _: &mut SAContext<'_, Self>) -> SAStopProgress {
        SAStopProgress::Settled
    }
    fn display_transition(&mut self, cx: &mut SAContext<'_, Self>, event: &SADisplayTransition) {
        assert_eq!(cx.phase(), SAContextPhase::Display);
        assert!(!cx.core.input.enabled);
        self.events.push(event.clone());
        if self.ready && event.state == SADisplayTransitionState::AwaitingPresentation {
            cx.report_presentation(event.receipt, SAPresentationStatus::Ready)
                .unwrap();
        }
    }
}
fn window(core: &mut Core<App>, native: &mut TestBackend) -> SAWindowTarget {
    SAContext::new(core, BackendOps::Test(native), SAContextPhase::Event)
        .create_window(SAWindowSpec::default())
        .unwrap()
}
fn request(
    core: &mut Core<App>,
    native: &mut TestBackend,
    target: SAWindowTarget,
    request: SADisplayRequest,
) -> SADisplayReceipt {
    SAContext::new(core, BackendOps::Test(native), SAContextPhase::Event)
        .request_display(target, request)
        .unwrap()
}

#[test]
fn integrated_driver_all_six_simulated_modes_emit_owned_progress_and_exact_ready() {
    let mut core = Core::<App>::new().unwrap();
    let mut native = TestBackend::default();
    let mut app = App {
        ready: true,
        ..App::default()
    };
    core.start(&mut app, BackendOps::Test(&mut native));
    let target = window(&mut core, &mut native);
    let monitors = SAContext::new(
        &mut core,
        BackendOps::Test(&mut native),
        SAContextPhase::Event,
    )
    .monitors(target)
    .unwrap();
    let modes = [
        SADisplayRequest::Windowed { placement: None },
        SADisplayRequest::Borderless {
            monitor: monitors[0].id.clone(),
        },
        SADisplayRequest::Exclusive {
            mode: monitors[0].video_modes[0].clone(),
        },
    ];
    for from in 0..3 {
        for to in 0..3 {
            if from == to {
                continue;
            }
            request(&mut core, &mut native, target, modes[from].clone());
            core.pump_display(&mut app, BackendOps::Test(&mut native));
            let receipt = request(&mut core, &mut native, target, modes[to].clone());
            core.pump_display(&mut app, BackendOps::Test(&mut native));
            let states: Vec<_> = app
                .events
                .iter()
                .filter(|event| event.receipt == receipt)
                .map(|event| event.state.clone())
                .collect();
            assert_eq!(
                states,
                [
                    SADisplayTransitionState::Requested,
                    SADisplayTransitionState::Applying,
                    SADisplayTransitionState::AwaitingPresentation,
                    SADisplayTransitionState::Ready
                ]
            );
            let mut cx = SAContext::new(
                &mut core,
                BackendOps::Test(&mut native),
                SAContextPhase::Event,
            );
            assert_eq!(
                cx.display_observed(target).unwrap().backend_mode,
                modes[to].mode()
            );
            assert_eq!(
                cx.report_presentation(receipt, SAPresentationStatus::Ready),
                Err(SAError::StaleIdentity)
            );
            assert_eq!(
                target.generation,
                cx.core.windows.get(target.id).unwrap().generation
            );
        }
    }
}

#[test]
fn pending_reversal_partial_failure_and_close_preserve_terminal_statuses_and_real_leases() {
    let mut core = Core::<App>::new().unwrap();
    let mut native = TestBackend::default();
    let mut app = App::default();
    core.start(&mut app, BackendOps::Test(&mut native));
    let target = window(&mut core, &mut native);
    let lease = SAContext::new(
        &mut core,
        BackendOps::Test(&mut native),
        SAContextPhase::Event,
    )
    .acquire_window(target)
    .unwrap();
    let initial = request(
        &mut core,
        &mut native,
        target,
        SADisplayRequest::Windowed { placement: None },
    );
    core.pump_display(&mut app, BackendOps::Test(&mut native));
    let pending = request(
        &mut core,
        &mut native,
        target,
        SADisplayRequest::Windowed { placement: None },
    );
    let final_request = request(
        &mut core,
        &mut native,
        target,
        SADisplayRequest::Windowed { placement: None },
    );
    core.pump_display(&mut app, BackendOps::Test(&mut native));
    assert!(app.events.iter().any(
        |event| event.receipt == pending && event.state == SADisplayTransitionState::Superseded
    ));
    assert_eq!(
        core.windows
            .get(target.id)
            .unwrap()
            .display
            .active()
            .unwrap()
            .receipt,
        initial
    );
    {
        let mut cx = SAContext::new(
            &mut core,
            BackendOps::Test(&mut native),
            SAContextPhase::Event,
        );
        cx.report_presentation(
            initial,
            SAPresentationStatus::Failed(SAError::application("simulated SR retirement")),
        )
        .unwrap();
    }
    if let crate::backend::NativeWindow::Test(window) = &core.windows.get(target.id).unwrap().native
    {
        window.fail_display.set(true);
    }
    core.pump_display(&mut app, BackendOps::Test(&mut native));
    assert!(app.events.iter().any(|event| event.receipt == final_request
        && matches!(
            event.state,
            SADisplayTransitionState::Failed(SAError::Native { .. })
        )));
    assert!(core.ordinary_open());
    {
        let mut cx = SAContext::new(
            &mut core,
            BackendOps::Test(&mut native),
            SAContextPhase::Event,
        );
        cx.request_close(target).unwrap();
        assert!(matches!(
            cx.acquire_window(target),
            Err(SAError::AdmissionClosed)
        ));
        assert_eq!(
            cx.begin_text(
                target,
                SATextCaret {
                    position: SAPhysicalPosition { x: 0.0, y: 0.0 },
                    size: SAPhysicalSize {
                        width: 1,
                        height: 1
                    }
                }
            ),
            Err(SAError::AdmissionClosed)
        );
    }
    core.reap_closing_windows(BackendOps::Test(&mut native));
    assert!(core.windows.get(target.id).is_ok() && native.complete_destruction().is_none());
    std::thread::spawn(move || drop(lease)).join().unwrap();
    core.reap_closing_windows(BackendOps::Test(&mut native));
    assert_eq!(
        SAContext::new(
            &mut core,
            BackendOps::Test(&mut native),
            SAContextPhase::Event
        )
        .window_state(target),
        Ok(SAWindowState::Retiring)
    );
    core.native_destroyed(native.complete_destruction().unwrap());
    assert_eq!(
        SAContext::new(
            &mut core,
            BackendOps::Test(&mut native),
            SAContextPhase::Event
        )
        .window_state(target),
        Err(SAError::StaleIdentity)
    );
    assert_eq!(core.state, SAHostState::Running);
}

#[test]
fn repeated_native_key_reuse_keeps_old_acknowledgments_in_acquisition_order() {
    let mut core = Core::<App>::new().unwrap();
    let mut native = TestBackend::default();
    let mut app = App::default();
    core.start(&mut app, BackendOps::Test(&mut native));
    let first = window(&mut core, &mut native);
    let key = core.windows.get(first.id).unwrap().native.key();
    SAContext::new(
        &mut core,
        BackendOps::Test(&mut native),
        SAContextPhase::Event,
    )
    .request_close(first)
    .unwrap();
    core.reap_closing_windows(BackendOps::Test(&mut native));
    let second = window(&mut core, &mut native);
    let third = window(&mut core, &mut native);
    // Model three buffered native acquisitions reusing one opaque native key;
    // this fixture changes no native handle or operating-system state.
    for target in [second, third] {
        if let crate::backend::NativeWindow::Test(window) =
            &mut core.windows.get_mut(target.id).unwrap().native
        {
            let crate::backend::NativeWindowKey::Test(id) = key else {
                unreachable!()
            };
            window.id = id;
        }
        core.native_windows
            .iter_mut()
            .find(|(_, current)| *current == target)
            .unwrap()
            .0 = key;
    }
    SAContext::new(
        &mut core,
        BackendOps::Test(&mut native),
        SAContextPhase::Event,
    )
    .request_close(second)
    .unwrap();
    core.reap_closing_windows(BackendOps::Test(&mut native));
    core.native_destroyed(key);
    assert_eq!(core.native_windows[0].1, second);
    core.native_destroyed(key);
    assert!(core.windows.get(third.id).is_ok());
    assert!(core.failure.is_none());
    assert_eq!(core.native_windows[0].1, third);
}

#[test]
fn failed_observation_destruction_keeps_new_live_window_and_access_valid() {
    let mut core = Core::<App>::new().unwrap();
    let mut native = TestBackend::default();
    let mut app = App::default();
    core.start(&mut app, BackendOps::Test(&mut native));
    native.fail_next_observation = true;
    let spec = SAWindowSpec {
        title: String::from("failed observation"),
        visible: false,
        size: SAPhysicalSize {
            width: 317,
            height: 211,
        },
    };
    let title_pointer = spec.title.as_ptr();
    let expected = spec.clone();
    let (returned, error) = SAContext::new(
        &mut core,
        BackendOps::Test(&mut native),
        SAContextPhase::Event,
    )
    .create_window(spec)
    .unwrap_err()
    .into_parts();
    assert_eq!(returned, expected);
    assert_eq!(returned.title.as_ptr(), title_pointer);
    assert_eq!(error, SAScaleFactor::new(f64::NAN).unwrap_err());
    let failed = core.native_windows[0].1;
    assert!(core.windows.get(failed.id).is_err());
    let current = window(&mut core, &mut native);
    let access = SAContext::new(
        &mut core,
        BackendOps::Test(&mut native),
        SAContextPhase::Event,
    )
    .acquire_window(current)
    .unwrap();
    core.native_destroyed(native.complete_destruction().unwrap());
    assert!(access.is_native_alive());
    assert_eq!(
        SAContext::new(
            &mut core,
            BackendOps::Test(&mut native),
            SAContextPhase::Event,
        )
        .window_state(current),
        Ok(SAWindowState::Live)
    );
    assert!(core.failure.is_none());
    drop(access);
}

#[test]
fn failed_initialization_and_successful_window_acknowledge_in_either_order() {
    use crate::backend::test::Trace;
    for failed_first in [true, false] {
        let mut core = Core::<App>::new().unwrap();
        let mut native = TestBackend::default();
        let mut app = App::default();
        core.start(&mut app, BackendOps::Test(&mut native));
        native.fail_next_observation = true;
        SAContext::new(
            &mut core,
            BackendOps::Test(&mut native),
            SAContextPhase::Event,
        )
        .create_window(SAWindowSpec {
            title: String::from("A"),
            ..SAWindowSpec::default()
        })
        .unwrap_err();
        let (failed_key, failed) = core.native_windows[0];
        let current = window(&mut core, &mut native);
        assert_ne!(failed.id, current.id);
        let current_key = core.windows.get(current.id).unwrap().native.key();
        SAContext::new(
            &mut core,
            BackendOps::Test(&mut native),
            SAContextPhase::Event,
        )
        .request_close(current)
        .unwrap();
        core.reap_closing_windows(BackendOps::Test(&mut native));
        let order = if failed_first {
            [failed_key, current_key]
        } else {
            [current_key, failed_key]
        };
        for (index, key) in order.into_iter().enumerate() {
            core.native_destroyed(native.complete_destruction_of(key).unwrap());
            assert_eq!(core.native_windows.len(), 1 - index);
            assert_eq!(
                core.shutdown_snapshot().pending_native_destructions,
                1 - index
            );
            assert!(core.failure.is_none());
        }
        assert!(native.complete_destruction().is_none());
        assert_eq!(
            native
                .trace
                .borrow()
                .iter()
                .filter(|event| matches!(event, Trace::DestroyRequested(_)))
                .count(),
            2
        );
        let replacement = window(&mut core, &mut native);
        assert_ne!(replacement.id, failed.id);
        assert!(core.windows.get(failed.id).is_err());
    }
}

#[test]
fn stop_with_only_failed_initialization_waits_for_actual_native_acknowledgement() {
    struct FailedStartup;
    impl SAApplication for FailedStartup {
        type Message = ();
        type LocalEvent = ();
        fn started(&mut self, cx: &mut SAContext<'_, Self>) -> Result<(), SAError> {
            cx.create_window(SAWindowSpec::default())
                .map(|_| ())
                .map_err(|rejected| rejected.into_parts().1)
        }
        fn stopping(&mut self, _: &mut SAContext<'_, Self>) -> SAStopProgress {
            SAStopProgress::Settled
        }
    }
    let mut core = Core::<FailedStartup>::new().unwrap();
    let mut native = TestBackend::default();
    native.fail_next_observation = true;
    let mut app = FailedStartup;
    core.start(&mut app, BackendOps::Test(&mut native));
    assert_eq!(
        core.failure,
        Some(SAScaleFactor::new(f64::NAN).unwrap_err())
    );
    assert_eq!(core.state, SAHostState::Stopping);
    for _ in 0..3 {
        core.poll_stop(&mut app, BackendOps::Test(&mut native));
        let snapshot = core.shutdown_snapshot();
        assert_eq!(snapshot.state, SAHostState::Retiring);
        assert!(snapshot.application_settled);
        assert_eq!(snapshot.retained_window_roots, 0);
        assert_eq!(snapshot.external_window_leases, 0);
        assert_eq!(snapshot.pending_native_destructions, 1);
    }
    core.native_destroyed(native.complete_destruction().unwrap());
    assert_eq!(core.state, SAHostState::Closed);
    assert_eq!(core.shutdown_snapshot().pending_native_destructions, 0);
    assert!(native.complete_destruction().is_none());
    let trace = native.trace.borrow();
    assert_eq!(trace.len(), 3);
}

#[test]
fn inconsistent_native_key_fault_retains_other_live_window_for_controlled_retirement() {
    let mut core = Core::<App>::new().unwrap();
    let mut native = TestBackend::default();
    let mut app = App::default();
    core.start(&mut app, BackendOps::Test(&mut native));
    native.fail_next_observation = true;
    SAContext::new(
        &mut core,
        BackendOps::Test(&mut native),
        SAContextPhase::Event,
    )
    .create_window(SAWindowSpec::default())
    .unwrap_err();
    let failed = core.native_windows[0].1;
    let current = window(&mut core, &mut native);
    let access = SAContext::new(
        &mut core,
        BackendOps::Test(&mut native),
        SAContextPhase::Event,
    )
    .acquire_window(current)
    .unwrap();
    let actual_key = core.windows.get(current.id).unwrap().native.key();
    // Corrupt only the old acquisition association. The live record and its own
    // native ledger entry remain the actual current acquisition.
    core.native_windows[0].1 = current;
    core.native_destroyed(native.complete_destruction().unwrap());
    assert!(core.failure.is_some());
    assert_eq!(core.stop_reason, Some(SAStopReason::BackendFailed));
    assert!(core.windows.get(current.id).is_ok());
    assert!(access.is_native_alive());
    assert_eq!(core.native_windows, [(actual_key, current)]);
    core.poll_stop(&mut app, BackendOps::Test(&mut native));
    assert_eq!(core.state, SAHostState::Retiring);
    assert!(native.complete_destruction().is_none());
    drop(access);
    core.reap_windows();
    let key = native.complete_destruction().unwrap();
    assert_eq!(key, actual_key);
    core.native_destroyed(key);
    assert_eq!(core.state, SAHostState::Closed);
    // The intentionally lost association is private corruption, not authority
    // to retire that reservation as if a matching acknowledgement existed.
    assert!(core.windows.get(failed.id).is_err());
}
