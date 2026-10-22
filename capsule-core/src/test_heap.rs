//! Heap accounting for the bounded-memory tests: every allocation and free made **on the
//! calling thread** is tallied, so a test reads its own peak without interference from the
//! tests running beside it. It wraps the system allocator and changes nothing else.
//!
//! One module for the whole unit-test binary, because a binary has exactly one
//! `#[global_allocator]`: every bounded-memory test in the crate measures through it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

fn track(delta: isize) {
    // `try_with`: an allocation during thread teardown must not panic.
    let _ = LIVE.try_with(|live| {
        let now = live.get() + delta;
        live.set(now);
        let _ = PEAK.try_with(|peak| peak.set(peak.get().max(now)));
    });
}

// SAFETY: every method delegates to `System` with the caller's own arguments and only adds
// thread-local bookkeeping, so `System`'s guarantees pass through unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded verbatim; the caller upholds `alloc`'s contract.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            track(layout.size() as isize);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded verbatim; the caller upholds `dealloc`'s contract.
        unsafe { System.dealloc(ptr, layout) };
        track(-(layout.size() as isize));
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded verbatim; the caller upholds `realloc`'s contract.
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            track(new_size as isize - layout.size() as isize);
        }
        new
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// Run `f` and return its result with the peak heap it grew this thread by, in bytes.
pub(crate) fn peak_during<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let base = LIVE.with(Cell::get);
    PEAK.with(|peak| peak.set(base));
    let out = f();
    let peak = PEAK.with(Cell::get);
    (out, usize::try_from(peak - base).unwrap_or(0))
}
