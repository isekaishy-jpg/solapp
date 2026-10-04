use super::*;
use crate::identity::{SAWindowGeneration, WindowSlots};

fn target() -> SAWindowTarget {
    let mut slots = WindowSlots::<()>::new(SAHostId::allocate().unwrap());
    SAWindowTarget {
        id: slots.reserve().unwrap(),
        generation: SAWindowGeneration::INITIAL,
    }
}

fn placement() -> SAWindowedPlacement {
    SAWindowedPlacement {
        position: SAScreenPosition { x: -700, y: 32 },
        size: SAPhysicalSize {
            width: 640,
            height: 480,
        },
        maximized: false,
    }
}

fn observed(mode: SADisplayMode) -> SADisplayObserved {
    SADisplayObserved {
        backend_mode: mode,
        geometry: SADisplayGeometry {
            position: Some(placement().position),
            size: placement().size,
            scale: SAScaleFactor::new(1.25).unwrap(),
            minimized: Some(false),
            maximized: false,
        },
    }
}

fn monitor(host: SAHostId) -> SAMonitorSnapshot {
    let id = SAMonitorId {
        host,
        selection: MonitorSelection::Simulated(1),
    };
    SAMonitorSnapshot {
        id: id.clone(),
        name: Some(String::from("simulated monitor")),
        position: SAScreenPosition { x: 0, y: 0 },
        size: SAPhysicalSize {
            width: 1920,
            height: 1080,
        },
        scale: SAScaleFactor::new(1.0).unwrap(),
        video_modes: vec![SAVideoMode {
            monitor: id,
            native: None,
            size: SAPhysicalSize {
                width: 1920,
                height: 1080,
            },
            bit_depth: 32,
            refresh_millihertz: 59940,
        }],
    }
}

fn request(mode: SADisplayMode, host: SAHostId) -> SADisplayRequest {
    let monitor = monitor(host);
    match mode {
        SADisplayMode::Windowed => SADisplayRequest::Windowed { placement: None },
        SADisplayMode::Borderless => SADisplayRequest::Borderless {
            monitor: monitor.id,
        },
        SADisplayMode::Exclusive => SADisplayRequest::Exclusive {
            mode: monitor.video_modes[0].clone(),
        },
    }
}

#[test]
fn all_six_directed_transitions_require_exact_presentation_report() {
    let modes = [
        SADisplayMode::Windowed,
        SADisplayMode::Borderless,
        SADisplayMode::Exclusive,
    ];
    for from in modes {
        for to in modes {
            if from == to {
                continue;
            }
            let target = target();
            let mut state = DisplayState::new(target, observed(from));
            let receipt = state
                .request(request(to, target.id.host()))
                .unwrap()
                .receipt;
            assert_eq!(state.observed().backend_mode, from);
            assert_eq!(
                state.pending().unwrap().state,
                SADisplayTransitionState::Requested
            );
            let apply = state.start_next().unwrap();
            assert_eq!(apply.transition.receipt, receipt);
            assert_eq!(receipt.transition.target(), target);
            assert_eq!(
                state.active().unwrap().state,
                SADisplayTransitionState::Applying
            );
            assert_eq!(
                state.report(receipt, SAPresentationStatus::Ready),
                Err(SAError::StaleIdentity)
            );
            state.applied(receipt, observed(to)).unwrap();
            assert_eq!(
                state.active().unwrap().state,
                SADisplayTransitionState::AwaitingPresentation
            );
            assert!(state.settled().is_none());
            assert_eq!(
                state
                    .report(receipt, SAPresentationStatus::Ready)
                    .unwrap()
                    .state,
                SADisplayTransitionState::Ready
            );
            assert_eq!(state.observed().backend_mode, to);
            assert!(state.active().is_none());
        }
    }
}

#[test]
fn rapid_reversal_supersedes_only_pending_and_preserves_applying_obligations() {
    let target = target();
    let host = target.id.host();
    let mut state = DisplayState::new(target, observed(SADisplayMode::Windowed));
    let first = state
        .request(request(SADisplayMode::Borderless, host))
        .unwrap()
        .receipt;
    assert!(state.start_next().is_some());
    let second = state
        .request(request(SADisplayMode::Exclusive, host))
        .unwrap()
        .receipt;
    let reversal = state
        .request(request(SADisplayMode::Windowed, host))
        .unwrap();
    let superseded = reversal.superseded.unwrap();
    assert_eq!(superseded.receipt, second);
    assert_eq!(superseded.state, SADisplayTransitionState::Superseded);
    assert!(state.start_next().is_none());
    assert_eq!(state.active().unwrap().receipt, first);
    state
        .applied(first, observed(SADisplayMode::Borderless))
        .unwrap();
    state.report(first, SAPresentationStatus::Ready).unwrap();
    let apply = state.start_next().unwrap();
    assert_eq!(apply.transition.receipt, reversal.receipt);
    assert_eq!(apply.windowed_placement, Some(placement()));
    state
        .applied(reversal.receipt, observed(SADisplayMode::Windowed))
        .unwrap();
    assert!(state.saved_windowed.is_none());
    assert_eq!(
        state.report(first, SAPresentationStatus::Ready),
        Err(SAError::StaleIdentity)
    );
    let mut stale_revision = reversal.receipt;
    stale_revision.presentation_revision = first.presentation_revision;
    assert_eq!(
        state.report(stale_revision, SAPresentationStatus::Ready),
        Err(SAError::StaleIdentity)
    );
    state
        .report(reversal.receipt, SAPresentationStatus::Ready)
        .unwrap();
}

#[test]
fn stale_target_failure_and_retirement_do_not_complete_newer_transition() {
    let target = target();
    let mut state = DisplayState::new(target, observed(SADisplayMode::Windowed));
    let first = state
        .request(request(SADisplayMode::Borderless, target.id.host()))
        .unwrap()
        .receipt;
    state.start_next().unwrap();
    let mut wrong_target = first;
    wrong_target.transition.target = self::target();
    assert_eq!(
        state.applied(wrong_target, observed(SADisplayMode::Borderless)),
        Err(SAError::StaleIdentity)
    );
    let reason = SAError::application("simulated native acquisition failure");
    assert_eq!(
        state.fail(first, reason.clone()).unwrap().state,
        SADisplayTransitionState::Failed(reason)
    );
    assert_eq!(state.observed().backend_mode, SADisplayMode::Windowed);
    let next = state
        .request(request(SADisplayMode::Exclusive, target.id.host()))
        .unwrap()
        .receipt;
    state.start_next().unwrap();
    state
        .applied(next, observed(SADisplayMode::Exclusive))
        .unwrap();
    let pending = state
        .request(request(SADisplayMode::Windowed, target.id.host()))
        .unwrap()
        .receipt;
    assert_eq!(state.begin_retirement().unwrap().receipt, pending);
    assert!(matches!(
        state.request(request(SADisplayMode::Windowed, target.id.host())),
        Err(SAError::AdmissionClosed)
    ));
    assert_eq!(state.active().unwrap().receipt, next);
    assert_eq!(
        state.report(first, SAPresentationStatus::Ready),
        Err(SAError::StaleIdentity)
    );
    assert!(
        state
            .report(
                next,
                SAPresentationStatus::Failed(SAError::application("simulated SR failure"))
            )
            .is_ok()
    );
}

#[test]
fn selection_loss_foreign_host_and_invalid_geometry_fail_before_admission() {
    let target = target();
    let snapshot = monitor(target.id.host());
    let borderless = SADisplayRequest::Borderless {
        monitor: snapshot.id.clone(),
    };
    let exclusive = SADisplayRequest::Exclusive {
        mode: snapshot.video_modes[0].clone(),
    };
    let current = vec![snapshot.clone()];
    assert!(
        borderless
            .validate_choices(target.id.host(), &current)
            .is_ok()
    );
    assert!(
        exclusive
            .validate_choices(target.id.host(), &current)
            .is_ok()
    );
    assert_eq!(
        borderless.validate_choices(target.id.host(), &[]),
        Err(SAError::StaleIdentity)
    );
    let mut missing_mode = snapshot;
    missing_mode.video_modes.clear();
    assert_eq!(
        exclusive.validate_choices(target.id.host(), &[missing_mode]),
        Err(SAError::StaleIdentity)
    );
    let mut state = DisplayState::new(target, observed(SADisplayMode::Windowed));
    assert!(matches!(
        state.request(request(
            SADisplayMode::Borderless,
            SAHostId::allocate().unwrap()
        )),
        Err(SAError::ForeignHost)
    ));
    let invalid = SAWindowedPlacement {
        size: SAPhysicalSize {
            width: 0,
            height: 1,
        },
        ..placement()
    };
    assert!(
        state
            .request(SADisplayRequest::Windowed {
                placement: Some(invalid)
            })
            .is_err()
    );
    assert!(state.pending().is_none());
    assert_eq!(state.next_serial, 1);
    assert_eq!(
        SAScaleFactor::new(f64::NAN),
        Err(SAError::InvalidInput("scale must be positive and finite"))
    );
}

#[test]
fn monitor_loss_adjusts_saved_position_without_inventing_monitor_or_mode() {
    let snapshot = monitor(SAHostId::allocate().unwrap());
    let adjusted = placement()
        .adjusted_for_monitors(std::slice::from_ref(&snapshot))
        .unwrap();
    assert_eq!(adjusted.position, SAScreenPosition { x: 0, y: 32 });
    assert_eq!(adjusted.size, placement().size);
    assert_eq!(
        adjusted.adjusted_for_monitors(&[snapshot]).unwrap(),
        adjusted
    );
    assert_eq!(
        placement().adjusted_for_monitors(&[]),
        Err(SAError::StaleIdentity)
    );
}

#[test]
fn checked_ids_and_backend_mismatch_preserve_inflight_transition() {
    let target = target();
    let mut state = DisplayState::new(target, observed(SADisplayMode::Windowed));
    let receipt = state
        .request(request(SADisplayMode::Borderless, target.id.host()))
        .unwrap()
        .receipt;
    state.start_next().unwrap();
    state
        .applied(receipt, observed(SADisplayMode::Windowed))
        .unwrap();
    assert!(matches!(
        state.report(receipt, SAPresentationStatus::Ready),
        Err(SAError::InvalidInput(_))
    ));
    assert_eq!(state.active().unwrap().receipt, receipt);
    state.next_serial = u64::MAX;
    assert!(matches!(
        state.request(request(SADisplayMode::Windowed, target.id.host())),
        Err(SAError::IdentityExhausted(
            SAIdentityKind::DisplayTransition
        ))
    ));
    state.next_serial = 2;
    state.next_revision = u64::MAX;
    assert!(matches!(
        state.request(request(SADisplayMode::Windowed, target.id.host())),
        Err(SAError::IdentityExhausted(SAIdentityKind::Presentation))
    ));
    assert!(state.pending().is_none());
    state.observe(observed(SADisplayMode::Borderless));
    state.report(receipt, SAPresentationStatus::Ready).unwrap();
}

#[test]
fn implicit_maximized_return_preserves_backend_normal_restore_rectangle() {
    let target = target();
    let mut maximized = observed(SADisplayMode::Windowed);
    maximized.geometry.maximized = true;
    let mut state = DisplayState::new(target, maximized);
    let fullscreen = state
        .request(request(SADisplayMode::Borderless, target.id.host()))
        .unwrap()
        .receipt;
    state.start_next().unwrap();
    assert!(state.saved_windowed.unwrap().maximized);
    state
        .applied(fullscreen, observed(SADisplayMode::Borderless))
        .unwrap();
    state
        .report(fullscreen, SAPresentationStatus::Ready)
        .unwrap();
    let restore = state
        .request(request(SADisplayMode::Windowed, target.id.host()))
        .unwrap()
        .receipt;
    assert!(state.start_next().unwrap().windowed_placement.is_none());
    state.applied(restore, maximized).unwrap();
    state.report(restore, SAPresentationStatus::Ready).unwrap();
    let explicit = SAWindowedPlacement {
        maximized: true,
        ..placement()
    };
    state
        .request(SADisplayRequest::Windowed {
            placement: Some(explicit),
        })
        .unwrap();
    assert_eq!(
        state.start_next().unwrap().windowed_placement,
        Some(explicit)
    );
}
