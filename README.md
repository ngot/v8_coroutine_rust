# 🧵 v8_coroutine_rust

### ⚠️ TL;DR

Many stackful coroutines on **one shared V8 isolate**, each suspending in native code and resuming in any order.
Stock V8 cannot do this: it keeps per-thread state in C++ `thread_local`s, so two coroutines on one OS thread corrupt each other.
These crates link rusty_v8 against a V8 built with the coroutine-local patch from the [ngot/rusty_v8](https://github.com/ngot/rusty_v8/tree/coroutine) fork, which moves that state into a slot table per coroutine.

## 📦 Crates

| Crate                                     | What                                                                                                                               |
| ----------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| 🧵 [`v8-coroutine`](v8-coroutine)         | `Coroutine`, `CoroutineStack`, `suspend`, `park`, `init`: coroutines on a shared isolate, plus the patch's per-variable test suite |
| 🧬 [`v8-coroutine-sys`](v8-coroutine-sys) | rusty_v8 re-exported with its one exact pin, and bindings to the patch's exports (`coroutine_local`, `object_shape`, `trace`)      |

## 🚀 Using them

Depend on a release tag of this repository, and name the matching V8 release in your own `.cargo/config.toml`, because Cargo reads only the building workspace's config:

```toml
# Cargo.toml
[dependencies]
v8 = { package = "v8-coroutine-sys", git = "https://github.com/ngot/v8_coroutine_rust", tag = "v0.3.1" }
v8-coroutine = { git = "https://github.com/ngot/v8_coroutine_rust", tag = "v0.3.1" }
```

```toml
# .cargo/config.toml
[env]
RUSTY_V8_MIRROR = "https://github.com/ngot/rusty_v8/releases/download"
RUSTY_V8_MIRROR_TAG = "v152.2.0-coroutine.1"
```

rusty_v8's build script then downloads the patched archive for your target on the first build.
Linking a stock `v8` archive fails at link time (missing `v8_coroutine_*`), never silently, and `v8_coroutine::init` refuses an archive whose patch version differs from `PATCH_VERSION`.
Released targets: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`.

## 🏷️ Versions

| This repo | rusty_v8  | V8 release             | Patch version |
| --------- | --------- | ---------------------- | ------------- |
| `v0.3.1`  | `152.2.0` | `v152.2.0-coroutine.1` | 13            |

A patch change is released from the fork first, then this repository bumps `RUSTY_V8_MIRROR_TAG` and `PATCH_VERSION` together and tags a new version.

## 🧪 Testing

`cargo test --release` runs every layer against the configured V8 release; CI runs it on all four targets.

## ⚖️ License

MIT, see [LICENSE](LICENSE); V8 and rusty_v8 keep their own licenses (see each crate's README).
