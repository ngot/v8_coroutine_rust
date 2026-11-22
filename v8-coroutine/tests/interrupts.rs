//! Layer 2 for the stack guard: an interrupt is requested of the isolate, not of one coroutine,
//! so it reaches whichever coroutine runs JS next. Stock V8 archives the pending flags with the
//! state of the thread that last ran and restores another thread's over them, so a coroutine
//! spinning in JS never saw an interrupt requested while it was switched in.

mod common;
use common::*;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

fn init() {
    common::init("--expose-gc");
}

/// Long enough that a pending interrupt runs at a loop back edge.
const SPIN: &str = "let i = 0; while (i < 1e6) i++; 'spun'";

unsafe extern "C" fn mark(_: v8::UnsafeRawIsolatePtr, data: *mut c_void) {
    // SAFETY: `data` is a `&'static AtomicBool` (see `request`).
    unsafe { &*(data as *const AtomicBool) }.store(true, Ordering::SeqCst);
}

fn request(handle: &v8::IsolateHandle, fired: &'static AtomicBool) {
    assert!(handle.request_interrupt(mark, fired as *const AtomicBool as *mut c_void));
}

/// B suspends last, so the interrupt lands in its live state, which A's resume archives.
#[test]
fn an_interrupt_requested_while_suspended_reaches_the_coroutine_resumed_next() {
    init();
    static FIRED: AtomicBool = AtomicBool::new(false);
    let js = Js::new();
    let handle = js.isolate.lock().thread_safe_handle();
    let mut a = js.coroutine(&format!("suspend(); {SPIN}"));
    let mut b = js.coroutine("suspend(); 'B'");
    start(&mut a);
    start(&mut b);
    request(&handle, &FIRED);
    assert_eq!(finish(&mut a, ""), Ok("spun".into()));
    assert!(FIRED.load(Ordering::SeqCst), "the interrupt stayed with B's archived state");
    assert_eq!(finish(&mut b, ""), Ok("B".into()));
}

/// A coroutine's first entry initialises its stack guard, which must keep a pending interrupt.
#[test]
fn an_interrupt_requested_while_suspended_reaches_a_new_coroutine() {
    init();
    static FIRED: AtomicBool = AtomicBool::new(false);
    let js = Js::new();
    let handle = js.isolate.lock().thread_safe_handle();
    let mut a = js.coroutine("suspend(); 'A'");
    start(&mut a);
    request(&handle, &FIRED);
    let mut c = js.coroutine(SPIN);
    assert_eq!(c.resume(String::new()), Some(Ok("spun".into())));
    assert!(FIRED.load(Ordering::SeqCst), "the interrupt was cleared when C's stack guard started");
    assert_eq!(finish(&mut a, ""), Ok("A".into()));
}

/// Termination targets the JS that runs now: one pending when its coroutine suspends stays there.
#[test]
fn a_pending_termination_stays_with_its_coroutine() {
    init();
    let js = Js::new();
    let mut a = js.coroutine_with(|scope| {
        scope.terminate_execution();
        let _: String = v8_coroutine::suspend();
        run(scope, "'A ran'")
    });
    start(&mut a);
    let mut b = js.coroutine(SPIN);
    assert_eq!(b.resume(String::new()), Some(Ok("spun".into())));
    assert!(finish(&mut a, "").is_err(), "A's JS ran although its termination was pending");
}

/// A coroutine that finishes leaves its state live rather than archived; a request made then still
/// reaches the coroutine restored next.
#[test]
fn an_interrupt_requested_after_a_coroutine_finishes_reaches_the_next() {
    init();
    static FIRED: AtomicBool = AtomicBool::new(false);
    let js = Js::new();
    let handle = js.isolate.lock().thread_safe_handle();
    let mut a = js.coroutine(&format!("suspend(); {SPIN}"));
    start(&mut a);
    let mut b = js.coroutine("'B'");
    assert_eq!(b.resume(String::new()), Some(Ok("B".into())));
    request(&handle, &FIRED);
    assert_eq!(finish(&mut a, ""), Ok("spun".into()));
    assert!(FIRED.load(Ordering::SeqCst), "the interrupt was lost when B finished");
}

/// A termination still pending when its coroutine finishes ends with it.
#[test]
fn a_pending_termination_ends_with_its_coroutine() {
    init();
    let js = Js::new();
    let mut a = js.coroutine_with(|scope| {
        scope.terminate_execution();
        v8::String::new(scope, "A").map(Into::into)
    });
    assert_eq!(a.resume(String::new()), Some(Ok("A".into())));
    let mut b = js.coroutine(SPIN);
    assert_eq!(b.resume(String::new()), Some(Ok("spun".into())), "A's termination reached B");
}
