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
