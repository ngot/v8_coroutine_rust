//! Layer 5: with every coroutine on one shared slot array (what an unpatched V8 does), the
//! layer 3 scenarios must fail. Each runs in a child process, since failure may be a crash.

mod common;

use std::process::Command;

const CHILD_ENV: &str = "V8_COROUTINE_NEGATIVE_CONTROL_SCENARIO";

#[test]
fn scenarios_fail_when_coroutines_share_v8_slots() {
    let mut passed = vec![];
    for (name, _) in common::scenarios::ALL {
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "child_runs_one_scenario_with_shared_slots", "--ignored", "--nocapture"])
            .env(CHILD_ENV, name)
            .env("V8_COROUTINE_SHARE_SLOTS", "1")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        let why = stderr
            .lines()
            .find(|l| l.contains("Check failed") || l.contains("panicked") || l.contains("Fatal error in"))
            .unwrap_or("no message");
        eprintln!("shared slots, {name}: {} ({why})", out.status);
        if out.status.success() {
            passed.push(*name);
        }
    }
    assert!(passed.is_empty(), "these scenarios pass even without per-coroutine V8 state: {passed:?}");
}

#[test]
#[ignore = "child process of scenarios_fail_when_coroutines_share_v8_slots"]
fn child_runs_one_scenario_with_shared_slots() {
    let Ok(name) = std::env::var(CHILD_ENV) else {
        return;
    };
    common::init("--expose-gc");
    let (_, scenario) = common::scenarios::ALL.iter().find(|(n, _)| *n == name).unwrap();
    scenario();
}
