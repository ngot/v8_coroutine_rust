//! Layer 3 interleavings. Each scenario builds its own isolate and drives coroutines by hand.
//! Layers 4 and 5 rerun these under other V8 flags or with shared slots.

use super::*;

/// Every scenario, by name, for binaries that run them all.
pub const ALL: &[(&str, fn())] = &[
    ("out_of_order_resume", out_of_order_resume),
    ("try_catch_and_handles_across_suspend", try_catch_and_handles_across_suspend),
    ("exception_after_resume_reaches_its_own_try_catch", exception_after_resume_reaches_its_own_try_catch),
    ("stack_overflow_is_contained", stack_overflow_is_contained),
    ("gc_while_coroutines_are_suspended", gc_while_coroutines_are_suspended),
    ("thousand_coroutines_in_seeded_random_order", thousand_coroutines_in_seeded_random_order),
];

/// A suspends, B suspends, A resumes and finishes first.
pub fn out_of_order_resume() {
    let js = Js::new();
    let src = |name: &str| format!("const me = '{name}'; const got = suspend(); me + ':' + got");
    let (mut a, mut b) = (js.coroutine(&src("A")), js.coroutine(&src("B")));
    start(&mut a);
    start(&mut b);
    assert_eq!(finish(&mut a, "1"), Ok("A:1".into()));
    assert_eq!(finish(&mut b, "2"), Ok("B:2".into()));
}

/// JS try/catch, a Rust TryCatch, and live handles in both JS and Rust survive while
/// another coroutine runs.
pub fn try_catch_and_handles_across_suspend() {
    let js = Js::new();
    let mut a = js.coroutine_with(|scope| {
        let obj = v8::Object::new(scope);
        let (k, v) = (v8::String::new(scope, "x").unwrap(), v8::Integer::new(scope, 42));
        obj.set(scope, k.into(), v.into());
        let out =
            run(scope, "try { const o = { n: 7 }; const r = suspend(); `${o.n}${r}` } catch (e) { 'wrong ' + e }")?;
        let x = obj.get(scope, k.into())?.int32_value(scope)?;
        let s = format!("{}/{x}", out.to_rust_string_lossy(scope));
        Some(v8::String::new(scope, &s)?.into())
    });
    let mut b = js.coroutine("try { suspend(); throw new Error('from B') } catch (e) { 'B caught ' + e.message }");
    start(&mut a);
    start(&mut b);
    assert_eq!(finish(&mut b, ""), Ok("B caught from B".into()));
    assert_eq!(finish(&mut a, "!"), Ok("7!/42".into()));
}

/// An exception thrown after resume lands in the TryCatch of the coroutine that threw it, never
/// in one belonging to a coroutine that is suspended.
pub fn exception_after_resume_reaches_its_own_try_catch() {
    let js = Js::new();
    let mut a = js.coroutine("suspend(); throw new Error('thrown by A')");
    let mut b = js.coroutine("suspend(); 'B fine'");
    start(&mut a);
    start(&mut b);
    let err = finish(&mut a, "").unwrap_err();
    assert!(err.contains("thrown by A"), "{err}");
    assert_eq!(finish(&mut b, ""), Ok("B fine".into()));
}

/// Unbounded recursion in one coroutine is a RangeError there; the others are unharmed.
pub fn stack_overflow_is_contained() {
    let js = Js::new();
    let deep = "suspend(); function f(n) { return f(n + 1) + 1 } \
                try { f(0); 'no overflow' } catch (e) { e instanceof RangeError ? 'RangeError' : 'other ' + e }";
    let mut a = js.coroutine(deep);
    let mut b = js.coroutine(
        "const big = Array.from({ length: 1000 }, (_, i) => i); suspend(); String(big.reduce((s, x) => s + x))",
    );
    start(&mut a);
    start(&mut b);
    assert_eq!(finish(&mut a, ""), Ok("RangeError".into()));
    assert_eq!(finish(&mut b, ""), Ok("499500".into()));
    // The overflowed coroutine's stack is reusable.
    let mut c = Coroutine::new(a.into_stack(), js.isolate.clone(), |_, _: String| Ok::<_, String>(String::new()));
    assert!(c.resume(String::new()).is_some());
}

/// Full GCs while coroutines are suspended keep every suspended coroutine's objects alive and intact.
pub fn gc_while_coroutines_are_suspended() {
    let js = Js::new();
    let keep = |seed: u32| {
        format!(
            "const keep = Array.from({{ length: 20000 }}, (_, i) => ({{ i, s: 'v' + (i * {seed}) }})); \
             suspend(); \
             keep.every((o, i) => o.i === i && o.s === 'v' + (i * {seed})) ? 'intact' : 'corrupt'"
        )
    };
    let mut coroutines: Vec<_> = (1..=8).map(|seed| js.coroutine(&keep(seed))).collect();
    for f in &mut coroutines {
        start(f);
    }
    let mut gc = js.coroutine(
        "for (let i = 0; i < 5; i++) { gc(); Array.from({ length: 50000 }, (_, i) => ({ i })) } 'collected'",
    );
    assert_eq!(gc.resume(String::new()), Some(Ok("collected".into())));
    for f in coroutines.iter_mut().rev() {
        assert_eq!(finish(f, ""), Ok("intact".into()));
    }
}

/// 1000 coroutines, each suspending 5 times, resumed in a seeded random order.
pub fn thousand_coroutines_in_seeded_random_order() {
    const N: usize = 1000;
    const ROUNDS: usize = 5;
    let js = Js::new();
    let src = |id: usize| {
        format!(
            "let acc = 0; \
             for (let r = 0; r < {ROUNDS}; r++) {{ \
               const keep = [{id}, r, 'k' + {id}]; \
               acc += Number(suspend()); \
               if (keep[0] !== {id} || keep[1] !== r || keep[2] !== 'k' + {id}) throw new Error('corrupt'); \
             }} \
             String({id} * 1000000 + acc)"
        )
    };
    let mut coroutines: Vec<_> = (0..N).map(|id| js.coroutine(&src(id))).collect();
    let mut sums = vec![0usize; N];
    let mut left = vec![ROUNDS; N];
    for f in &mut coroutines {
        start(f);
    }
    let mut rng = Lcg(0x0de0);
    let mut live: Vec<usize> = (0..N).collect();
    while !live.is_empty() {
        let pick = rng.next(live.len());
        let id = live[pick];
        let v = rng.next(100);
        sums[id] += v;
        left[id] -= 1;
        match coroutines[id].resume(v.to_string()) {
            None => assert!(left[id] > 0, "coroutine {id} suspended after its last round"),
            Some(r) => {
                assert_eq!(left[id], 0, "coroutine {id} finished early: {r:?}");
                assert_eq!(r, Ok((id * 1_000_000 + sums[id]).to_string()));
                live.swap_remove(pick);
            }
        }
    }
}
