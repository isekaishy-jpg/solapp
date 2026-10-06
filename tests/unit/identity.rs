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

#[test]
fn arena_candidate_survives_failed_admission_and_exhaustion_never_rejoins_free_chain() {
    let mut arena = Arena::new(SAIdentityKind::Timer);
    let unused = arena.reserve().unwrap();
    assert_eq!(arena.reserve().unwrap(), unused);
    assert_eq!(arena.get(unused), None);
    arena.insert(unused, 10);
    let mut last = arena.reserve().unwrap();
    arena.entries[last.index as usize].incarnation = Some(u64::MAX);
    last.generation = u64::MAX;
    arena.insert(last, 20);
    let live = arena.reserve().unwrap();
    arena.insert(live, 30);
    assert_eq!(arena.remove(last), Some(20));
    assert_eq!(arena.remove(last), None);
    assert_eq!(arena.remove(unused), Some(10));
    let reused = arena.reserve().unwrap();
    assert_eq!(reused.index, unused.index);
    assert_eq!(reused.generation, unused.generation + 1);
    assert_eq!(
        arena.reserve().unwrap(),
        reused,
        "failed companion reservation leaves head unchanged"
    );
    arena.insert(reused, 40);
    let fresh = arena.reserve().unwrap();
    assert_eq!(fresh.index, 3);
    assert_eq!(arena.get(unused), None);
    assert_eq!(arena.get(last), None);
    assert_eq!(arena.get(live), Some(&30));
    assert_eq!(
        arena
            .iter()
            .map(|(key, value)| (key.index, *value))
            .collect::<Vec<_>>(),
        [(0, 40), (2, 30)]
    );
}

#[test]
fn arena_selection_is_linear_for_fresh_admissions_and_constant_for_reuse() {
    let mut arena = Arena::new(SAIdentityKind::Recipient);
    let mut keys = Vec::new();
    for value in 0..4096 {
        let key = arena.reserve().unwrap();
        arena.insert(key, value);
        keys.push(key);
    }
    assert_eq!(arena.selection_steps, 4096);
    let capacity = arena.entries.capacity();
    for key in &keys {
        assert!(arena.remove(*key).is_some());
    }
    assert_eq!(arena.entries.capacity(), capacity);
    for value in 0..4096 {
        let key = arena.reserve().unwrap();
        arena.insert(key, value);
    }
    assert_eq!(arena.selection_steps, 8192);
    assert_eq!(arena.entries.len(), 4096);
    assert_eq!(arena.entries.capacity(), capacity);
    println!("arena-selection: fresh=4096 steps=4096 reuse=4096 steps=4096");
}

#[test]
fn arena_owned_values_follow_a_mixed_identity_model() {
    use std::collections::HashMap;
    use std::{cell::Cell, rc::Rc};
    struct Owned {
        value: usize,
        drops: Rc<Cell<usize>>,
    }
    impl Drop for Owned {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
        }
    }
    let drops = Rc::new(Cell::new(0));
    let mut admitted = 0;
    let mut arena = Arena::new(SAIdentityKind::Subscription);
    let mut model = HashMap::new();
    let mut history = Vec::new();
    let mut seed = 17_u64;
    for value in 0..10000 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        if history.is_empty() || seed & 3 == 0 {
            let key = arena.reserve().unwrap();
            // Alternate failed-admission/retry before committing ownership.
            if value & 1 == 0 {
                assert_eq!(arena.reserve().unwrap(), key);
            }
            arena.insert(
                key,
                Owned {
                    value,
                    drops: Rc::clone(&drops),
                },
            );
            admitted += 1;
            assert_eq!(model.insert(key, value), None);
            history.push(key);
        } else {
            let key = history[(seed as usize) % history.len()];
            assert_eq!(
                arena.get(key).map(|owned| owned.value),
                model.get(&key).copied()
            );
            assert_eq!(
                arena.remove(key).map(|owned| owned.value),
                model.remove(&key)
            );
            assert!(arena.remove(key).is_none());
        }
        assert_eq!(arena.iter().count(), model.len());
    }
    for (key, value) in model {
        assert_eq!(arena.remove(key).map(|owned| owned.value), Some(value));
    }
    assert_eq!(arena.iter().count(), 0);
    assert_eq!(
        drops.get(),
        admitted,
        "each committed owned value is disposed exactly once"
    );
}

#[test]
fn arena_removal_and_reuse_require_no_second_allocation() {
    let mut arena = Arena::new(SAIdentityKind::Timer);
    let mut keys = Vec::new();
    for value in 0..4096 {
        let key = arena.reserve().unwrap();
        arena.insert(key, value);
        keys.push(key);
    }
    let (_, remove_counts) = crate::allocation_probe::measure(|| {
        for key in &keys {
            assert!(arena.remove(*key).is_some());
        }
    });
    let (_, reuse_counts) = crate::allocation_probe::measure(|| {
        for value in 0..4096 {
            let key = arena.reserve().unwrap();
            arena.insert(key, value);
        }
    });
    assert_eq!(remove_counts.allocations + remove_counts.reallocations, 0);
    assert_eq!(reuse_counts.allocations + reuse_counts.reallocations, 0);
    println!(
        "arena-backing: removal=4096 allocations=0 reallocations=0 reuse=4096 allocations=0 reallocations=0"
    );
}
