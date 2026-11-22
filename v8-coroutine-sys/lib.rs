//! rusty_v8, linked against the V8 archive built with the coroutine-local patch
//! (`coroutine/coroutine-local.patch` on the `coroutine` branch of github.com/ngot/rusty_v8).
//! Re-exported whole, so dependents keep writing `v8::`.

pub use rusty_v8::*;

/// The functions the coroutine-local patch exports.
pub mod coroutine_local {
    use std::ffi::{c_int, c_void};

    /// Must equal `V8_COROUTINE_PATCH_VERSION` in the patch the linked archive was built with.
    pub const PATCH_VERSION: i32 = 13;

    unsafe extern "C" {
        pub fn v8_coroutine_patch_version() -> c_int;
        pub fn v8_coroutine_slot_count() -> usize;
        pub fn v8_coroutine_slots_init(slots: *mut usize, stack_start: *mut c_void, stack_limit: *mut c_void);
        pub fn v8_coroutine_slots_use(slots: *mut usize);
        /// Jobs in the isolate's default microtask queue.
        pub fn v8_coroutine_microtask_count(isolate: *mut c_void) -> usize;
        /// The owner of the default queue's first job (0 for none), or -1 when it is empty.
        pub fn v8_coroutine_microtask_peek(isolate: *mut c_void) -> isize;
        /// Moves the default queue's first job into the empty, idle queue `to` and stores the
        /// job's owner (a Smi set by `v8_coroutine_microtask_owner`, else 0): 1 when moved, 0
        /// when there is none or the isolate is terminating, -1 when `to` is busy.
        pub fn v8_coroutine_microtask_take(isolate: *mut c_void, to: *mut c_void, owner: *mut isize) -> c_int;
        /// Sets the owner that jobs queued from now on carry; 0 clears it. Must fit in 31 bits.
        pub fn v8_coroutine_microtask_owner(isolate: *mut c_void, owner: isize);
    }

    /// The `v8::Isolate*` the microtask exports take.
    pub fn isolate_ptr(isolate: &rusty_v8::Isolate) -> *mut c_void {
        // `Isolate` and `UnsafeRawIsolatePtr` are both `repr(transparent)` over the pointer.
        unsafe { std::mem::transmute(isolate.as_raw_isolate_ptr()) }
    }

    /// The `v8::MicrotaskQueue*` the microtask exports take.
    pub fn queue_ptr(queue: &rusty_v8::MicrotaskQueue) -> *mut c_void {
        queue as *const rusty_v8::MicrotaskQueue as *mut c_void
    }
}

/// V8's own trace events (GC phases, compilation, execution), recorded through the platform's
/// libplatform `TracingController` and returned as Chrome trace JSON objects.
pub mod trace {
    use std::ffi::{CString, c_char, c_int};

    unsafe extern "C" {
        fn v8_trace_start(categories: *const *const c_char, count: usize) -> c_int;
        fn v8_trace_stop();
        fn v8_trace_take(data: *mut *const c_char) -> usize;
        fn v8_trace_now_us() -> i64;
    }

    /// Records the given categories, replacing any earlier list. False when the platform has no
    /// libplatform controller (before `V8::initialize_platform`).
    pub fn start(categories: &[String]) -> bool {
        let owned: Vec<CString> = categories.iter().filter_map(|name| CString::new(name.as_str()).ok()).collect();
        let pointers: Vec<*const c_char> = owned.iter().map(|name| name.as_ptr()).collect();
        unsafe { v8_trace_start(pointers.as_ptr(), pointers.len()) != 0 }
    }

    /// Stops recording; what was recorded stays for [`take`].
    pub fn stop() {
        unsafe { v8_trace_stop() }
    }

    /// V8's trace clock in microseconds, the base of the timestamps [`take`] returns.
    pub fn now_us() -> i64 {
        unsafe { v8_trace_now_us() }
    }

    /// The events recorded since the last call, as comma separated JSON objects.
    pub fn take() -> String {
        let mut data: *const c_char = std::ptr::null();
        let len = unsafe { v8_trace_take(&mut data) };
        if data.is_null() || len == 0 {
            return String::new();
        }
        String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(data.cast::<u8>(), len) }).into_owned()
    }
}

/// Reusable own-property layouts, the fast-object shape stock V8 cannot hand an embedder.
///
/// A shape is a V8 map with a chosen prototype, including null, and a fixed list of own data
/// properties in fast mode, plus the field offset of each property. An object allocated from a
/// shape needs no name hashing and no map transition per property: one allocation and one store
/// per field. The map is kept alive by a `Global` handle inside the shape, so it survives every
/// GC, and the shape must outlive every object allocated from it.
///
/// The shape is affine to the isolate that created it and must be released before that isolate is
/// disposed, which `Drop` does.
pub mod object_shape {
    use std::ffi::c_void;

    use rusty_v8 as v8;

    unsafe extern "C" {
        /// Builds a shape, or returns null when the layout cannot be a fast map.
        fn v8_object_shape_new(
            isolate: *mut c_void,
            prototype_or_null: *const c_void,
            names: *const *const c_void,
            count: usize,
        ) -> *mut c_void;
        fn v8_object_shape_release(shape: *mut c_void);
        fn v8_object_shape_field_count(shape: *const c_void) -> usize;
        /// Allocates one object from the shape and fills every field, in one step.
        fn v8_object_shape_new_object(
            isolate: *mut c_void,
            shape: *const c_void,
            values: *const *const c_void,
            count: usize,
        ) -> *const c_void;
        /// Builds many rows from one shape in a single call.
        fn v8_object_shape_new_objects(
            isolate: *mut c_void,
            shape: *const c_void,
            fields: *const u32,
            columns: usize,
            kinds: *const u8,
            numbers: *const f64,
            values: *const *const c_void,
            value_count: usize,
            rows: usize,
        ) -> *const c_void;
    }

    /// The shape's properties, in the order their names were given.
    pub struct ObjectShape {
        raw: *mut c_void,
    }

    impl ObjectShape {
        /// Builds a shape whose own properties are `names`, in that order, under `prototype`.
        ///
        /// `prototype` is `None` for a null prototype. Returns `None` when V8 cannot keep the
        /// layout in fast mode: no names, 128 names or more, an array-index name such as `"0"`, a
        /// repeated name, or a property that would not fit in the object body.
        #[must_use]
        pub fn new<'s, 'i>(
            scope: &v8::PinScope<'s, 'i>,
            prototype: Option<v8::Local<'s, v8::Value>>,
            names: &[v8::Local<'s, v8::String>],
        ) -> Option<Self> {
            // `Local<T>` is `repr(C)` over one pointer, so a slice of handles is the pointer
            // array the export reads.
            let count = names.len();
            let names = names.as_ptr() as *const *const v8::String as *const *const c_void;
            let prototype = prototype.map_or(std::ptr::null(), |value| &*value as *const v8::Value as *const c_void);
            let raw = unsafe { v8_object_shape_new(isolate_ptr(scope), prototype, names, count) };
            (!raw.is_null()).then_some(Self { raw })
        }

        /// The number of own properties, which is also the number of values a row needs.
        #[must_use]
        pub fn len(&self) -> usize {
            unsafe { v8_object_shape_field_count(self.raw) }
        }

        #[must_use]
        pub fn is_empty(&self) -> bool {
            self.len() == 0
        }

        /// Allocates one object from the shape and writes `values` to its fields in order.
        ///
        /// Returns `None` when `values` is not exactly `len()` long or V8 cannot allocate.
        #[must_use]
        pub fn new_object<'s>(
            &self,
            scope: &v8::PinScope<'s, '_>,
            values: &[v8::Local<'s, v8::Value>],
        ) -> Option<v8::Local<'s, v8::Object>> {
            if values.len() != self.len() {
                return None;
            }
            let values = values.as_ptr() as *const *const v8::Value as *const *const c_void;
            let object = unsafe { v8_object_shape_new_object(isolate_ptr(scope), self.raw, values, self.len()) };
            // SAFETY: the export built the handle in the caller's current handle scope, which is
            // `scope`, and `Local` is `repr(C)` over the one pointer V8's `Local` holds.
            (!object.is_null()).then(|| unsafe { std::mem::transmute(object) })
        }

        /// Builds `rows` objects from a cell plan and returns them as an array.
        ///
        /// `kinds` and `numbers` hold one entry per cell in row-major order, `fields` one field
        /// index per column, and `values` the handle for every cell whose kind is 2. Kind 0 is
        /// null, kind 1 is the number from `numbers`, and kind 2 is the next value from `values`.
        ///
        /// This exists for results large enough that a call per row would spend its time in
        /// handle-scope bookkeeping: the rows and their boxed numbers live inside the call.
        #[must_use]
        pub fn new_objects<'s>(
            &self,
            scope: &v8::PinScope<'s, '_>,
            fields: &[u32],
            kinds: &[u8],
            numbers: &[f64],
            values: &[v8::Local<'s, v8::Value>],
            rows: usize,
        ) -> Option<v8::Local<'s, v8::Array>> {
            if fields.is_empty() || kinds.len() != rows * fields.len() {
                return None;
            }
            let value_count = values.len();
            let values = values.as_ptr() as *const *const v8::Value as *const *const c_void;
            let array = unsafe {
                v8_object_shape_new_objects(
                    isolate_ptr(scope),
                    self.raw,
                    fields.as_ptr(),
                    fields.len(),
                    kinds.as_ptr(),
                    numbers.as_ptr(),
                    values,
                    value_count,
                    rows,
                )
            };
            // SAFETY: the export escaped the array into the caller's scope, which is `scope`.
            (!array.is_null()).then(|| unsafe { std::mem::transmute(array) })
        }
    }

    impl Drop for ObjectShape {
        fn drop(&mut self) {
            unsafe { v8_object_shape_release(self.raw) };
        }
    }

    fn isolate_ptr(scope: &v8::PinScope<'_, '_>) -> *mut c_void {
        // `Isolate` and `UnsafeRawIsolatePtr` are both `repr(transparent)` over the pointer.
        unsafe { std::mem::transmute(scope.as_raw_isolate_ptr()) }
    }
}
