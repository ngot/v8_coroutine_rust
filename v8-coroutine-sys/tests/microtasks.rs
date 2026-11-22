//! Layer 1: the microtask exports move one job at a time and carry its owner.

use v8_coroutine_sys as v8;
use v8_coroutine_sys::coroutine_local::*;

fn run<'s>(scope: &mut v8::PinScope<'s, '_>, code: &str) -> v8::Local<'s, v8::Value> {
    let code = v8::String::new(scope, code).unwrap();
    v8::Script::compile(scope, code, None).unwrap().run(scope).unwrap()
}

#[test]
fn jobs_move_one_at_a_time_with_their_owner() {
    let platform = v8::new_default_platform(0, false).make_shared();
    v8::V8::initialize_platform(platform);
    v8::V8::initialize();
    let isolate = &mut v8::Isolate::new(Default::default());
    isolate.set_microtasks_policy(v8::MicrotasksPolicy::Explicit);
    let raw = isolate_ptr(isolate);
    v8::scope!(let scope, isolate);
    let context = v8::Context::new(scope, Default::default());
    let scope = &mut v8::ContextScope::new(scope, context);
    run(scope, "globalThis.log = []");
    unsafe { v8_coroutine_microtask_owner(raw, 7) };
    run(scope, "Promise.resolve().then(() => log.push('a'))");
    unsafe { v8_coroutine_microtask_owner(raw, 0) };
    run(scope, "Promise.resolve().then(() => log.push('b'))");
    assert_eq!(unsafe { v8_coroutine_microtask_count(raw) }, 2);
    assert_eq!(unsafe { v8_coroutine_microtask_peek(raw) }, 7);

    let queue = v8::MicrotaskQueue::new(scope, v8::MicrotasksPolicy::Explicit);
    let mut owner = -1;
    assert_eq!(unsafe { v8_coroutine_microtask_take(raw, queue_ptr(&queue), &mut owner) }, 1);
    assert_eq!(owner, 7);
    // A queue holding a job takes no other.
    assert_eq!(unsafe { v8_coroutine_microtask_take(raw, queue_ptr(&queue), &mut owner) }, -1);
    queue.perform_checkpoint(scope);
    assert_eq!(run(scope, "log.join()").to_rust_string_lossy(scope), "a");

    assert_eq!(unsafe { v8_coroutine_microtask_peek(raw) }, 0);
    assert_eq!(unsafe { v8_coroutine_microtask_take(raw, queue_ptr(&queue), &mut owner) }, 1);
    assert_eq!(owner, 0);
    queue.perform_checkpoint(scope);
    assert_eq!(run(scope, "log.join()").to_rust_string_lossy(scope), "a,b");
    assert_eq!(unsafe { v8_coroutine_microtask_count(raw) }, 0);
    assert_eq!(unsafe { v8_coroutine_microtask_peek(raw) }, -1);
    assert_eq!(unsafe { v8_coroutine_microtask_take(raw, queue_ptr(&queue), &mut owner) }, 0);
}
