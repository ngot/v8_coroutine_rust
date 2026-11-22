//! Numbers for the README "Cost" table. Not a check; run with
//! `cargo test --release -p v8-coroutine --test measure -- --ignored --nocapture`.

mod common;

use std::time::Instant;

fn rss_kb() -> usize {
    let out =
        std::process::Command::new("ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().parse().unwrap()
}

#[test]
#[ignore = "measurement, run by hand"]
fn suspend_resume_latency_and_memory_per_suspended_coroutine() {
    common::init("");
    let js = common::Js::new();

    // Round trips: one coroutine, many suspends, so the cost is the switch itself.
    const ROUNDS: usize = 200_000;
    let mut f = js.coroutine(&format!("for (let i = 0; i < {ROUNDS}; i++) suspend(); 'done'"));
    common::start(&mut f);
    let t = Instant::now();
    for _ in 1..ROUNDS {
        assert!(f.resume(String::new()).is_none());
    }
    let per = t.elapsed() / ROUNDS as u32;
    assert_eq!(common::finish(&mut f, ""), Ok("done".into()));
    println!("suspend+resume round trip (Unlocker, archive, switch, Locker, restore): {per:?}");

    // Same, with 1000 coroutines alive so V8 really archives and restores a different thread each time.
    const N: usize = 1000;
    const EACH: usize = 100;
    let mut coroutines: Vec<_> =
        (0..N).map(|_| js.coroutine(&format!("for (let i = 0; i < {EACH}; i++) suspend(); 'done'"))).collect();
    for f in &mut coroutines {
        common::start(f);
    }
    let t = Instant::now();
    for _ in 1..EACH {
        for f in &mut coroutines {
            assert!(f.resume(String::new()).is_none());
        }
    }
    let per = t.elapsed() / (N * (EACH - 1)) as u32;
    for f in &mut coroutines {
        assert_eq!(common::finish(f, ""), Ok("done".into()));
    }
    println!("suspend+resume round trip, round robin over {N} coroutines: {per:?}");

    // Memory: RSS growth per coroutine suspended inside a JS call.
    const M: usize = 10_000;
    let before = rss_kb();
    let mut coroutines: Vec<_> =
        (0..M).map(|i| js.coroutine(&format!("const id = {i}; suspend(); String(id)"))).collect();
    for f in &mut coroutines {
        common::start(f);
    }
    let after = rss_kb();
    println!(
        "{M} suspended coroutines: RSS {before} KB -> {after} KB, {:.1} KB per suspended coroutine",
        (after - before) as f64 / M as f64
    );
    for (i, f) in coroutines.iter_mut().enumerate() {
        assert_eq!(common::finish(f, ""), Ok(i.to_string()));
    }
}
