//! Finishing a coroutine and reusing its stack frees everything the coroutine allocated.
//! Live Rust heap bytes are counted by a wrapping global allocator (V8 allocates elsewhere).

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};

use v8_coroutine::{Coroutine, CoroutineStack};

struct Counting;
static LIVE: AtomicIsize = AtomicIsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        LIVE.fetch_add(l.size() as isize, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size() as isize, Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static A: Counting = Counting;

#[test]
fn into_stack_frees_the_coroutine() {
    common::init("");
    let js = common::Js::new();
    let mut stack = CoroutineStack::new();
    let cycle = |stack: CoroutineStack| {
        let mut c = Coroutine::new(stack, js.isolate.clone(), |_, n: u32| n + 1);
        assert_eq!(c.resume(1), Some(2));
        c.into_stack()
    };
    stack = cycle(stack); // first use may allocate lazily
    let before = LIVE.load(Ordering::Relaxed);
    for _ in 0..1000 {
        stack = cycle(stack);
    }
    let leaked = LIVE.load(Ordering::Relaxed) - before;
    assert!(leaked < 1000, "{leaked} bytes still live after 1000 finished coroutines");
}
