use crate::identity::{SAHostId, SAWindowGeneration, WindowSlots};
use crate::pacing::Pacing;
use crate::time::Clock;
use crate::*;
use std::time::Duration;

fn target(host: SAHostId) -> SAWindowTarget {
    SAWindowTarget {
        id: WindowSlots::<()>::new(host).reserve().unwrap(),
        generation: SAWindowGeneration::INITIAL,
    }
}
fn raw(host: SAHostId, millis: u64) -> SARawTime {
    SARawTime {
        host,
        elapsed: Duration::from_millis(millis),
    }
}
fn app(raw: SARawTime) -> SAApplicationTime {
    SAApplicationTime {
        host: raw.host,
        elapsed: raw.elapsed,
    }
}

#[test]
fn stock_renderer_activation_and_forever_phase_selection_remain_distinct() {
    let stock = SAPacingPolicy::Stock {
        foreground_rate: 30,
        background_rate: 120,
    };
    assert_eq!(stock.effective_rate(true), Some(30));
    assert_eq!(stock.effective_rate(false), Some(30));
    let reverse = SAPacingPolicy::Stock {
        foreground_rate: 120,
        background_rate: 30,
    };
    assert_eq!(reverse.effective_rate(true), Some(120));
    assert_eq!(reverse.effective_rate(false), Some(30));
    assert_eq!(
        SAPacingPolicy::Stock {
            foreground_rate: 0,
            background_rate: 1
        }
        .effective_rate(false),
        Some(8)
    );
    assert_eq!(
        SAPacingPolicy::Stock {
            foreground_rate: u32::MAX,
            background_rate: 0
        }
        .effective_rate(true),
        Some(u32::MAX)
    );
    assert_eq!(
        SAPacingPolicy::Stock {
            foreground_rate: 0,
            background_rate: 0
        }
        .effective_rate(false),
        None
    );
    assert_eq!(
        SAPacingPolicy::Forever {
            foreground_rate: 30,
            background_rate: 120,
            foreground: false,
            phase: SAFramePhase::Ordinary
        }
        .effective_rate(false),
        Some(120)
    );
    assert_eq!(
        SAPacingPolicy::Forever {
            foreground_rate: 0,
            background_rate: 0,
            foreground: true,
            phase: SAFramePhase::Glue
        }
        .effective_rate(true),
        Some(60)
    );
}

#[test]
fn finite_frame_requests_cannot_bypass_caps_unrelated_redraw_does_not_ack_and_late_work_is_not_added_twice()
 {
    let host = SAHostId::allocate().unwrap();
    let target = target(host);
    // A separately issued host makes a definitely unrelated redraw target.
    let unrelated = target_fn();
    let mut pacing = Pacing::new();
    pacing.set(
        Some(target),
        SAPacingPolicy::Stock {
            foreground_rate: 60,
            background_rate: 60,
        },
        raw(host, 0),
    );
    assert_eq!(pacing.arm_redraw(raw(host, 0)), Some(target));
    assert!(
        pacing
            .claim(unrelated, raw(host, 0), app(raw(host, 0)))
            .unwrap()
            .is_none()
    );
    assert_eq!(pacing.arm_redraw(raw(host, 0)), None);
    pacing
        .claim(target, raw(host, 0), app(raw(host, 0)))
        .unwrap()
        .unwrap();
    pacing.complete(raw(host, 10)).unwrap();
    assert_eq!(
        pacing.deadline().unwrap().time().elapsed(),
        Duration::from_millis(16)
    );
    pacing.request(raw(host, 11)).unwrap();
    assert!(!pacing.eligible(target, raw(host, 11)));
    assert_eq!(pacing.arm_redraw(raw(host, 11)), None);
    assert!(
        pacing
            .claim(target, raw(host, 16), app(raw(host, 16)))
            .unwrap()
            .is_some()
    );
    pacing.complete(raw(host, 40)).unwrap();
    assert!(pacing.eligible(target, raw(host, 40)));
    assert!(
        pacing
            .claim(target, raw(host, 40), app(raw(host, 40)))
            .unwrap()
            .is_some()
    );
    pacing.complete(raw(host, 40)).unwrap();
    assert_eq!(
        pacing.deadline().unwrap().time().elapsed(),
        Duration::from_millis(56)
    );
    pacing.set(Some(target), SAPacingPolicy::Disabled, raw(host, 41));
    assert!(pacing.request(raw(host, 41)).is_err());
    assert_eq!(pacing.arm_redraw(raw(host, 41)), None);
}
fn target_fn() -> SAWindowTarget {
    target(SAHostId::allocate().unwrap())
}

#[test]
fn renderer_managed_pacing_only_schedules_explicit_frames_and_counts_application_delta() {
    let host = SAHostId::allocate().unwrap();
    let target = target(host);
    let mut pacing = Pacing::new();
    pacing.set(Some(target), SAPacingPolicy::RendererManaged, raw(host, 0));
    let first = pacing
        .claim(target, raw(host, 0), app(raw(host, 0)))
        .unwrap()
        .unwrap();
    assert_eq!(first.delta, Duration::ZERO);
    pacing.complete(raw(host, 20)).unwrap();
    assert_eq!(pacing.deadline(), None);
    assert!(!pacing.eligible(target, raw(host, 100)));
    pacing.request(raw(host, 100)).unwrap();
    let second = pacing
        .claim(target, raw(host, 100), app(raw(host, 40)))
        .unwrap()
        .unwrap();
    assert_eq!(second.sequence, 2);
    assert_eq!(second.delta, Duration::from_millis(40));
    assert_eq!(second.delta_seconds(), 0.04);
}

#[test]
fn application_rescaling_is_continuous_checked_and_does_not_change_raw_deadlines() {
    let host = SAHostId::allocate().unwrap();
    let mut clock = Clock::new(host);
    clock.manual = Some(Duration::from_millis(100));
    let raw_deadline = clock
        .sample()
        .checked_add(Duration::from_millis(100))
        .unwrap();
    clock.rescale(2.0).unwrap();
    assert_eq!(
        clock.application(clock.sample()).unwrap().elapsed(),
        Duration::from_millis(100)
    );
    clock.manual = Some(Duration::from_millis(150));
    assert_eq!(
        clock.application(clock.sample()).unwrap().elapsed(),
        Duration::from_millis(200)
    );
    clock.rescale(0.0).unwrap();
    clock.manual = Some(Duration::from_millis(500));
    assert_eq!(
        clock.application(clock.sample()).unwrap().elapsed(),
        Duration::from_millis(200)
    );
    assert_eq!(raw_deadline.time().elapsed(), Duration::from_millis(200));
    for bad in [-1.0, f64::NAN, f64::INFINITY] {
        assert!(clock.rescale(bad).is_err());
    }
    assert_eq!(
        clock.application(clock.sample()).unwrap().elapsed(),
        Duration::from_millis(200)
    );
    assert_eq!(
        clock.application(raw(SAHostId::allocate().unwrap(), 500)),
        Err(SAError::ForeignHost)
    );
    let value = SAApplicationTime {
        host,
        elapsed: Duration::from_millis(u64::from(u32::MAX) + 2) + Duration::from_micros(999),
    };
    assert_eq!(value.milliseconds_wrapping(), 1);
}
