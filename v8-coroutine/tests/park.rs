//! `park`: a coroutine that keeps the isolate locked across its suspend, and every way the lock
//! is taken back from it. A parked holder must always unlock on its own stack: unlocking it from
//! another stack aborts V8 (`Check failed: IsOnCentralStack()`), and a second `Locker` while it
//! holds the lock would block the OS thread.

mod common;
use common::{Js, finish, run, start};
use v8_coroutine::{Coroutine, CoroutineStack};

fn init() {
    common::init("--expose-gc");
}

/// `park()` in JS: suspends keeping the isolate, returns the string the test resumes it with.
fn js_park(scope: &mut v8::PinScope, _: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let value: String = v8_coroutine::park();
    rv.set(v8::String::new(scope, &value).unwrap().into());
}

fn js() -> Js {
    let js = Js::new();
    {
        let mut locker = js.isolate.lock();
        v8::scope!(let scope, &mut *locker);
        let context = v8::Local::new(scope, &*js.context);
        let scope = &mut v8::ContextScope::new(scope, context);
        let f = v8::Function::new(scope, js_park).unwrap();
        let key = v8::String::new(scope, "park").unwrap();
        context.global(scope).set(scope, key.into(), f.into()).unwrap();
    }
    js
}

/// Sums what it is resumed with until `end`, parking in between.
const SUM: &str = "let n = 0; for (;;) { const v = park(); if (v === 'end') break; \
                   n += Number(v); } String(n)";

/// Runs `src` under the thread's own `Locker`, as the inspector and teardown do.
fn on_the_thread(js: &Js, src: &str) -> String {
    let mut locker = js.isolate.lock();
    v8::scope!(let scope, &mut *locker);
    let context = v8::Local::new(scope, &*js.context);
    let scope = &mut v8::ContextScope::new(scope, context);
    run(scope, src).unwrap().to_rust_string_lossy(scope)
}

#[test]
fn a_parked_coroutine_resumes_a_thousand_times() {
    init();
    let js = js();
    let mut a = js.coroutine(SUM);
    start(&mut a);
    for _ in 0..1000 {
        assert!(a.resume("1".into()).is_none());
    }
    assert_eq!(finish(&mut a, "end").unwrap(), "1000");
}

#[test]
fn another_coroutine_takes_the_isolate_from_a_parked_one() {
    init();
    let js = js();
    let mut a = js.coroutine(SUM);
    let mut b = js.coroutine(SUM);
    // `c` suspends the ordinary way and collects garbage while the others are parked.
    let mut c = js.coroutine(
        "const kept = []; for (;;) { const v = suspend(); if (v === 'end') break; \
         kept.push({ v }); gc(); } String(kept.length)",
    );
    start(&mut a);
    start(&mut b);
    start(&mut c);
    for _ in 0..50 {
        assert!(a.resume("1".into()).is_none());
        assert!(b.resume("2".into()).is_none());
        assert!(c.resume("x".into()).is_none());
        assert!(a.resume("1".into()).is_none());
        assert!(a.resume("1".into()).is_none());
    }
    assert_eq!(finish(&mut a, "end").unwrap(), "150");
    assert_eq!(finish(&mut b, "end").unwrap(), "100");
    assert_eq!(finish(&mut c, "end").unwrap(), "50");
}

#[test]
fn release_lets_the_thread_take_its_own_locker() {
    init();
    let js = js();
    let mut a = js.coroutine(SUM);
    start(&mut a);
    for i in 0..50 {
        assert!(a.resume("1".into()).is_none());
        v8_coroutine::release();
        assert_eq!(on_the_thread(&js, &format!("gc(); String({i} + 1)")), (i + 1).to_string());
    }
    assert_eq!(finish(&mut a, "end").unwrap(), "50");
}

#[test]
fn release_without_a_parked_holder_does_nothing() {
    init();
    let js = js();
    v8_coroutine::release();
    assert_eq!(on_the_thread(&js, "'free'"), "free");
}

#[test]
fn dropping_a_parked_holder_gives_the_isolate_back() {
    init();
    let js = js();
    let mut a = js.coroutine(SUM);
    start(&mut a);
    // Dropped unfinished, so it is leaked, but it must not keep the isolate locked.
    drop(a);
    assert_eq!(on_the_thread(&js, "'free'"), "free");
    let mut b = js.coroutine("String(suspend())");
    start(&mut b);
    assert_eq!(finish(&mut b, "ok").unwrap(), "ok");
}

#[test]
fn park_inside_a_nested_resume_unlocks() {
    init();
    let js = js();
    let isolate = js.isolate.clone();
    let mut outer = Coroutine::new(js.stack(), js.isolate.clone(), move |locker, _: String| {
        let mut inner =
            Coroutine::new(CoroutineStack::new(), isolate, |_, _: String| -> String { v8_coroutine::park() });
        // The inner coroutine runs inside this one: keeping the isolate would leave it locked
        // under the inner identity, and this coroutine's relock would block the thread.
        assert!(locker.unlock(|| inner.resume(String::new())).is_none());
        let done = locker.unlock(|| inner.resume("inner".into()));
        drop(inner.into_stack());
        done.unwrap()
    });
    assert_eq!(outer.resume(String::new()).as_deref(), Some("inner"));
    assert_eq!(on_the_thread(&js, "'free'"), "free");
}
