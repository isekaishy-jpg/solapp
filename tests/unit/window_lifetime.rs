use super::*;
use crate::identity::{SAHostId, SAWindowGeneration, WindowSlots};
use std::sync::atomic::AtomicUsize;

fn target() -> SAWindowTarget {
    let mut slots = WindowSlots::<()>::new(SAHostId::allocate().unwrap());
    SAWindowTarget {
        id: slots.reserve().unwrap(),
        generation: SAWindowGeneration::INITIAL,
    }
}

#[test]
fn transferable_leases_keep_exact_generation_until_owner_observes_release() {
    fn require_send_sync<T: Send + Sync>() {}
    require_send_sync::<SAWindowAccess>();
    let wakes = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&wakes);
    let window = target();
    let anchor = WindowAnchor::simulated(
        window,
        Arc::new(move || {
            count.fetch_add(1, Ordering::AcqRel);
        }),
    );
    let lease = anchor.acquire().unwrap();
    let clone = lease.clone();
    assert_eq!(anchor.external_count(), 2);
    anchor.begin_retirement();
    assert!(matches!(anchor.acquire(), Err(SAError::AdmissionClosed)));
    let released = thread::spawn(move || {
        assert_eq!(clone.target(), window);
        assert!(clone.is_native_alive());
        assert!(matches!(clone.native_ref(), Err(SAError::WrongThread)));
        drop(clone);
    });
    released.join().unwrap();
    assert_eq!(anchor.external_count(), 1);
    assert_eq!(wakes.load(Ordering::Acquire), 1);
    drop(lease);
    assert_eq!(anchor.external_count(), 0);
    assert_eq!(wakes.load(Ordering::Acquire), 2);
    assert!(anchor.is_alive());
}

#[test]
fn release_wake_observes_decrement_and_fault_invalidates_existing_leases() {
    let window = target();
    let root = Arc::new(std::sync::Mutex::new(std::sync::Weak::<WindowAnchor>::new()));
    let wake_root = Arc::clone(&root);
    let observed = Arc::new(AtomicUsize::new(usize::MAX));
    let wake_observed = Arc::clone(&observed);
    let anchor = WindowAnchor::simulated(
        window,
        Arc::new(move || {
            let anchor = wake_root.lock().unwrap().upgrade().unwrap();
            // This local upgrade is one extra root for inspection only.
            wake_observed.store(anchor.external_count() - 1, Ordering::Release);
        }),
    );
    *root.lock().unwrap() = Arc::downgrade(&anchor);
    let access = anchor.acquire().unwrap();
    assert!(matches!(access.native_ref(), Err(SAError::Native { .. })));
    anchor.invalidate_and_retain_native_root();
    assert!(!access.is_native_alive());
    assert!(matches!(access.native_ref(), Err(SAError::StaleIdentity)));
    assert!(matches!(anchor.acquire(), Err(SAError::StaleIdentity)));
    drop(access);
    assert_eq!(observed.load(Ordering::Acquire), 0);
}
