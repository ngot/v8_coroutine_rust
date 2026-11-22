//! The eval cache keeps a source it has seen once only as a hash placeholder, as
//! `compilation-cache-table.h` describes; V8 13.7 dropped the placeholder and caches the source
//! and its function on first sight, and a top-level eval's bytecode is never flushed, so every
//! distinct one-shot eval stayed in the heap until a memory-pressure GC.

use v8_coroutine_sys as v8;

const PAD: usize = 1000;

fn run(scope: &mut v8::PinScope, source: &str) {
    let source = v8::String::new(scope, source).unwrap();
    let script = v8::Script::compile(scope, source, None).unwrap();
    script.run(scope).unwrap();
}

fn used(scope: &mut v8::PinScope) -> usize {
    run(scope, "gc(); gc();");
    scope.get_heap_statistics().used_heap_size()
}

#[test]
fn a_one_shot_eval_is_not_kept() {
    // No tiering: optimized code for the loops would count as kept heap.
    v8::V8::set_flags_from_string("--expose-gc --no-opt --no-maglev --no-sparkplug");
    v8::V8::initialize_platform(v8::new_default_platform(0, false).make_shared());
    v8::V8::initialize();
    let isolate = &mut v8::Isolate::new(Default::default());
    v8::scope!(let scope, isolate);
    let context = v8::Context::new(scope, Default::default());
    let scope = &mut v8::ContextScope::new(scope, context);
    run(
        scope,
        &format!(
            "let n = 0;
             const evals = (count) => {{
               for (let i = 0; i < count; i++) (0, eval)(`${{n++}}; /*${{'x'.repeat({PAD})}}*/`);
             }};
             const again = (count) => {{
               for (let i = 0; i < count; i++) (0, eval)('1 + 1');
             }};
             evals(2000);"
        ),
    );
    let before = used(scope);
    run(scope, "evals(20000);");
    let per_eval = (used(scope).saturating_sub(before)) / 20000;
    // Unpatched: about 1,600 bytes each (the 1,000-byte source, its key and function).
    assert!(per_eval < PAD / 4, "{per_eval} bytes kept per distinct eval");

    // A source seen twice is still cached: the same function comes back from the cache.
    run(scope, "again(1000);");
    let before = used(scope);
    run(scope, "again(20000);");
    let per_eval = (used(scope).saturating_sub(before)) / 20000;
    assert!(per_eval < 8, "{per_eval} bytes kept per repeated eval");
}
