use super::*;

#[test]
fn reused_slot_rejects_old_identity_and_foreign_host() {
    let host = SAHostId::allocate().unwrap();
    let mut slots = WindowSlots::new(host);
    let old = slots.reserve().unwrap();
    slots.insert(old, 1);
    slots.retire_all();
    let current = slots.reserve().unwrap();
    slots.insert(current, 2);
    assert_eq!(old.slot, current.slot);
    assert_ne!(old, current);
    assert_eq!(slots.get(old), Err(SAError::StaleIdentity));
    assert_eq!(slots.get(current), Ok(&2));
    let foreign = SAWindowId {
        host: SAHostId::allocate().unwrap(),
        ..current
    };
    assert_eq!(slots.get(foreign), Err(SAError::ForeignHost));
}

#[test]
fn exhausted_slot_is_permanently_retired() {
    let mut slots = WindowSlots::new(SAHostId::allocate().unwrap());
    let mut last = slots.reserve().unwrap();
    slots.slots[0].incarnation = Some(u64::MAX);
    last.incarnation = u64::MAX;
    slots.insert(last, 1);
    slots.retire_all();
    let next = slots.reserve().unwrap();
    slots.insert(next, 2);
    assert_ne!(last.slot, next.slot);
    assert_eq!(slots.get(last), Err(SAError::StaleIdentity));
}

#[test]
fn native_generation_is_checked_separately_from_slot_identity() {
    use crate::backend::{BackendOps, test::TestBackend};
    use crate::context::{SAContext, SAContextPhase};
    use crate::foundation_tests::App;
    use crate::host::Core;
    use crate::window::SAWindowSpec;
    let mut core = Core::new().unwrap();
    let mut backend = TestBackend::default();
    let mut cx = SAContext::<App>::new(
        &mut core,
        BackendOps::Test(&mut backend),
        SAContextPhase::Startup,
    );
    let current = cx.create_window(SAWindowSpec::default()).unwrap();
    let stale = SAWindowTarget {
        generation: SAWindowGeneration(2),
        ..current
    };
    assert_eq!(cx.window_state(stale), Err(SAError::StaleIdentity));
}

#[test]
fn reservations_are_private_exclusive_and_release_only_the_exact_incarnation() {
    let mut slots = WindowSlots::<u8>::new(SAHostId::allocate().unwrap());
    let failed = slots.reserve().unwrap();
    assert_eq!(slots.get(failed), Err(SAError::StaleIdentity));
    assert_eq!(slots.iter().count(), 0);
    assert_eq!(slots.iter_mut().count(), 0);
    assert!(slots.remove_where(|_| true).is_none());
    let current = slots.reserve().unwrap();
    assert_ne!(failed.slot, current.slot);
    slots.insert(current, 17);
    assert!(!slots.cancel_reservation(current));
    let foreign = SAWindowId {
        host: SAHostId::allocate().unwrap(),
        ..failed
    };
    assert!(!slots.cancel_reservation(foreign));
    assert!(slots.cancel_reservation(failed));
    assert!(!slots.cancel_reservation(failed));
    let reused = slots.reserve().unwrap();
    assert_eq!(reused.slot, failed.slot);
    assert_eq!(reused.incarnation, failed.incarnation + 1);
    assert!(!slots.cancel_reservation(failed));
    assert!(slots.cancel_reservation(reused));
    assert_eq!(slots.get(current), Ok(&17));
}

#[test]
fn cancelled_reservation_at_last_incarnation_never_wraps_or_reuses_slot() {
    let mut slots = WindowSlots::<()>::new(SAHostId::allocate().unwrap());
    let mut last = slots.reserve().unwrap();
    slots.slots[0].incarnation = Some(u64::MAX);
    last.incarnation = u64::MAX;
    assert!(slots.cancel_reservation(last));
    assert_eq!(slots.slots[0].incarnation, None);
    let next = slots.reserve().unwrap();
    assert_ne!(next.slot, last.slot);
    assert_eq!(next.incarnation, 1);
    assert!(!slots.cancel_reservation(last));
}

#[test]
fn native_acquisition_failure_releases_and_advances_reserved_identity_before_retry() {
    use crate::backend::{BackendOps, test::TestBackend};
    use crate::context::{SAContext, SAContextPhase};
    use crate::foundation_tests::App;
    use crate::host::Core;
    use crate::window::SAWindowSpec;
    let mut core = Core::<App>::new().unwrap();
    let mut backend = TestBackend::default();
    backend.fail_title = Some(String::from("fail"));
    let spec = SAWindowSpec {
        title: String::from("fail"),
        ..SAWindowSpec::default()
    };
    let (returned, error) = SAContext::new(
        &mut core,
        BackendOps::Test(&mut backend),
        SAContextPhase::Startup,
    )
    .create_window(spec.clone())
    .unwrap_err()
    .into_parts();
    assert_eq!(returned, spec);
    assert!(matches!(
        error,
        SAError::Native {
            operation: crate::SANativeOperation::CreateWindow,
            ..
        }
    ));
    assert!(backend.trace.borrow().is_empty());
    assert!(core.native_windows.is_empty());
    let current = SAContext::new(
        &mut core,
        BackendOps::Test(&mut backend),
        SAContextPhase::Startup,
    )
    .create_window(SAWindowSpec::default())
    .unwrap();
    assert_eq!(current.id.slot, 0);
    assert_eq!(current.id.incarnation, 2);
}

#[test]
fn failed_initialization_reservation_reuses_only_after_acknowledged_incarnation_change() {
    use crate::backend::{BackendOps, test::TestBackend};
    use crate::context::{SAContext, SAContextPhase};
    use crate::foundation_tests::App;
    use crate::host::Core;
    use crate::window::SAWindowSpec;
    let mut core = Core::<App>::new().unwrap();
    let mut backend = TestBackend::default();
    backend.fail_next_observation = true;
    SAContext::new(
        &mut core,
        BackendOps::Test(&mut backend),
        SAContextPhase::Startup,
    )
    .create_window(SAWindowSpec::default())
    .unwrap_err();
    let failed = core.native_windows[0].1;
    let before_ack = SAContext::new(
        &mut core,
        BackendOps::Test(&mut backend),
        SAContextPhase::Startup,
    )
    .create_window(SAWindowSpec::default())
    .unwrap();
    assert_ne!(failed.id.slot, before_ack.id.slot);
    core.native_destroyed(backend.complete_destruction().unwrap());
    let after_ack = SAContext::new(
        &mut core,
        BackendOps::Test(&mut backend),
        SAContextPhase::Startup,
    )
    .create_window(SAWindowSpec::default())
    .unwrap();
    assert_eq!(failed.id.slot, after_ack.id.slot);
    assert_eq!(after_ack.id.incarnation, failed.id.incarnation + 1);
    assert_eq!(
        core.windows.get(failed.id).err(),
        Some(SAError::StaleIdentity)
    );
}
