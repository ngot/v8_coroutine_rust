//! Layer 4: the layer 3 scenarios with V8's heap verifier and compaction stress on.

mod common;

#[test]
fn scenarios_under_verify_heap_and_stress_compaction() {
    common::init("--expose-gc --verify-heap --stress-compaction");
    for (name, scenario) in common::scenarios::ALL {
        eprintln!("scenario: {name}");
        scenario();
    }
}
