//! Layer 3: coroutines that enter different contexts on one shared isolate. Each coroutine
//! must see its own context as current after a suspend, whatever order they resume in.

mod common;

use std::rc::Rc;

use common::{Js, finish, run, start};
use v8_coroutine::Coroutine;

type Probe = Coroutine<String, Result<String, String>>;

fn init() {
    common::init("");
}

/// A context whose global `marker` names it. Created outside any coroutine.
fn marked_context(js: &Js, marker: &str) -> Rc<v8::Global<v8::Context>> {
    let mut locker = js.isolate.lock();
    v8::scope!(let scope, &mut *locker);
    let context = v8::Context::new(scope, Default::default());
    {
        let scope = &mut v8::ContextScope::new(scope, context);
        run(scope, &format!("globalThis.marker = {marker:?};")).unwrap();
    }
    Rc::new(v8::Global::new(scope, context))
}

/// The marker of the context that is current in `scope`.
fn read_marker(scope: &mut v8::PinScope) -> String {
    let value = run(scope, "globalThis.marker").expect("no marker in the current context");
    value.to_rust_string_lossy(scope)
}

/// Enters `context`, reads its marker, suspends, and reads it again.
/// Reports `before,input,after`.
fn flat_probe(js: &Js, context: Rc<v8::Global<v8::Context>>) -> Probe {
    js.coroutine_with(move |tc| {
        let context = v8::Local::new(tc, &*context);
        let scope = &mut v8::ContextScope::new(tc, context);
        let before = read_marker(scope);
        let input: String = v8_coroutine::suspend();
        let after = read_marker(scope);
        let report = format!("{before},{input},{after}");
        Some(v8::String::new(scope, &report).unwrap().into())
    })
}

/// Enters `outer`, then `inner`, and suspends inside `inner`. Once `inner` is left it reads
/// the marker again. Reports `before,input,after,outer`.
fn nested_probe(js: &Js, outer: Rc<v8::Global<v8::Context>>, inner: Rc<v8::Global<v8::Context>>) -> Probe {
    js.coroutine_with(move |tc| {
        let outer = v8::Local::new(tc, &*outer);
        let scope = &mut v8::ContextScope::new(tc, outer);
        let (before, input, after) = {
            let inner = v8::Local::new(scope, &*inner);
            let scope = &mut v8::ContextScope::new(scope, inner);
            let before = read_marker(scope);
            let input: String = v8_coroutine::suspend();
            (before, input, read_marker(scope))
        };
        let outer_marker = read_marker(scope);
        let report = format!("{before},{input},{after},{outer_marker}");
        Some(v8::String::new(scope, &report).unwrap().into())
    })
}

#[test]
fn contexts_survive_out_of_order_resume() {
    init();
    let js = Js::new();
    let a = marked_context(&js, "a");
    let b = marked_context(&js, "b");
    let mut first = flat_probe(&js, a);
    let mut second = flat_probe(&js, b);
    start(&mut first);
    start(&mut second);

    // Opposite order to the starts: b resumes and finishes before a does.
    assert_eq!(finish(&mut second, "to-b").unwrap(), "b,to-b,b");
    assert_eq!(finish(&mut first, "to-a").unwrap(), "a,to-a,a");
}

#[test]
fn nested_context_survives_suspend_while_another_context_runs() {
    init();
    let js = Js::new();
    let a = marked_context(&js, "a");
    let b = marked_context(&js, "b");
    let mut nested = nested_probe(&js, a.clone(), b);
    let mut other = flat_probe(&js, a);
    start(&mut nested);
    start(&mut other);

    // `other` resumes while `nested` is suspended inside its inner context.
    assert_eq!(finish(&mut other, "to-other").unwrap(), "a,to-other,a");
    assert_eq!(finish(&mut nested, "to-nested").unwrap(), "b,to-nested,b,a");
}
