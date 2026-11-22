//! Layer 3: interleavings on one shared isolate, chosen by the test.

mod common;
use common::scenarios as s;

fn init() {
    common::init("--expose-gc");
}

#[test]
fn out_of_order_resume() {
    init();
    s::out_of_order_resume();
}

#[test]
fn try_catch_and_handles_across_suspend() {
    init();
    s::try_catch_and_handles_across_suspend();
}

#[test]
fn exception_after_resume_reaches_its_own_try_catch() {
    init();
    s::exception_after_resume_reaches_its_own_try_catch();
}

#[test]
fn stack_overflow_is_contained() {
    init();
    s::stack_overflow_is_contained();
}

#[test]
fn gc_while_coroutines_are_suspended() {
    init();
    s::gc_while_coroutines_are_suspended();
}

#[test]
fn thousand_coroutines_in_seeded_random_order() {
    init();
    s::thousand_coroutines_in_seeded_random_order();
}
