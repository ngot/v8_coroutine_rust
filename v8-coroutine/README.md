# 🧵 v8-coroutine

### ⚠️ TL;DR

Many stackful coroutines on **one shared V8 isolate**, each suspending in native code (`v8::Unlocker`) and resuming in any order.
Stock V8 cannot do this: it keys per-thread state by C++ `thread_local`s, so two coroutines on one OS thread corrupt each other.
This crate only works with the patched V8 of [`v8-coroutine-sys`](../v8-coroutine-sys), whose `coroutine-local.patch` keeps that state in a slot table per coroutine.
Linking it against a stock `v8` archive fails at link time (missing `v8_coroutine_*`), never silently.

## 🧩 How it works

| Piece                                           | Role                                                                                                                                                                                |
| ----------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 🩹 `v8-coroutine-sys`'s `coroutine-local.patch` | Declares 7 V8 `thread_local`s as `v8_coroutine::CoroutineLocal<T>`; values live in the running coroutine's slot table, selected through one real `thread_local` pointer             |
| 📦 `CoroutineStack`                             | A corosensei stack (`STACK_SIZE`, 512 KB reserved) plus its V8 slot table, initialized with that stack's bounds                                                                     |
| 🔁 `Coroutine::resume`                          | Selects this coroutine's table (`v8_coroutine_slots_use`), switches stacks, selects the OS thread's own table again                                                                 |
| 🔒 `Coroutine::new`                             | The body runs holding a `v8::Locker`, so V8 treats the coroutine as its own thread                                                                                                  |
| ⏸️ `suspend()`                                  | `Locker::unlock` (V8 archives this coroutine's state) around a corosensei yield; `~Unlocker` restores it on resume                                                                  |
| 🅿️ `park()` and `release()`                     | Yields keeping the isolate locked, for a coroutine likely to run next; any other `resume`, or `release()`, first resumes it to unlock on its own stack                              |
| 🔔 Interrupts                                   | A pending interrupt belongs to the isolate: the patch keeps it across a switch for the next coroutine, except a termination, which stays with the coroutine it hit and ends with it |
| 📏 `init()`                                     | Checks the patch version stamp, sets `--stack-size` to fit `STACK_SIZE`, starts V8 once                                                                                             |

The crate never schedules: the caller decides which coroutine resumes next, so an I/O event loop, a test, or anything else can drive it.

## 🚀 Usage

```rust
v8_coroutine::init(); // set other V8 flags first
let isolate = Rc::new(unsafe { v8::Isolate::new(Default::default()).try_into_shared() }.unwrap());

let mut co = Coroutine::new(CoroutineStack::new(), isolate.clone(), |locker, first: MyInput| {
    v8::scope!(let scope, &mut **locker);
    // ... run JS; a native callback calls v8_coroutine::suspend::<MyInput>() to wait for I/O
});
match co.resume(first_input) {
    None => { /* suspended: resume it later with the I/O result */ }
    Some(result) => { let stack = co.into_stack(); /* reuse it */ }
}
```

| Rule                                                                  | Why                                                                                                 |
| --------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------- |
| 🧯 Finish every coroutine before dropping it                          | An unfinished one is leaked, never unwound: unwinding would run through V8 frames                   |
| ♻️ Reuse stacks via `into_stack()`                                    | Each stack carries a V8 thread identity; V8 keeps metadata per identity, so a pool keeps it bounded |
| 🧵 Call `suspend::<In>()` only inside a coroutine, with its `In` type | It panics otherwise                                                                                 |
| 🚫 Never suspend while holding the lock by other means                | The next coroutine's `Locker` would block the OS thread                                             |
| 🔓 Call `release()` before a `Locker` taken outside every coroutine   | A coroutine parked with `park()` still holds the lock                                               |
| 🧯 Catch panics in native callbacks                                   | A panic must never unwind through V8 frames                                                         |

## 🏗️ Getting the patched V8

See [`v8-coroutine-sys`](../v8-coroutine-sys/README.md): rusty_v8's build script downloads the patched archive from an [ngot/rusty_v8](https://github.com/ngot/rusty_v8/tree/coroutine) release.

## 🧪 Test suite

`cargo test -p v8-coroutine-sys -p v8-coroutine` runs every layer against the configured release; the fork's CI runs the `thread_local` drift guard before it builds.

| Layer               | File                                | Catches                                                                                                                                                |
| ------------------- | ----------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 1️⃣ ABI              | `v8-coroutine-sys`'s `tests/abi.rs` | Unpatched or stale archive (version stamp)                                                                                                             |
| 2️⃣ Per variable     | `tests/per_variable.rs`             | One test per `patched` row of the patch's [`coroutine/thread_locals.txt`](https://github.com/ngot/rusty_v8/blob/coroutine/coroutine/thread_locals.txt) |
| 2️⃣ Interrupts       | `tests/interrupts.rs`               | An interrupt requested while suspended or after a finish reaches the next coroutine; a pending termination stays with its coroutine and ends with it   |
| 3️⃣ Interleaving     | `tests/interleaving.rs`             | Out-of-order resume, `TryCatch` and handles across suspends, stack overflow, GC while suspended, 1000 coroutines in seeded random order                |
| 3️⃣ Park             | `tests/park.rs`                     | `park` held across 1000 resumes, taken back by other coroutines, by `release()` and by drop; nested `park` unlocks                                     |
| 4️⃣ Heap checks      | `tests/heap_checks.rs`              | Layer 3 under `--verify-heap --stress-compaction`                                                                                                      |
| 5️⃣ Negative control | `tests/negative_control.rs`         | Layer 3 with all coroutines on one shared slot table, in child processes; every scenario must crash                                                    |
| 6️⃣ Drift guard      | the fork's `coroutine.yml`          | A V8 `thread_local` missing from `thread_locals.txt`                                                                                                   |

`V8_COROUTINE_EXTRA_FLAGS="--trace-gc"` adds V8 flags to any test binary when debugging.

## 📊 Cost

`cargo test --release -p v8-coroutine --test measure -- --ignored --nocapture`, on Apple Silicon (macOS 27), V8 15.2:

| Measure                                                                                      | Result              |
| -------------------------------------------------------------------------------------------- | ------------------- |
| ⏱️ Suspend + resume, round robin over 1000 coroutines (real archive and restore each switch) | about 280 to 330 ns |
| ⏱️ Suspend + resume, one coroutine re-locking (V8's lazy-archive fast path)                  | about 120 to 210 ns |
| 💾 Memory per suspended coroutine (touched stack pages, archived V8 state, JS objects)       | about 43 KB         |

## 📁 Layout

| Path            | What                                               |
| --------------- | -------------------------------------------------- |
| `src/lib.rs`    | `init`, `CoroutineStack`, `Coroutine`, `suspend`   |
| `tests/common/` | Hand-driven test harness and the layer 3 scenarios |

## ⚖️ Licenses

| Component                                    | License           | Use                                             |
| -------------------------------------------- | ----------------- | ----------------------------------------------- |
| 🧬 `v8-coroutine-sys` (patched V8, rusty_v8) | BSD-3-Clause, MIT | See [its README](../v8-coroutine-sys/README.md) |
| 🔀 corosensei                                | MIT OR Apache-2.0 | Unmodified crates.io dependency                 |

This crate's own source is licensed under MIT; see [LICENSE](LICENSE).

`v8-coroutine-sys` and its patched V8 dependency have separate licenses; see [its README](../v8-coroutine-sys/README.md).
