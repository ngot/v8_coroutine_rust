//! Test-only harness for the V8 coroutine patch suite. Nothing here schedules on its own:
//! every test picks which coroutine resumes next, so each interleaving is reproducible.
#![allow(dead_code)]

use std::ffi::c_void;
use std::rc::Rc;
use std::sync::Once;

use v8_coroutine::{Coroutine, CoroutineStack};

pub mod scenarios;

/// Initializes V8 once per test binary, with `flags` set first, then any flags in
/// `V8_COROUTINE_EXTRA_FLAGS` (for example `--trace-gc` when debugging).
pub fn init(flags: &str) {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        v8::V8::set_flags_from_string(flags);
        if let Ok(extra) = std::env::var("V8_COROUTINE_EXTRA_FLAGS") {
            v8::V8::set_flags_from_string(&extra);
        }
        v8_coroutine::init();
    });
}

// rusty_v8's own C bindings, exported by the archive; used to observe V8 thread state.
unsafe extern "C" {
    pub fn v8__Isolate__TryGetCurrent() -> *mut c_void;
    pub fn v8__Locker__IsLocked(isolate: *mut c_void) -> bool;
}

pub fn raw(isolate: &v8::Isolate) -> *mut c_void {
    // SAFETY: UnsafeRawIsolatePtr is a transparent wrapper over the isolate pointer.
    unsafe { std::mem::transmute(isolate.as_raw_isolate_ptr()) }
}

pub fn current_isolate() -> *mut c_void {
    unsafe { v8__Isolate__TryGetCurrent() }
}

/// A shared isolate with one context whose global has `suspend()` and `gc()`.
pub struct Js {
    pub isolate: Rc<v8::SharedIsolate>,
    pub context: Rc<v8::Global<v8::Context>>,
    /// Set by the negative control: every coroutine gets this stack's slots.
    shared_slots: Option<CoroutineStack>,
}

/// `suspend()` in JS: suspends the coroutine, returns the string the test resumes it with.
fn js_suspend(scope: &mut v8::PinScope, _: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let value: String = v8_coroutine::suspend();
    rv.set(v8::String::new(scope, &value).unwrap().into());
}

impl Js {
    pub fn new() -> Self {
        let isolate = unsafe { v8::Isolate::new(Default::default()).try_into_shared() }
            .unwrap_or_else(|_| panic!("isolate cannot be shared"));
        let context = {
            let mut locker = isolate.lock();
            v8::scope!(let scope, &mut *locker);
            let context = v8::Context::new(scope, Default::default());
            let scope = &mut v8::ContextScope::new(scope, context);
            let f = v8::Function::new(scope, js_suspend).unwrap();
            let key = v8::String::new(scope, "suspend").unwrap();
            context.global(scope).set(scope, key.into(), f.into()).unwrap();
            v8::Global::new(scope, context)
        };
        let shared_slots = std::env::var_os("V8_COROUTINE_SHARE_SLOTS").map(|_| CoroutineStack::new());
        Js { isolate: Rc::new(isolate), context: Rc::new(context), shared_slots }
    }

    pub fn stack(&self) -> CoroutineStack {
        let mut stack = CoroutineStack::new();
        if let Some(shared) = &self.shared_slots {
            stack.share_slots_with(shared);
        }
        stack
    }

    /// A coroutine with the context entered and a TryCatch around `body`.
    /// Returns `Ok(result)` or `Err(exception stack)`.
    pub fn coroutine_with<F>(&self, body: F) -> Coroutine<String, Result<String, String>>
    where
        F: for<'s, 'i> FnOnce(&mut v8::PinScope<'s, 'i>) -> Option<v8::Local<'s, v8::Value>> + 'static,
    {
        let context = self.context.clone();
        Coroutine::new(self.stack(), self.isolate.clone(), move |locker, _first: String| {
            v8::scope!(let scope, &mut **locker);
            let context = v8::Local::new(scope, &*context);
            let scope = &mut v8::ContextScope::new(scope, context);
            v8::tc_scope!(let tc, scope);
            match body(tc) {
                Some(v) => Ok(v.to_rust_string_lossy(tc)),
                None => {
                    let e = tc.stack_trace().or_else(|| tc.exception());
                    Err(e.map_or("no exception".into(), |e| e.to_rust_string_lossy(tc)))
                }
            }
        })
    }

    /// A coroutine that runs `src` as a classic script. The script is wrapped in a block so
    /// coroutines sharing the context can each declare the same top-level `let` / `const`.
    pub fn coroutine(&self, src: &str) -> Coroutine<String, Result<String, String>> {
        let src = format!("{{\n{src}\n}}");
        self.coroutine_with(move |scope| run(scope, &src))
    }
}

pub fn run<'s>(scope: &mut v8::PinScope<'s, '_>, src: &str) -> Option<v8::Local<'s, v8::Value>> {
    let code = v8::String::new(scope, src)?;
    v8::Script::compile(scope, code, None)?.run(scope)
}

/// Starts a coroutine: runs it to its first suspend. Panics if it finishes instead.
pub fn start(f: &mut Coroutine<String, Result<String, String>>) {
    assert!(f.resume(String::new()).is_none(), "coroutine finished before suspending");
}

/// Resumes a coroutine that must finish now, and returns its result.
pub fn finish(f: &mut Coroutine<String, Result<String, String>>, input: &str) -> Result<String, String> {
    f.resume(input.to_string()).expect("coroutine suspended again instead of finishing")
}

/// Deterministic PRNG for seeded interleavings.
pub struct Lcg(pub u64);

impl Lcg {
    pub fn next(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n
    }
}
