//! Per-thread, explicitly scoped allocation evidence for private unit fixtures.
//! Byte counters describe requested layouts, not allocator overhead or live memory.
//! Allocation/reallocation calls are counted even if the allocator returns null;
//! deallocation may release storage allocated outside this scope or thread.

#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Counts {
    pub(crate) allocations: usize,
    pub(crate) reallocations: usize,
    pub(crate) deallocations: usize,
    pub(crate) requested_bytes: usize,
    pub(crate) reallocated_bytes: usize,
    pub(crate) deallocated_bytes: usize,
}
thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static COUNTS: Cell<Counts> = const { Cell::new(Counts {
        allocations: 0, reallocations: 0, deallocations: 0,
        requested_bytes: 0, reallocated_bytes: 0, deallocated_bytes: 0,
    }) };
}

struct Probe;
#[global_allocator]
static ALLOCATOR: Probe = Probe;

fn record(update: impl FnOnce(&mut Counts)) {
    // TLS may already be torn down during a thread's final deallocation.
    let _ = ACTIVE.try_with(|active| {
        if active.get() {
            let _ = COUNTS.try_with(|counts| {
                let mut value = counts.get();
                update(&mut value);
                counts.set(value);
            });
        }
    });
}

// SAFETY: every allocation operation forwards the identical layout/pointer to
// System; bookkeeping uses const TLS Cells and neither allocates nor unwinds.
unsafe impl GlobalAlloc for Probe {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(|counts| {
            counts.allocations = counts.allocations.saturating_add(1);
            counts.requested_bytes = counts.requested_bytes.saturating_add(layout.size());
        });
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(|counts| {
            counts.allocations = counts.allocations.saturating_add(1);
            counts.requested_bytes = counts.requested_bytes.saturating_add(layout.size());
        });
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(|counts| {
            counts.reallocations = counts.reallocations.saturating_add(1);
            counts.reallocated_bytes = counts.reallocated_bytes.saturating_add(size);
        });
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        record(|counts| {
            counts.deallocations = counts.deallocations.saturating_add(1);
            counts.deallocated_bytes = counts.deallocated_bytes.saturating_add(layout.size());
        });
        unsafe { System.dealloc(ptr, layout) }
    }
}

struct Scope;
impl Drop for Scope {
    fn drop(&mut self) {
        ACTIVE.with(|active| active.set(false));
    }
}

pub(crate) fn measure<T>(work: impl FnOnce() -> T) -> (T, Counts) {
    ACTIVE.with(|active| assert!(!active.get(), "allocation probe scopes cannot nest"));
    COUNTS.with(|counts| counts.set(Counts::default()));
    ACTIVE.with(|active| active.set(true));
    let scope = Scope;
    let result = work();
    drop(scope);
    (result, COUNTS.with(Cell::get))
}

#[test]
fn requested_bytes_and_scoped_release_do_not_claim_live_memory() {
    use std::hint::black_box;
    let (buffer, growth) = measure(|| {
        let mut buffer = Vec::<u8>::with_capacity(32);
        buffer.extend_from_slice(&[7; 32]);
        buffer.reserve_exact(64);
        black_box(buffer)
    });
    assert_eq!(growth.allocations, 1);
    assert_eq!(growth.requested_bytes, 32);
    assert_eq!(growth.reallocations, 1);
    assert_eq!(growth.reallocated_bytes, buffer.capacity());
    assert_eq!(growth.deallocations, 0);
    let capacity = buffer.capacity();
    let (_, release) = measure(|| drop(black_box(buffer)));
    assert_eq!(release.allocations + release.reallocations, 0);
    assert_eq!(release.deallocations, 1);
    assert_eq!(release.deallocated_bytes, capacity);
}
