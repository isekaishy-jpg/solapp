//! Per-thread, explicitly scoped allocation evidence for private unit fixtures.

#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Counts {
    pub(crate) allocations: usize,
    pub(crate) reallocations: usize,
    pub(crate) deallocations: usize,
}
thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static COUNTS: Cell<Counts> = const { Cell::new(Counts { allocations: 0, reallocations: 0, deallocations: 0 }) };
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
        record(|counts| counts.allocations = counts.allocations.saturating_add(1));
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(|counts| counts.allocations = counts.allocations.saturating_add(1));
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(|counts| counts.reallocations = counts.reallocations.saturating_add(1));
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        record(|counts| counts.deallocations = counts.deallocations.saturating_add(1));
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
