//! Layer 1: the linked V8 archive carries this crate's version of the coroutine-local patch.
//! Linking this binary at all proves the exported symbols are present.

use v8_coroutine_sys::coroutine_local::*;

#[test]
fn patch_version_matches_crate() {
    assert_eq!(unsafe { v8_coroutine_patch_version() }, PATCH_VERSION);
}

#[test]
fn slot_array_is_non_empty() {
    assert!(unsafe { v8_coroutine_slot_count() } > 0);
}

#[test]
fn clearing_the_current_slots_is_allowed() {
    unsafe { v8_coroutine_slots_use(std::ptr::null_mut()) };
}
