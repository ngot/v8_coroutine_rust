//! Each coroutine stack sets V8's JS limit from its own bounds: recursion depth follows the
//! stack's size, holds across suspend, resume and reuse, and ignores `--stack-size`.

mod common;

use v8_coroutine::Coroutine;

/// Flags stay settable, as an embedder may start V8 itself, so a test can change `--stack-size` at run time; and
/// only the interpreter runs, so a frame's size, and the depth, do not depend on tiering.
const FLAGS: &str = "--no-freeze-flags-after-init --no-sparkplug --no-maglev --no-turbofan";

/// Recursion depth until `RangeError`, then the error's class, as `"<depth> <name>"`.
const DEPTH: &str = "let d = 0; function f() { d++; f(); }
let name = 'none'; try { f(); } catch (e) { name = e.constructor.name; } `${d} ${name}`";

fn depth(js: &common::Js) -> usize {
    let mut c = js.coroutine(DEPTH);
    let out = c.resume(String::new()).expect("finishes").expect("no exception");
    assert!(out.ends_with(" RangeError"), "{out}");
    out.split(' ').next().unwrap().parse().unwrap()
}

#[test]
fn depth_follows_the_stack_size() {
    common::init(FLAGS);
    let js = common::Js::new();
    let small = depth(&js);
    v8_coroutine::set_stack_size(2 * v8_coroutine::STACK_SIZE);
    let large = depth(&js);
    v8_coroutine::set_stack_size(v8_coroutine::MIN_STACK_SIZE);
    let tiny = depth(&js);
    v8_coroutine::set_stack_size(v8_coroutine::STACK_SIZE);
    // 448 KB of JS on the default stack, 960 KB on 1 MB, 128 KB on 192 KB.
    assert!(large * 10 > small * 19, "small {small}, large {large}");
    assert!(tiny * 3 < small, "small {small}, tiny {tiny}");
}

#[test]
fn stack_size_flag_does_not_move_the_limit() {
    common::init(FLAGS);
    let js = common::Js::new();
    let before = depth(&js);
    // A limit 4 MB below the start of a 512 KB stack would let JS run into the guard page.
    v8::V8::set_flags_from_string("--stack-size=4000");
    let after = depth(&js);
    v8::V8::set_flags_from_string(&format!(
        "--stack-size={}",
        (v8_coroutine::STACK_SIZE - v8_coroutine::STACK_MARGIN) / 1024
    ));
    assert!(after * 10 < before * 11 && before * 10 < after * 11, "before {before}, after {after}");
}

/// Recursion depth, as a function a script can call twice.
const MEASURE: &str = "function measure() { let d = 0; function f() { d++; f(); } try { f(); } catch {} return d; }";

#[test]
fn limit_holds_across_suspend_and_reuse() {
    common::init(FLAGS);
    let js = common::Js::new();
    let default = depth(&js);
    v8_coroutine::set_stack_size(1024 * 1024);
    let mut c = js.coroutine(&format!("{MEASURE} const first = measure(); suspend(); `${{first}} ${{measure()}}`"));
    common::start(&mut c);
    // Another coroutine runs on its own stack while this one is suspended.
    let other = depth(&js);
    let out = common::finish(&mut c, "").expect("no exception");
    let stack = c.into_stack();
    assert!(stack.size() >= 1024 * 1024);
    let context = js.context.clone();
    let mut reuse = Coroutine::new(stack, js.isolate.clone(), move |locker, ()| {
        v8::scope!(let scope, &mut **locker);
        let context = v8::Local::new(scope, &*context);
        let scope = &mut v8::ContextScope::new(scope, context);
        let v = common::run(scope, &format!("{{ {MEASURE} measure() }}")).unwrap();
        v.to_rust_string_lossy(scope)
    });
    let reused: usize = reuse.resume(()).unwrap().parse().unwrap();
    v8_coroutine::set_stack_size(v8_coroutine::STACK_SIZE);
    let (first, second) = out.split_once(' ').unwrap();
    let (first, second): (usize, usize) = (first.parse().unwrap(), second.parse().unwrap());
    let near = |a: usize, b: usize| a * 10 > b * 9 && a * 10 < b * 11;
    assert!(first > default * 2, "first {first}, default {default}");
    assert!(near(other, first), "first {first}, other {other}");
    assert!(near(second, first), "first {first}, second {second}");
    assert!(near(reused, first), "first {first}, reused {reused}");
}
