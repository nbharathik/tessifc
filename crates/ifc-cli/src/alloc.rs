// SPDX-License-Identifier: Apache-2.0
//! Approximate allocated-byte peak, distinct from resident memory.
//! Threads batch their deltas; unflushed bytes are absent from the report.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};

static CURRENT: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

/// Bytes a thread accumulates before it touches the shared counters.
const FLUSH_BYTES: isize = 256 * 1024;

thread_local! {
    // `const` initialised and without a destructor, so reaching it never
    // allocates, which inside an allocator would recurse.
    static PENDING: Cell<isize> = const { Cell::new(0) };
}

/// Wraps the system allocator with a byte counter.
pub struct Counting;

impl Counting {
    /// A counting allocator. `const` so it can be a `#[global_allocator]`.
    pub const fn new() -> Self {
        Counting
    }
}

/// The approximate high-water mark since the process started.
pub fn peak() -> usize {
    let _ = PENDING.try_with(|pending| flush(pending.replace(0)));
    PEAK.load(Ordering::Relaxed)
}

#[inline]
fn record(delta: isize) {
    // A thread whose local storage is already torn down (the last frees on
    // the way out) goes straight to the shared counters.
    let flushed = PENDING.try_with(|pending| {
        let total = pending.get() + delta;
        if total.abs() >= FLUSH_BYTES {
            pending.set(0);
            Some(total)
        } else {
            pending.set(total);
            None
        }
    });
    match flushed {
        Ok(Some(total)) => flush(total),
        Ok(None) => {}
        Err(_) => flush(delta),
    }
}

#[cold]
fn flush(delta: isize) {
    update(&CURRENT, &PEAK, delta);
}

fn update(current: &AtomicIsize, peak: &AtomicUsize, delta: isize) {
    // A free on another thread can arrive before the allocating thread flushes.
    let now = current
        .fetch_add(delta, Ordering::Relaxed)
        .wrapping_add(delta);
    peak.fetch_max(now.max(0) as usize, Ordering::Relaxed);
}

// SAFETY: every method forwards to the system allocator with the layout it was
// given; the bookkeeping (a thread-local cell and two atomics) never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds the GlobalAlloc contract for `layout`.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            record(layout.size() as isize);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `alloc` with this same `layout`.
        unsafe { System.dealloc(ptr, layout) };
        record(-(layout.size() as isize));
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds the GlobalAlloc contract for `layout`.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            record(layout.size() as isize);
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `ptr` came from `alloc` with `layout`, and the caller
        // guarantees `new_size` is valid.
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            record(new_size as isize - layout.size() as isize);
        }
        new_ptr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frees_flushed_before_allocations_do_not_wrap_the_peak() {
        let current = AtomicIsize::new(0);
        let peak = AtomicUsize::new(0);
        update(&current, &peak, -512_000);
        update(&current, &peak, 128_000);
        assert_eq!(peak.load(Ordering::Relaxed), 0);
        update(&current, &peak, 512_000);
        assert_eq!(current.load(Ordering::Relaxed), 128_000);
        assert_eq!(peak.load(Ordering::Relaxed), 128_000);
        update(&current, &peak, -128_000);
        assert_eq!(peak.load(Ordering::Relaxed), 128_000);
    }
}
