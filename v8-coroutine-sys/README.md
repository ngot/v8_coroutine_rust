# 🧬 v8-coroutine-sys

### ⚠️ TL;DR

`v8-coroutine-sys` wraps its dependency `rusty_v8` (the crates.io `v8` crate), links it against a V8 built with the coroutine-local patch, and re-exports it whole, so dependents keep writing `v8::`.
The patch keeps V8's per-thread state in a slot table per coroutine, which is what lets [`v8-coroutine`](../v8-coroutine) run many coroutines on one isolate, keeps a pending interrupt across a coroutine switch, and it exports a reusable object shape, which is the one fast-property layout the public API cannot express.
The patch, its `thread_local` audit and the archive build live on the `coroutine` branch of [ngot/rusty_v8](https://github.com/ngot/rusty_v8/tree/coroutine), whose tagged releases carry the archives.
Linking it against a stock `v8` archive fails at link time (missing `v8_coroutine_*`), never silently.

## 🧩 What it holds

| Path         | What                                                                                                                                                                                                                                                 |
| ------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Cargo.toml` | The only exact pin of the `rusty_v8` dependency: `rusty_v8 = { package = "v8", version = "=X.Y.Z" }`                                                                                                                                                 |
| `lib.rs`     | `pub use rusty_v8::*`, plus `coroutine_local` (the thread-state exports and `PATCH_VERSION`), `object_shape` (reusable fast-property layouts) and `trace` (V8's own trace events through libplatform's controller, with the legacy macros mapped on) |
| `tests/`     | Layer 1, `abi.rs`: the linked archive carries this patch version; the object shape, microtask, eval cache and script cache behaviour the patch adds                                                                                                  |

## 🏗️ Getting the patched V8

rusty_v8's build script downloads the archive and its binding itself once two variables name the fork's release, for example in a `.cargo/config.toml` `[env]` table:

```toml
RUSTY_V8_MIRROR = "https://github.com/ngot/rusty_v8/releases/download"
RUSTY_V8_MIRROR_TAG = "v152.2.0-coroutine.1"
```

| Rule                                                                   | Why                                                                                                                                                                                   |
| ---------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 🏷️ The tag is `v<pinned rusty_v8 version>-coroutine.<n>`               | Each release is cut from the rusty_v8 tag this crate pins                                                                                                                             |
| 🔢 `PATCH_VERSION` in `lib.rs` equals the archive's patch version      | `tests/abi.rs` fails on a mismatch, and `v8_coroutine::init` refuses to start                                                                                                         |
| 💾 The build script reads `~/.cargo/.rusty_v8` first, keyed on the tag | The build script never writes it; filling it once with the release's files spares every fresh target directory a download; a released tag is final, so a patch change takes a new one |

To test an unreleased patch, build it in the fork (see its `coroutine/README.md`) and set `RUSTY_V8_ARCHIVE` and `RUSTY_V8_SRC_BINDING_PATH` to the build output instead.

## ⚖️ Licenses

| Component                      | License      | Use                                                                                                   |
| ------------------------------ | ------------ | ----------------------------------------------------------------------------------------------------- |
| ⚙️ V8                          | BSD-3-Clause | Built with the coroutine-local patch, which modifies V8 files; the patch carries its own license file |
| 📦 `rusty_v8` (crates.io `v8`) | MIT          | Unmodified dependency                                                                                 |

⚠️ Shipping the patched static library must include V8's `LICENSE` and the notices of the third-party code built into it (ICU, abseil, and others); every fork release carries them as `licenses_<target>.tar.gz`.
This crate's original Rust code and documentation are licensed under MIT; see [LICENSE](LICENSE).
