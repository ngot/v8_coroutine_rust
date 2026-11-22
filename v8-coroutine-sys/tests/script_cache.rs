//! The script cache keeps a script seen once only as its weak key, and keeps the top-level
//! function once the same source is compiled again while that Script is alive. Stock V8 15 keeps
//! the function from the first compile until its bytecode is flushed, which time-based flushing
//! does after 180 seconds of GCs that V8 starts itself, so a burst of distinct scripts
//! (`vm.Script`, code mode) filled a small heap.

use v8_coroutine_sys as v8;

const PAD: usize = 1000;

fn compile<'s>(scope: &mut v8::PinScope<'s, '_>, source: &str) -> v8::Local<'s, v8::Script> {
    let source = v8::String::new(scope, source).unwrap();
    v8::Script::compile(scope, source, None).unwrap()
}

fn run(scope: &mut v8::PinScope, source: &str) {
    // Its own handle scope, so only the cache can keep the compiled script.
    v8::scope!(let scope, scope);
    let script = compile(scope, source);
    script.run(scope).unwrap();
}

fn used(scope: &mut v8::PinScope) -> usize {
    run(scope, "gc(); gc();");
    scope.get_heap_statistics().used_heap_size()
}

#[test]
fn a_one_shot_script_is_not_kept() {
    // No tiering: optimized code for the loops would count as kept heap.
    v8::V8::set_flags_from_string("--expose-gc --no-opt --no-maglev --no-sparkplug");
    v8::V8::initialize_platform(v8::new_default_platform(0, false).make_shared());
    v8::V8::initialize();
    let isolate = &mut v8::Isolate::new(Default::default());
    v8::scope!(let scope, isolate);
    let context = v8::Context::new(scope, Default::default());
    let scope = &mut v8::ContextScope::new(scope, context);
    let pad = "x".repeat(PAD);
    for i in 0..2000 {
        run(scope, &format!("{i}; /*{pad}*/"));
    }
    let before = used(scope);
    for i in 2000..22000 {
        run(scope, &format!("{i}; /*{pad}*/"));
    }
    let per_script = (used(scope).saturating_sub(before)) / 20000;
    // Unpatched: about 1,600 bytes each (the 1,000-byte source, its Script and function).
    assert!(per_script < PAD / 4, "{per_script} bytes kept per distinct script");

    // A script compiled twice stays cached: the third compile returns the same top-level
    // function, across more mark-compacts than an eval placeholder lives. A mark-compact between
    // the first two compiles would only move the caching to the third.
    // Stock V8 passes this half too; it catches a patch that never caches.
    let again = format!("1; /*{pad}*/");
    run(scope, &again);
    run(scope, &again);
    let first = {
        v8::scope!(let scope, scope);
        let unbound = compile(scope, &again).get_unbound_script(scope);
        v8::Weak::new(scope, unbound)
    };
    for _ in 0..15 {
        run(scope, "gc();");
    }
    v8::scope!(let scope, scope);
    let unbound = compile(scope, &again).get_unbound_script(scope);
    let kept = first.to_local(scope).expect("the cached script was collected");
    assert!(kept == unbound, "the third compile missed the cache");
}
