//! Layer 2: one test per `patched` row in the patch's coroutine/thread_locals.txt (ngot/rusty_v8, branch coroutine),
//! each showing that variable stays separate per coroutine.
//!
//! Most use bare coroutines with their own slot arrays, so a coroutine can hold V8 state
//! (an entered isolate, a held Locker) across a switch, which `v8_coroutine::Coroutine` never does.

mod common;

use std::ffi::c_void;
use std::ptr::null_mut;

use common::{current_isolate, raw, v8__Locker__IsLocked};
use corosensei::stack::{DefaultStack, Stack};
use corosensei::{Coroutine, CoroutineResult, Yielder};
use v8::coroutine_local as ffi;

/// A coroutine with its own V8 slot array; `step` runs it to its next yield.
struct BareCoroutine {
    co: Coroutine<(), (), (), DefaultStack>,
    slots: Vec<usize>,
}

impl BareCoroutine {
    fn new(f: impl FnOnce(&Yielder<(), ()>) + 'static) -> Self {
        let stack = DefaultStack::new(v8_coroutine::STACK_SIZE).unwrap();
        let mut slots = vec![0; unsafe { ffi::v8_coroutine_slot_count() }];
        unsafe {
            ffi::v8_coroutine_slots_init(
                slots.as_mut_ptr(),
                stack.base().get() as *mut c_void,
                stack.limit().get() as *mut c_void,
            )
        };
        BareCoroutine { co: Coroutine::with_stack(stack, move |y: &Yielder<(), ()>, ()| f(y)), slots }
    }

    /// Returns true when the coroutine finished.
    fn step(&mut self) -> bool {
        unsafe { ffi::v8_coroutine_slots_use(self.slots.as_mut_ptr()) };
        let r = self.co.resume(());
        unsafe { ffi::v8_coroutine_slots_use(null_mut()) };
        matches!(r, CoroutineResult::Return(()))
    }
}

/// An isolate that is not entered on the test thread. `addr` (the Rust wrapper, boxed so
/// it does not move) is safe to move into coroutines; `raw` is V8's `Isolate*` for comparisons.
struct Iso {
    isolate: Box<v8::OwnedIsolate>,
    addr: usize,
    raw: usize,
}

impl Iso {
    fn new() -> Self {
        common::init("");
        let isolate = Box::new(v8::Isolate::new(Default::default()));
        let addr = &**isolate as *const v8::Isolate as usize;
        let raw = raw(&isolate) as usize;
        unsafe { isolate.exit() };
        Iso { isolate, addr, raw }
    }

    fn get(addr: usize) -> &'static v8::Isolate {
        // SAFETY: the test keeps the isolate alive until every coroutine using it is done.
        unsafe { &*(addr as *const v8::Isolate) }
    }
}

impl Drop for Iso {
    fn drop(&mut self) {
        // OwnedIsolate's drop exits the isolate, so it must be entered here.
        unsafe { self.isolate.enter() };
    }
}

/// g_current_isolate_: each coroutine sees the isolate it entered, not the last one entered.
#[test]
fn current_isolate_is_per_coroutine() {
    let (one, two) = (Iso::new(), Iso::new());
    let coroutine = |iso: &Iso| {
        let (p, raw) = (iso.addr, iso.raw);
        BareCoroutine::new(move |y| {
            unsafe { Iso::get(p).enter() };
            y.suspend(());
            assert_eq!(current_isolate() as usize, raw, "coroutine sees another coroutine's isolate");
            unsafe { Iso::get(p).exit() };
            assert!(current_isolate().is_null());
        })
    };
    let (mut a, mut b) = (coroutine(&one), coroutine(&two));
    assert!(!a.step());
    assert!(!b.step());
    assert!(current_isolate().is_null(), "the scheduler sees a coroutine's isolate");
    assert!(a.step());
    assert!(b.step());
}

/// g_current_per_isolate_thread_data_: a second coroutine entering the same isolate gets its
/// own entry. With shared thread data, V8 treats it as a re-entry, and its Exit would
/// leave the isolate current.
#[test]
fn per_isolate_thread_data_is_per_coroutine() {
    let iso = Iso::new();
    let (p, raw) = (iso.addr, iso.raw);
    let mut a = BareCoroutine::new(move |y| {
        unsafe { Iso::get(p).enter() };
        y.suspend(());
        assert_eq!(current_isolate() as usize, raw);
        unsafe { Iso::get(p).exit() };
        assert!(current_isolate().is_null());
    });
    let mut b = BareCoroutine::new(move |_| {
        unsafe { Iso::get(p).enter() };
        unsafe { Iso::get(p).exit() };
        assert!(current_isolate().is_null(), "B's enter was treated as a re-entry of A's");
    });
    assert!(!a.step());
    assert!(b.step());
    assert!(a.step());
}

/// g_current_local_heap_: written by Isolate::Enter next to the two above. Release V8 only
/// reads it in runtime-call-stats and crash paths, so this checks the storage directly:
/// entering an isolate on one coroutine leaves every other coroutine's slots and the thread's alone.
#[test]
fn enter_writes_only_the_running_coroutines_slots() {
    let (one, two) = (Iso::new(), Iso::new());
    let (p1, p2) = (one.addr, two.addr);
    let coroutine = |p: usize| {
        BareCoroutine::new(move |y| {
            unsafe { Iso::get(p).enter() };
            y.suspend(());
            unsafe { Iso::get(p).exit() };
        })
    };
    let (mut a, mut b) = (coroutine(p1), coroutine(p2));
    let fresh_b = b.slots.clone();
    assert!(!a.step());
    let a_entered = a.slots.clone();
    assert_eq!(b.slots, fresh_b, "A's enter wrote B's slots");
    assert!(!b.step());
    assert_eq!(a.slots, a_entered, "B's enter wrote A's slots");
    let changed = a.slots.iter().zip(&b.slots).filter(|(x, y)| x != y).count();
    // thread id, isolate, per-isolate thread data, local heap, stack start
    assert!(changed >= 4, "only {changed} slots differ between coroutines in different isolates");
    assert!(a.step());
    assert!(b.step());
}

/// thread_id: V8's Locker ownership is keyed by it. A coroutine holding the Locker owns the
/// isolate; another coroutine and the scheduler do not.
#[test]
fn thread_id_is_per_coroutine() {
    common::init("");
    let shared = std::rc::Rc::new(unsafe { v8::Isolate::new(Default::default()).try_into_shared() }.unwrap());
    let addr = {
        let locker = shared.lock();
        raw(&locker) as usize
    };
    let is_locked = move || unsafe { v8__Locker__IsLocked(addr as *mut c_void) };
    let s = shared.clone();
    let mut a = BareCoroutine::new(move |y| {
        let locker = s.lock();
        assert!(is_locked());
        y.suspend(()); // still holding the Locker
        assert!(is_locked(), "A lost its Locker ownership across a switch");
        drop(locker);
    });
    let mut b = BareCoroutine::new(move |_| {
        assert!(!is_locked(), "B is taken for the coroutine that holds the Locker");
    });
    assert!(!a.step());
    assert!(b.step());
    assert!(!is_locked(), "the scheduler is taken for the coroutine that holds the Locker");
    assert!(a.step());
}

/// thread_stack_start: each coroutine's stack guard comes from its own stack, so deep recursion
/// is a RangeError at a sane depth on every coroutine (with the OS thread's stack start, the
/// first throw would hit V8's IsOnCentralStack CHECK, or overflow at once).
#[test]
fn stack_start_is_per_coroutine() {
    common::init("");
    let js = common::Js::new();
    let depth = "suspend(); let n = 0; function f() { n++; f() } \
                 try { f() } catch (e) { if (!(e instanceof RangeError)) throw e } String(n)";
    let (mut a, mut b) = (js.coroutine(depth), js.coroutine(depth));
    common::start(&mut a);
    common::start(&mut b);
    let da: usize = common::finish(&mut b, "").unwrap().parse().unwrap();
    let db: usize = common::finish(&mut a, "").unwrap().parse().unwrap();
    assert!(da > 1000 && db > 1000, "overflowed too early: {da}, {db} frames");
    // Frame size shifts with JIT tier-up timing (about 10% run to run), so allow 2x; a stack limit
    // taken from the wrong stack is off by far more, or aborts.
    assert!(da.max(db) < 2 * da.min(db), "coroutines got different stack limits: {da} vs {db} frames");
}

/// thread_reserved_stack_limit: the lower bound of V8's IsOnCentralStack check, which every
/// GC CHECKs. With the OS thread's value, the first GC on a coroutine aborts the process.
#[test]
fn reserved_stack_limit_is_per_coroutine() {
    common::init("");
    let js = common::Js::new();
    // Enough garbage for several scavenges, on two interleaved coroutines.
    let churn = "suspend(); let n = 0; for (let i = 0; i < 200; i++) n += Array.from({ length: 10000 }, (_, j) => ({ j })).length; String(n)";
    let (mut a, mut b) = (js.coroutine(churn), js.coroutine(churn));
    common::start(&mut a);
    common::start(&mut b);
    assert_eq!(common::finish(&mut a, ""), Ok("2000000".into()));
    assert_eq!(common::finish(&mut b, ""), Ok("2000000".into()));
}

/// current_per_thread_assert_data: a coroutine holding a `ValueView` (which opens a
/// DisallowGarbageCollectionInRelease scope) across a suspend must not stop other coroutines
/// from allocating: release V8 CHECKs AllowHeapAllocationInRelease on every allocation.
#[test]
fn per_thread_asserts_are_per_coroutine() {
    common::init("");
    let js = common::Js::new();
    let mut a = js.coroutine_with(|scope| {
        let s = v8::String::new(scope, "flat string held across a suspend")?;
        let view = v8::ValueView::new(scope, s);
        let _: String = v8_coroutine::suspend();
        let len = match view.data() {
            v8::ValueViewData::OneByte(b) => b.len(),
            v8::ValueViewData::TwoByte(b) => b.len(),
        };
        drop(view);
        Some(v8::Integer::new(scope, len as i32).into())
    });
    let mut b = js.coroutine("String(Array.from({ length: 100000 }, (_, i) => ({ i })).length)");
    common::start(&mut a);
    assert_eq!(b.resume(String::new()), Some(Ok("100000".into())));
    assert_eq!(common::finish(&mut a, ""), Ok("33".into()));
}
