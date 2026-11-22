//! Coroutines on one shared V8 isolate.
//!
//! Requires the V8 archive of the `v8-coroutine-sys` crate, built with its `coroutine-local.patch`, which keeps
//! V8's per-thread state in a slot table per coroutine. Each [`CoroutineStack`] owns a stack
//! and its slot table; [`Coroutine::resume`] points V8 at that table, and [`suspend`]
//! releases the isolate with `v8::Unlocker` so V8 archives the coroutine's state.
//! [`park`] instead keeps the isolate locked for a coroutine likely to run next; any other
//! resume, or [`release`], first resumes it so it unlocks on its own stack.
//!
//! The crate never schedules anything: the caller decides which coroutine resumes next.

use std::any::TypeId;
use std::cell::{Cell, UnsafeCell};
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::ptr::{null, null_mut};
use std::rc::Rc;

use corosensei::stack::{DefaultStack, Stack};
use corosensei::{Coroutine as RawCoroutine, CoroutineResult, Yielder};
use v8::coroutine_local as ffi;

/// Coroutine stack size. Reserved address space; only touched pages are committed.
pub const STACK_SIZE: usize = 512 * 1024;
/// Stack kept below V8's JS limit for V8's own C++ frames and native callbacks.
pub const STACK_MARGIN: usize = 64 * 1024;
/// The smallest stack [`set_stack_size`] gives: Node's floor for a Worker's `stackSizeMb`.
pub const MIN_STACK_SIZE: usize = 192 * 1024;

/// The platform's background thread count [`init`] uses; 0 keeps the default.
static PLATFORM_THREADS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Sets how many background threads the V8 platform runs (at least one); call it before [`init`].
pub fn set_platform_threads(threads: u32) {
    PLATFORM_THREADS.store(threads.max(1), std::sync::atomic::Ordering::Relaxed);
}

thread_local! {
    /// The size of the stacks [`CoroutineStack::new`] makes on this thread.
    static THREAD_STACK_SIZE: Cell<usize> = const { Cell::new(STACK_SIZE) };
}

/// Sets the size of the stacks [`CoroutineStack::new`] makes on the calling thread, at least
/// [`MIN_STACK_SIZE`]. Call it before the thread's first coroutine: stacks made earlier keep
/// their size.
pub fn set_stack_size(bytes: usize) {
    THREAD_STACK_SIZE.with(|s| s.set(bytes.max(MIN_STACK_SIZE)));
}

/// The size of the stacks [`CoroutineStack::new`] makes on the calling thread.
pub fn stack_size() -> usize {
    THREAD_STACK_SIZE.with(Cell::get)
}

// `v8::Isolate::SetStackLimit(uintptr_t)`: public V8 API the archive exports and rusty_v8 does
// not bind. A member function takes `this` first in both ABIs.
unsafe extern "C" {
    #[cfg_attr(windows, link_name = "?SetStackLimit@Isolate@v8@@QEAAX_K@Z")]
    #[cfg_attr(not(windows), link_name = "_ZN2v87Isolate13SetStackLimitEm")]
    fn v8_isolate_set_stack_limit(isolate: *mut c_void, limit: usize);
}

/// Initializes V8 for coroutines: checks the patch, fits `--stack-size` to [`STACK_SIZE`] (each
/// stack then sets its own limit, see [`CoroutineStack`]), then starts the platform. Set any
/// other V8 flags before the first call; later calls do nothing.
pub fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let version = unsafe { ffi::v8_coroutine_patch_version() };
        assert_eq!(
            version,
            ffi::PATCH_VERSION,
            "linked V8 has coroutine-local patch version {version}, v8-coroutine-sys expects {}; link the ngot/rusty_v8 release built from that patch version",
            ffi::PATCH_VERSION
        );
        v8::V8::set_flags_from_string(&format!("--stack-size={}", (STACK_SIZE - STACK_MARGIN) / 1024));
        // V8's ReportLiveCodeFromStackForGC calls CheckNoArchivedThreads, which FATALs
        // whenever a coroutine is archived, so wasm code GC must stay off while coroutines
        // suspend. The cost is that dead wasm code is never freed. A V8 patch that visits
        // archived stacks is the follow-up.
        v8::V8::set_flags_from_string("--no-wasm-code-gc");
        // Four background threads, as Node's `--v8-pool-size` default and Deno's platform cap.
        // V8 reads 0 as "one per core minus one", which a script that never compiles in the
        // background pays 23 thread starts for.
        let threads = match PLATFORM_THREADS.load(std::sync::atomic::Ordering::Relaxed) {
            0 => std::thread::available_parallelism().map_or(4, |n| n.get() as u32).min(4),
            set => set,
        };
        let platform = v8::new_default_platform(threads, false).make_shared();
        let _ = PLATFORM.set(PlatformHandle(platform.clone()));
        v8::V8::initialize_platform(platform);
        v8::V8::initialize();
    });
}

/// The platform [`init`] started, kept so [`pump_platform`] can run its foreground tasks.
static PLATFORM: std::sync::OnceLock<PlatformHandle> = std::sync::OnceLock::new();

/// Wraps the platform so the static can hold it. V8 platforms are thread-safe; the handle is
/// only used to pump tasks for an isolate that the calling thread has locked.
struct PlatformHandle(v8::SharedRef<v8::Platform>);

// SAFETY: the default platform guards its task queues with its own locks, and this handle only
// ever calls `pump_message_loop` on it.
unsafe impl Send for PlatformHandle {}
unsafe impl Sync for PlatformHandle {}

/// Runs the foreground tasks V8 posted for `isolate` until none is left, without waiting: V8
/// posts some work, such as `FinalizationRegistry` cleanups, to the platform rather than running
/// it. The caller holds the isolate's lock, and no coroutine of it may be suspended mid-task.
/// Does nothing before [`init`].
pub fn pump_platform(isolate: &v8::Isolate) {
    let Some(handle) = PLATFORM.get() else {
        return;
    };
    while v8::Platform::pump_message_loop(&handle.0, isolate, false) {}
}

/// A coroutine's V8 slot table. `Cell` because V8 writes it through a raw pointer.
type Slots = Rc<[Cell<usize>]>;

fn slots_ptr(slots: &Slots) -> *mut usize {
    slots.as_ptr() as *mut usize
}

/// Reserved size of the [`on_native_stack`] stack; only touched pages are committed.
const NATIVE_STACK_SIZE: usize = 8 * 1024 * 1024;

thread_local! {
    static NATIVE_STACK: Cell<Option<DefaultStack>> = const { Cell::new(None) };
}

/// Runs `f` on a separate stack, so recursion over untrusted input does not depend on how much of
/// the coroutine stack JS has already used: V8 leaves native code only [`STACK_MARGIN`].
/// `f` must not call V8 or suspend. A panic in `f` continues in the caller.
pub fn on_native_stack<R>(f: impl FnOnce() -> R) -> R {
    let stack = NATIVE_STACK.take();
    let mut stack = stack.unwrap_or_else(|| DefaultStack::new(NATIVE_STACK_SIZE).expect("allocate native stack"));
    let result = corosensei::on_stack(&mut stack, f);
    NATIVE_STACK.set(Some(stack));
    result
}

/// A stack plus the V8 identity that lives on it. Reuse it across coroutines (see
/// [`Coroutine::into_stack`]): V8 keeps per-thread metadata for every identity it has seen.
///
/// The first coroutine on a stack sets V8's JS limit for that identity to [`STACK_MARGIN`] above
/// the stack's end, so the limit follows the stack, never `--stack-size`. V8 keeps it with the
/// identity across suspends and reuse, so later coroutines on the stack do not set it again.
pub struct CoroutineStack {
    stack: DefaultStack,
    slots: Slots,
    /// Whether V8 has this stack's JS limit.
    limited: bool,
}

impl CoroutineStack {
    /// A stack of the calling thread's size ([`set_stack_size`]).
    pub fn new() -> Self {
        let stack = DefaultStack::new(stack_size()).expect("allocate coroutine stack");
        let slots: Slots = (0..unsafe { ffi::v8_coroutine_slot_count() }).map(|_| Cell::new(0)).collect();
        unsafe {
            ffi::v8_coroutine_slots_init(
                slots_ptr(&slots),
                stack.base().get() as *mut c_void,
                stack.limit().get() as *mut c_void,
            )
        };
        CoroutineStack { stack, slots, limited: false }
    }

    /// The stack's usable size in bytes.
    pub fn size(&self) -> usize {
        self.stack.base().get() - self.stack.limit().get()
    }

    /// Negative control only: makes coroutines on this stack use `other`'s slot table, which
    /// is what an unpatched V8 does to every coroutine on one OS thread.
    #[doc(hidden)]
    pub fn share_slots_with(&mut self, other: &CoroutineStack) {
        self.slots = other.slots.clone();
    }
}

impl Default for CoroutineStack {
    fn default() -> Self {
        Self::new()
    }
}

/// What a coroutine is resumed with: its next input, or a request to give back the isolate it
/// kept across [`park`].
enum Wake<In> {
    Run(In),
    Release,
}

/// Where [`suspend`] and [`park`] find the running coroutine's yielder and `Locker`. It is the
/// first field of [`Shared`], so a pointer to it also names the coroutine.
struct Current {
    yielder: Cell<*const ()>,
    locker: Cell<*mut ()>,
    input: TypeId,
    slots: *mut usize,
    /// Resumes this coroutine with [`Wake::Release`]; set by [`Coroutine::new`], which knows `R`.
    release: unsafe fn(*const Current),
}

/// A coroutine's state at a stable address: the pool that owns a [`Coroutine`] may move it
/// while it is parked, but a parked holder must stay reachable to be released.
#[repr(C)]
struct Shared<In: 'static, R: 'static> {
    current: Current,
    co: UnsafeCell<ManuallyDrop<Raw<In, R>>>,
}

/// The corosensei coroutine under a [`Coroutine`].
type Raw<In, R> = RawCoroutine<Wake<In>, (), R, DefaultStack>;

thread_local! {
    static CURRENT: Cell<*const Current> = const { Cell::new(null()) };
    /// The coroutine parked by [`park`] with the isolate still locked and entered, if any.
    static HELD: Cell<*const Current> = const { Cell::new(null()) };
    /// How many coroutines are being resumed on this thread, one inside another.
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// A coroutine that holds a `v8::Locker` on a shared isolate while it runs.
pub struct Coroutine<In: 'static, R: 'static> {
    shared: Rc<Shared<In, R>>,
    slots: Slots,
}

impl<In: 'static, R: 'static> Coroutine<In, R> {
    /// `f` runs on the coroutine with the isolate locked and entered. The first [`Coroutine::resume`]
    /// input is passed to `f`; later inputs are returned by [`suspend`].
    pub fn new(
        stack: CoroutineStack,
        isolate: Rc<v8::SharedIsolate>,
        f: impl FnOnce(&mut v8::Locker, In) -> R + 'static,
    ) -> Self {
        let slots = slots_ptr(&stack.slots);
        let js_limit = (!stack.limited).then(|| stack.stack.limit().get() + STACK_MARGIN);
        let shared = Rc::new_cyclic(|weak: &std::rc::Weak<Shared<In, R>>| {
            // The body reaches its `Current` through the allocation it lives in; a `Weak` keeps
            // the coroutine from owning itself.
            let cur = weak.as_ptr() as *const Current;
            let co = RawCoroutine::with_stack(stack.stack, move |yielder: &Yielder<Wake<In>, ()>, wake: Wake<In>| {
                let Wake::Run(input) = wake else {
                    unreachable!("a coroutine that never ran was asked to release the isolate")
                };
                let mut locker = isolate.lock();
                if let Some(limit) = js_limit {
                    // SAFETY: the isolate is locked and entered on this coroutine's V8 thread,
                    // and `limit` lies inside this coroutine's stack.
                    unsafe { v8_isolate_set_stack_limit(v8::coroutine_local::isolate_ptr(&locker), limit) };
                }
                // SAFETY: the `Shared` outlives its coroutine's body.
                let cur = unsafe { &*cur };
                cur.yielder.set(yielder as *const Yielder<Wake<In>, ()> as *const ());
                cur.locker.set(&mut locker as *mut v8::Locker as *mut ());
                f(&mut locker, input)
            });
            Shared {
                current: Current {
                    yielder: Cell::new(null()),
                    locker: Cell::new(null_mut()),
                    input: TypeId::of::<In>(),
                    slots,
                    release: release_holder::<In, R>,
                },
                co: UnsafeCell::new(ManuallyDrop::new(co)),
            }
        });
        Coroutine { shared, slots: stack.slots }
    }

    /// Runs the coroutine until it calls [`suspend`] or [`park`] (`None`) or returns (`Some`).
    ///
    /// A different coroutine parked holding the isolate is first resumed just long enough to give
    /// it back on its own stack, so this coroutine's `Locker` never waits on a lock whose owner
    /// is not running.
    pub fn resume(&mut self, input: In) -> Option<R> {
        let mine = &self.shared.current as *const Current;
        let held = HELD.with(|h| h.replace(null()));
        if !held.is_null() && held != mine {
            // SAFETY: a holder clears `HELD` before it can be dropped (see `Drop`).
            unsafe { ((*held).release)(held) };
        }
        // SAFETY: `self` is borrowed mutably, so nothing else resumes this coroutine.
        match unsafe { resume_raw(&self.shared, Wake::Run(input)) } {
            CoroutineResult::Yield(()) => None,
            CoroutineResult::Return(r) => Some(r),
        }
    }

    pub fn done(&self) -> bool {
        // SAFETY: a shared read; resumes need `&mut self`.
        unsafe { (*self.shared.co.get()).done() }
    }

    /// Returns the stack (and its V8 identity) of a finished coroutine for reuse.
    pub fn into_stack(self) -> CoroutineStack {
        assert!(self.done(), "into_stack on an unfinished coroutine");
        let this = ManuallyDrop::new(self);
        // SAFETY: `this` is never used or dropped again, so each field is moved out once.
        let (shared, slots) = unsafe { (std::ptr::read(&this.shared), std::ptr::read(&this.slots)) };
        let Ok(shared) = Rc::try_unwrap(shared) else { unreachable!("a finished coroutine's state has one owner") };
        let co = ManuallyDrop::into_inner(shared.co.into_inner());
        CoroutineStack { stack: co.into_stack(), slots, limited: true }
    }
}

/// Resumes `shared`'s coroutine with `wake` on its own slot table, as the running coroutine.
///
/// # Safety
///
/// Nothing else may be resuming the same coroutine.
///
/// Inlined so a resume is one frame fewer: a coroutine that makes a system call deep in its
/// stack lets the kernel overwrite the CPU's return address stack, so every frame under the
/// switch adds a mispredicted return on the way back out.
#[inline(always)]
unsafe fn resume_raw<In, R>(shared: &Shared<In, R>, wake: Wake<In>) -> CoroutineResult<(), R> {
    struct Restore(*const Current);
    impl Drop for Restore {
        fn drop(&mut self) {
            // Back to the resumer's V8 slot table, also if the coroutine panicked: the OS
            // thread's own (null) or, for a nested resume, the outer coroutine's.
            let outer = self.0;
            let slots = if outer.is_null() { null_mut() } else { unsafe { (*outer).slots } };
            unsafe { ffi::v8_coroutine_slots_use(slots) };
            CURRENT.with(|c| c.set(outer));
            DEPTH.with(|d| d.set(d.get() - 1));
        }
    }
    DEPTH.with(|d| d.set(d.get() + 1));
    let _restore = Restore(CURRENT.with(|c| c.replace(&shared.current)));
    unsafe { ffi::v8_coroutine_slots_use(shared.current.slots) };
    unsafe { (*shared.co.get()).resume(wake) }
}

/// The type-erased [`Current::release`]: resumes a parked holder so it unlocks on its own stack.
unsafe fn release_holder<In: 'static, R: 'static>(cur: *const Current) {
    // SAFETY: `cur` is the first field of a live `Shared<In, R>` (`repr(C)`).
    let shared = unsafe { &*(cur as *const Shared<In, R>) };
    match unsafe { resume_raw(shared, Wake::Release) } {
        CoroutineResult::Yield(()) => {}
        CoroutineResult::Return(_) => unreachable!("a coroutine finished while releasing"),
    }
}

/// Gives back the isolate a coroutine kept across [`park`], so code outside every coroutine can
/// take its own `Locker` (an inspector session, isolate teardown). Does nothing when no
/// coroutine is parked holding it. Call it where no coroutine is running.
pub fn release() {
    let held = HELD.with(|h| h.replace(null()));
    if !held.is_null() {
        // SAFETY: as in `Coroutine::resume`.
        unsafe { ((*held).release)(held) };
    }
}

impl<In: 'static, R: 'static> Drop for Coroutine<In, R> {
    fn drop(&mut self) {
        if std::ptr::eq(HELD.with(Cell::get), &self.shared.current) {
            release();
        }
        if self.done() {
            // SAFETY: the coroutine is finished and never touched again.
            unsafe { ManuallyDrop::drop(&mut *self.shared.co.get()) };
        }
        // ponytail: an unfinished coroutine is leaked, never unwound: unwinding would run
        // through V8 frames. Finish coroutines before dropping them.
    }
}

fn running() -> &'static Current {
    let cur = CURRENT.with(Cell::get);
    assert!(!cur.is_null(), "v8_coroutine::suspend called outside a coroutine");
    // SAFETY: `cur` belongs to the coroutine running this code, which outlives this call.
    unsafe { &*cur }
}

fn check_input<In: 'static>(cur: &Current) {
    assert_eq!(cur.input, TypeId::of::<In>(), "v8_coroutine::suspend::<In> does not match the coroutine's input type");
}

/// Suspends the running coroutine until the next [`Coroutine::resume`], whose input it returns.
/// The isolate is unlocked meanwhile (`v8::Unlocker`), so other coroutines can run JS.
///
/// Call it on a coroutine's own stack, typically from a native callback. `In` must be the
/// coroutine's input type.
pub fn suspend<In: 'static>() -> In {
    let cur = running();
    check_input::<In>(cur);
    let yielder = cur.yielder.get() as *const Yielder<Wake<In>, ()>;
    let locker = cur.locker.get() as *mut v8::Locker<'static>;
    // SAFETY: both point into the running coroutine's frame. Re-borrowing the Locker while
    // the coroutine body also holds it mirrors rusty_v8's own callbacks re-deriving the isolate.
    unsafe { (*locker).unlock(|| wait(&*yielder)) }
}

/// Suspends like [`suspend`] but keeps the isolate locked and entered, for a coroutine that is
/// likely the next one resumed, such as a pooled worker coroutine between two jobs: resuming it then
/// costs neither the lock nor V8's thread switch. Any other coroutine's resume, or [`release`],
/// first resumes this one briefly so it unlocks on its own stack, where V8 requires it.
///
/// Inside a nested resume it is a plain [`suspend`].
pub fn park<In: 'static>() -> In {
    let cur = running();
    check_input::<In>(cur);
    if DEPTH.with(Cell::get) != 1 || !HELD.with(Cell::get).is_null() {
        return suspend();
    }
    let yielder = cur.yielder.get() as *const Yielder<Wake<In>, ()>;
    let locker = cur.locker.get() as *mut v8::Locker<'static>;
    HELD.with(|h| h.set(cur));
    // SAFETY: as in `suspend`.
    match unsafe { (*yielder).suspend(()) } {
        Wake::Run(input) => input,
        // Someone else needs V8: unlock here, on this stack, and wait as `suspend` does.
        Wake::Release => unsafe { (*locker).unlock(|| wait(&*yielder)) },
    }
}

/// Yields until the coroutine is resumed with an input. A release request finds nothing to give
/// back while the isolate is already unlocked, so it just yields again.
fn wait<In>(yielder: &Yielder<Wake<In>, ()>) -> In {
    loop {
        if let Wake::Run(input) = yielder.suspend(()) {
            return input;
        }
    }
}
