use std::{
    collections::HashMap,
    ffi::{CStr, CString, c_char, c_void},
    path::Path,
    sync::Arc,
};

use jade_runtime::coll::{DictObj, StructObj};

use crate::{
    builtins::make_array,
    frontend::error::{JadeError, Result, Span},
    vm::{Mutex, VmValue},
};

// The transport trees for arrays/dicts are libc-heap so either `jade-runtime`
// instance in the process (this VM, and each dlopen'd package — each with its own
// mimalloc) can free them; see the JadeArr/JadeMap note below and runtime_aot's
// runtime.h for the full rationale.
unsafe extern "C" {
    fn malloc(size: usize) -> *mut c_void;
    fn free(ptr: *mut c_void);
}

// ── C-ABI tag constants ───────────────────────────────────────────────────────

pub const JADE_TAG_NIL: u8 = 0;
pub const JADE_TAG_INT: u8 = 1;
pub const JADE_TAG_FLOAT: u8 = 2;
pub const JADE_TAG_BOOL: u8 = 3;
/// Null-terminated UTF-8.  For *input* args, jade owns the buffer.
/// For *output* vals, the native lib owns the buffer (must stay valid through
/// the return of the native function — jade copies immediately).
pub const JADE_TAG_STR: u8 = 4;
/// Like JADE_TAG_STR but signals an error.  The str is the error message.
pub const JADE_TAG_ERROR: u8 = 5;
/// `data.as_arr` → a deep-copied [`JadeArr`] tree (libc-owned).
pub const JADE_TAG_ARRAY: u8 = 6;
/// `data.as_dict` → a deep-copied [`JadeMap`] tree (libc-owned).
pub const JADE_TAG_DICT: u8 = 7;
/// `data.as_struct` → a deep-copied [`JadeStruct`] tree (libc-owned).
///
/// A struct is a dict that also carries its type name. The name is what makes a
/// typed contract enforceable across the boundary: a receiver can refuse a
/// struct that is not the type it expects, where a bare dict with the wrong keys
/// reads as a set of nils and fails silently.
pub const JADE_TAG_STRUCT: u8 = 8;
/// `data.as_bytes` → a libc-owned [`JadeBytes`] copy of a binary blob.
///
/// Bytes cannot ride on [`JADE_TAG_STR`]: that is a NUL-terminated `char*`, so
/// a blob containing a NUL would be truncated at it and one that is not valid
/// UTF-8 would be corrupted by anything on the far side that assumed text. A
/// counted buffer is the only representation that survives the trip.
///
/// Added in v1.2.2, which is why [`jade_runtime::RUNTIME_ABI_VERSION`] moved to
/// 3: a package built against ABI 2 has no arm for this tag.
pub const JADE_TAG_BYTES: u8 = 9;
/// `data.as_handle` → a libc-owned [`JadeHandle`] wrapper around a foreign
/// pointer.
///
/// The pointer is the package's, not Jade's. [`ffi_free`] releases the wrapper
/// and the copied type name and leaves `ptr` alone — Jade cannot know what the
/// pointee is or which allocator made it, and closing it is a call the binding
/// exposes.
///
/// Added in v1.3.0, which is why [`jade_runtime::RUNTIME_ABI_VERSION`] moved to
/// 4: a package built against ABI 3 has no arm for this tag.
pub const JADE_TAG_HANDLE: u8 = 10;
/// `data.as_fn` → a [`JadeFn`]: a Jade function a C library may call back.
///
/// The value carries its own `invoke` pointer instead of the package calling an
/// agreed host symbol, because the two engines re-enter in completely different
/// ways — compiled code calls a lowered function directly, while the VM cannot
/// be re-entered from a C frame at all and has to post the call to the
/// interpreter and wait. One agreed symbol would have suited neither.
pub const JADE_TAG_FN: u8 = 11;
/// `data.as_char` → a Unicode scalar, with the trust bit in `_pad[0]`.
///
/// `char` is a first-class Jade type — `for c in "jade"` yields one — and until
/// v1.3.10 there was no way to move one across this boundary in any position.
/// The gap surfaced on struct fields: a C `char[32]` is an array of characters,
/// and an array of characters needs characters.
///
/// Not folded into [`JADE_TAG_INT`]. A char is not a number: it compares,
/// prints, and concatenates as text, and arriving as an integer would make the
/// receiving side guess which of the two it was holding. That guess is the
/// silent-wrong-answer failure this ABI carries tags to avoid.
///
/// Added in v1.3.10, which is why [`jade_runtime::RUNTIME_ABI_VERSION`] moved to
/// 5: a package built against ABI 4 has no arm for this tag.
pub const JADE_TAG_CHAR: u8 = 12;

// ── Value type ────────────────────────────────────────────────────────────────

#[repr(C)]
pub union JadeValData {
    pub as_int: i64,
    pub as_float: f64,
    pub as_bool: u8,
    /// Non-owning pointer to null-terminated UTF-8.
    pub as_str: *const u8,
    /// Padding — present for JADE_TAG_NIL.
    pub as_nil: u64,
    pub as_arr: *mut JadeArr,
    pub as_dict: *mut JadeMap,
    pub as_struct: *mut JadeStruct,
    pub as_bytes: *mut JadeBytes,
    pub as_handle: *mut JadeHandle,
    pub as_fn: *mut JadeFn,
    /// A Unicode scalar. 32 bits rather than the 21 a scalar needs, because a
    /// union member that is not a natural width is a portability question
    /// nobody wants to answer twice.
    pub as_char: u32,
}

#[repr(C)]
pub struct JadeVal {
    pub tag: u8,
    pub _pad: [u8; 7],
    pub data: JadeValData,
}

/// A collection cannot cross the FFI by pointer — the process holds two
/// `jade-runtime` instances (this VM and each dlopen'd package), each with its
/// own mimalloc, so a word owned by one must never be freed by the other. Arrays
/// and dicts are therefore *deep-copied* into these trees, every node allocated
/// with libc `malloc`/strdup — a process-shared allocator (mimalloc is a Rust
/// `#[global_allocator]`; it does not override the C `malloc`) — so either side
/// releases the whole tree with [`ffi_free`]. Layout mirrors `JadeArr`/`JadeMap`
/// in runtime_aot's runtime.h. Cyclic collections are unsupported.
#[repr(C)]
pub struct JadeArr {
    pub items: *mut JadeVal,
    pub len: usize,
}

#[repr(C)]
pub struct JadeMap {
    pub keys: *mut *const u8,
    pub vals: *mut JadeVal,
    pub len: usize,
}

/// A [`JadeMap`] plus the struct's type name, in declaration order. Same
/// ownership rules: every node is libc heap, released by [`ffi_free`].
#[repr(C)]
pub struct JadeStruct {
    /// Null-terminated UTF-8, libc-owned.
    pub type_name: *const u8,
    pub keys: *mut *const u8,
    pub vals: *mut JadeVal,
    pub len: usize,
}

/// A counted binary buffer. Same ownership rules as [`JadeArr`]: libc heap,
/// released by [`ffi_free`]. Counted rather than NUL-terminated because a blob
/// may contain NUL bytes and is not required to be valid UTF-8.
#[repr(C)]
pub struct JadeBytes {
    pub data: *mut u8,
    pub len: usize,
}

/// A foreign pointer plus the C type it came from. Layout mirrors `JadeHandle`
/// in runtime_aot's runtime.h.
///
/// Ownership splits, and that split is the whole subtlety of this tag: the
/// wrapper and `type_name` are libc heap released by [`ffi_free`], while `ptr`
/// is owned by the package that produced it and is never freed here.
#[repr(C)]
pub struct JadeHandle {
    pub ptr: *mut c_void,
    /// Null-terminated UTF-8, libc-owned.
    pub type_name: *const u8,
}

/// A Jade function a C library can call back into. Layout mirrors `JadeFn` in
/// runtime_aot's runtime.h.
///
/// `invoke` answers 0 on success with the result in `out`, non-zero when the
/// Jade side raised with `out` a `JADE_TAG_ERROR`. A raise must never leave
/// `invoke`: it is a `longjmp` in compiled code and a `Result` in the VM, and
/// either escaping into the C library's frames would unwind past whatever the
/// library was in the middle of.
#[repr(C)]
pub struct JadeFn {
    pub host: *mut c_void,
    pub invoke: Option<
        unsafe extern "C" fn(
            host: *mut c_void,
            argc: usize,
            argv: *const JadeVal,
            out: *mut JadeVal,
        ) -> i32,
    >,
}

impl JadeVal {
    pub fn nil() -> Self {
        JadeVal { tag: JADE_TAG_NIL, _pad: [0; 7], data: JadeValData { as_nil: 0 } }
    }
}

// ── Package descriptor ────────────────────────────────────────────────────────

/// Function pointer type used by the native ABI.
pub type JadeNativeFnPtr =
    unsafe extern "C" fn(argc: usize, argv: *const JadeVal, out: *mut JadeVal) -> i32;

/// Single exported function binding returned by `jade_pkg_init`.
#[repr(C)]
pub struct JadeBinding {
    /// Null-terminated ASCII/UTF-8 name.
    pub name: *const std::ffi::c_char,
    pub func: JadeNativeFnPtr,
}

/// Top-level descriptor written into the `out` pointer by `jade_pkg_init`.
#[repr(C)]
pub struct JadeNativePkg {
    /// Null-terminated package name (informational).
    pub name: *const std::ffi::c_char,
    pub bindings: *const JadeBinding,
    pub binding_count: usize,
}

// ── NativeLibFn — a callable that wraps a native ABI function ─────────────────

pub struct NativeLibFn {
    pub name: String,
    fn_ptr: JadeNativeFnPtr,
    /// Keep the library loaded for as long as any of its functions are alive.
    _lib: Arc<libloading::Library>,
}

// ── Callbacks ─────────────────────────────────────────────────────────────────

/// One call from a C callback back into the interpreter.
///
/// The VM cannot be re-entered from a C frame: calling a Jade function needs
/// `VmState` and an async context, and the C library is holding the stack. So
/// the native call runs on a worker thread and the callback *posts* here; the
/// interpreter services it from its own loop and sends the answer back. The
/// worker blocks in between, which is exactly what it is for.
///
/// This is the same inversion `uhttp.stream` uses, in both directions.
pub(crate) struct CallbackRequest {
    pub callee: VmValue,
    pub args: Vec<VmValue>,
    pub reply: tokio::sync::oneshot::Sender<Result<VmValue>>,
    /// Where the native call was written, so an error raised inside the
    /// callback points at the call the user can see rather than at nothing.
    pub span: Span,
}

/// What one `JadeFn`'s `host` pointer addresses: the Jade function to call, and
/// the way back to the interpreter.
pub(crate) struct CallbackHost {
    callee: VmValue,
    bus: std::sync::Arc<CallbackBus>,
    span: Span,
}

/// Where a Jade function lives once a C library has been given it.
///
/// One per VM rather than one per call, and that is the whole feature. A
/// library that *stores* a callback — an async request, a watcher — invokes it
/// from some later call entirely: `ares_search` registers and returns, and the
/// answer arrives during `ares_process`. With a channel scoped to the
/// registering call there is nobody listening by then, so the callback found a
/// closed channel and the Jade function was freed underneath it. It bound,
/// compiled, ran and did nothing.
///
/// Three things live here, and each answers a way that could go wrong.
///
/// `rx` is behind an async mutex rather than owned by the in-flight call,
/// because callbacks nest. While a serve loop awaits a Jade callback it must
/// not be holding the receiver — that callback may itself call a native
/// function, and that call needs to serve its own callbacks or it hangs. The
/// lock is taken across `recv` and nothing else.
///
/// `serving` counts the loops currently draining. A callback fired when none
/// are gets the neutral answer, exactly as it did when the channel closed at
/// the end of a call. Without the count it would block forever instead, since
/// the channel now never closes.
///
/// `live` owns every host and wrapper handed out. A stored callback is one the
/// library may invoke at any later moment, so there is no point at which
/// releasing it is safe — nothing in C says when a library is finished with
/// one. They live until the VM does. That is a bounded, deliberate leak: one
/// per callback-passing call, not one per invocation.
pub(crate) struct CallbackBus {
    tx: tokio::sync::mpsc::Sender<CallbackRequest>,
    rx: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<CallbackRequest>>,
    serving: std::sync::atomic::AtomicUsize,
    live: parking_lot::Mutex<CallbackRegistry>,
}

#[derive(Default)]
struct CallbackRegistry {
    /// `Box` is load-bearing: a raw pointer to each host crosses to the library,
    /// and a `Vec<CallbackHost>` would move its elements on the next
    /// reallocation and leave those pointers addressing freed memory.
    #[allow(clippy::vec_box)]
    hosts: Vec<Box<CallbackHost>>,
    wrappers: Vec<*mut JadeFn>,
}

impl Drop for CallbackRegistry {
    fn drop(&mut self) {
        // libc `free`, because `malloc` made them — the process holds two
        // allocators and they must not free each other's memory.
        for w in self.wrappers.drain(..) {
            unsafe { free(w as *mut c_void) };
        }
    }
}

// SAFETY: a host is reached only through a raw pointer held by C, and only one
// thread is inside a given callback at a time — the worker that the library
// called it from. The boxes never move, and the registry is behind a mutex.
unsafe impl Send for CallbackBus {}
unsafe impl Sync for CallbackBus {}

/// Held for as long as a loop is draining the bus. Its only job is to make the
/// count exact whichever way the loop exits.
pub(crate) struct ServeGuard(std::sync::Arc<CallbackBus>);

impl Drop for ServeGuard {
    fn drop(&mut self) {
        self.0.serving.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl CallbackBus {
    pub(crate) fn new() -> std::sync::Arc<Self> {
        // Depth 1 and not a queue: a callback blocks its worker until it is
        // answered, so at most one request is outstanding per serving loop.
        let (tx, rx) = tokio::sync::mpsc::channel::<CallbackRequest>(1);
        std::sync::Arc::new(CallbackBus {
            tx,
            rx: tokio::sync::Mutex::new(rx),
            serving: std::sync::atomic::AtomicUsize::new(0),
            live: parking_lot::Mutex::new(CallbackRegistry::default()),
        })
    }

    /// Whether any Jade function has been handed to a library on this VM.
    ///
    /// What makes a call that passes no function still take the serving path:
    /// once something is registered, any native call may be the one the library
    /// calls back from.
    pub(crate) fn has_live(&self) -> bool {
        !self.live.lock().hosts.is_empty()
    }

    pub(crate) fn serving(self: &std::sync::Arc<Self>) -> ServeGuard {
        self.serving.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        ServeGuard(std::sync::Arc::clone(self))
    }

    fn is_serving(&self) -> bool {
        self.serving.load(std::sync::atomic::Ordering::SeqCst) > 0
    }

    /// Take the next request. The lock is released before the caller runs Jade
    /// code, so a native call made from inside a callback can serve its own.
    pub(crate) async fn recv(&self) -> Option<CallbackRequest> {
        self.rx.lock().await.recv().await
    }
}

/// Whether a value is something a C library could call.
pub(crate) fn is_callable(v: &VmValue) -> bool {
    matches!(
        v,
        VmValue::Fn(_)
            | VmValue::Closure(_, _)
            | VmValue::BoundMethod(_)
            | VmValue::BuiltinFn(_)
            | VmValue::NativeBoundMethod(_)
    )
}

/// The `invoke` a VM-supplied [`JadeFn`] carries.
///
/// Runs on the worker thread the native call was moved to, never on the
/// interpreter's. Both channel operations are the blocking flavours for that
/// reason.
///
/// # Safety
/// `host` must point at a live [`CallbackHost`]; `argv` at `argc` values.
unsafe extern "C" fn vm_invoke_callback(
    host: *mut c_void,
    argc: usize,
    argv: *const JadeVal,
    out: *mut JadeVal,
) -> i32 {
    let h = unsafe { &*(host as *const CallbackHost) };

    let mut args = Vec::with_capacity(argc);
    for i in 0..argc {
        match ffi_to_vm(unsafe { &*argv.add(i) }, h.span) {
            Ok(v) => args.push(v),
            Err(_) => return 1,
        }
    }

    // Nobody draining means nobody will ever answer, and the channel no longer
    // closes at the end of a call to say so — it lives as long as the VM. So
    // the check that used to be a failed `send` is an explicit one, and the
    // outcome is the same: the neutral answer rather than a worker blocked for
    // good.
    //
    // A library calling back from a thread of its own lands here. That is not
    // supported and this is where it says so.
    if !h.bus.is_serving() {
        return 1;
    }

    let (reply, wait) = tokio::sync::oneshot::channel();
    let req = CallbackRequest { callee: h.callee.clone(), args, reply, span: h.span };
    if h.bus.tx.blocking_send(req).is_err() {
        // The interpreter stopped listening: the native call is already
        // unwinding. Report failure rather than block forever.
        return 1;
    }

    match wait.blocking_recv() {
        Ok(Ok(v)) => {
            // Scalars only, so there is nothing for the caller to free. A
            // callback returning a container would raise the question of who
            // releases it inside a C frame, for no use anyone has asked for.
            unsafe { *out = vm_to_ffi_owned(&v) };
            match unsafe { (*out).tag } {
                JADE_TAG_NIL | JADE_TAG_INT | JADE_TAG_FLOAT | JADE_TAG_BOOL => 0,
                _ => {
                    unsafe { ffi_free(&*out) };
                    unsafe { *out = JadeVal::nil() };
                    1
                }
            }
        }
        // A raise inside the callback. It must not travel out of here — the C
        // library is mid-operation and unwinding through its frames would leave
        // it however it happens to be. The shim turns this into a Jade error
        // once the native call has returned normally.
        Ok(Err(_)) | Err(_) => 1,
    }
}

impl NativeLibFn {
    pub fn call(&self, args: &[VmValue], span: Span) -> Result<VmValue> {
        // Build FFI args.  CStrings back top-level string args and must outlive
        // the native call; array/dict args are libc-heap trees freed below.
        let mut cstrings: Vec<CString> = Vec::new();
        let ffi_args: Vec<JadeVal> = args.iter().map(|v| vm_to_ffi(v, &mut cstrings)).collect();

        let mut out = JadeVal::nil();
        let status = unsafe { (self.fn_ptr)(ffi_args.len(), ffi_args.as_ptr(), &mut out) };
        // ffi_args and cstrings are still alive here ↑

        // Read the result before freeing anything (ffi_to_vm deep-copies out of
        // the transport tree), then release every marshalled tree.
        let result = if status != 0 {
            let msg = if out.tag == JADE_TAG_STR || out.tag == JADE_TAG_ERROR {
                unsafe {
                    CStr::from_ptr(out.data.as_str as *const c_char).to_string_lossy().into_owned()
                }
            } else {
                format!("native fn '{}' returned error code {}", self.name, status)
            };
            Err(JadeError::IoError { message: msg, span })
        } else {
            ffi_to_vm(&out, span)
        };

        unsafe {
            for a in &ffi_args {
                ffi_free(a);
            }
            ffi_free(&out);
        }

        result
    }

    /// The raw function pointer, for the callback path's worker thread.
    pub(crate) fn fn_ptr(&self) -> JadeNativeFnPtr {
        self.fn_ptr
    }
}

/// Everything a native call needs, packaged so it can cross to a worker thread.
///
/// The pointers inside are libc-heap trees this call owns for its duration, and
/// only one thread touches them at a time — the worker, while the interpreter
/// waits on it. That is what makes the `Send` sound; it is not a claim that
/// `JadeVal` is thread-safe in general.
pub(crate) struct NativeCallArgs {
    pub argv: Vec<JadeVal>,
    /// Kept alive because top-level string arguments borrow from it.
    pub _cstrings: Vec<CString>,
}

// SAFETY: see the note on the struct — ownership is exclusive for the duration
// of the call, and the interpreter does not touch these while the worker runs.
unsafe impl Send for NativeCallArgs {}

/// What the worker hands back: the arguments it borrowed, the value the library
/// wrote, and the status.
///
/// The arguments come back rather than being freed on the worker so the
/// interpreter reads `out` before anything is released — a native function may
/// return a pointer *into* one of its arguments, which is the bug that bit the
/// compiled path.
pub(crate) struct NativeCallResult {
    pub args: NativeCallArgs,
    pub out: JadeVal,
    pub status: i32,
}

// SAFETY: same reasoning as `NativeCallArgs` — handed over whole, touched by
// one thread at a time.
unsafe impl Send for NativeCallResult {}

/// Marshal arguments for a call that passes at least one Jade function.
///
/// Each callable becomes a [`JadeFn`] whose `host` is a [`CallbackHost`] owning
/// a clone of the sender. Everything else marshals as usual.
pub(crate) fn marshal_with_callbacks(
    args: &[VmValue],
    bus: &std::sync::Arc<CallbackBus>,
    span: Span,
) -> NativeCallArgs {
    let mut cstrings = Vec::new();
    let mut argv = Vec::with_capacity(args.len());

    for v in args {
        if is_callable(v) {
            let mut host =
                Box::new(CallbackHost { callee: v.clone(), bus: std::sync::Arc::clone(bus), span });
            let ptr: *mut c_void = &mut *host as *mut CallbackHost as *mut c_void;

            let f = unsafe { malloc(std::mem::size_of::<JadeFn>()) } as *mut JadeFn;
            if f.is_null() {
                std::alloc::handle_alloc_error(std::alloc::Layout::new::<JadeFn>());
            }
            unsafe { std::ptr::write(f, JadeFn { host: ptr, invoke: Some(vm_invoke_callback) }) };

            // Handed to the bus rather than to this call. A library may store
            // the pointer and invoke it from a later call entirely, so the call
            // that passed it is the wrong owner — that is what used to free the
            // function out from under a stored callback.
            {
                let mut live = bus.live.lock();
                live.hosts.push(host);
                live.wrappers.push(f);
            }
            argv.push(JadeVal { tag: JADE_TAG_FN, _pad: [0; 7], data: JadeValData { as_fn: f } });
        } else {
            argv.push(vm_to_ffi(v, &mut cstrings));
        }
    }

    NativeCallArgs { argv, _cstrings: cstrings }
}

/// Turn a completed native call into a Jade result. Shared by both paths.
pub(crate) fn finish_native_call(
    name: &str,
    argv: &[JadeVal],
    out: &JadeVal,
    status: i32,
    span: Span,
) -> Result<VmValue> {
    let result = if status != 0 {
        let msg = if out.tag == JADE_TAG_STR || out.tag == JADE_TAG_ERROR {
            unsafe {
                CStr::from_ptr(out.data.as_str as *const c_char).to_string_lossy().into_owned()
            }
        } else {
            format!("native fn '{name}' returned error code {status}")
        };
        Err(JadeError::IoError { message: msg, span })
    } else {
        ffi_to_vm(out, span)
    };

    unsafe {
        for a in argv {
            ffi_free(a);
        }
        ffi_free(out);
    }
    result
}

// ── Loader ────────────────────────────────────────────────────────────────────

/// Load a native package from a shared library and return its exported functions
/// as a `HashMap<name, VmValue::NativeLibFn>`.
/// Refuse a package built against a value ABI this runtime cannot talk to.
///
/// Two symbols are consulted, in order:
///
///  * `jade_pkg_abi_version` — emitted into every package `jade build --lib`
///    produces (see [`crate::aot`]). Authoritative.
///  * `jrt_abi_version` — the runtime's own accessor. A package that links
///    `jade-runtime` may re-export it, which is how packages published before
///    the first symbol existed can still be checked.
///
/// Neither present means the package does not link the Jade runtime at all — a
/// plain C library wrapped by `jade pkg add --c-abi`, which has no value ABI to
/// disagree about. Those load, as they always have.
///
/// This exists because nothing checked [`jade_runtime::RUNTIME_ABI_VERSION`]
/// despite its whole purpose being detection: when v1.1.31 began sending the
/// inference request as a struct, every published provider — built against
/// ABI 1 — failed with `native function returned an unknown value tag` from
/// inside the call, naming neither the version nor the fix.
fn check_package_abi(lib: &libloading::Library, lib_path: &Path, span: Span) -> Result<()> {
    let read = |sym: &[u8]| -> Option<u32> {
        let f: libloading::Symbol<unsafe extern "C" fn() -> u32> = unsafe { lib.get(sym) }.ok()?;
        Some(unsafe { f() })
    };

    let Some(theirs) = read(b"jade_pkg_abi_version\0").or_else(|| read(b"jrt_abi_version\0"))
    else {
        return Ok(());
    };

    let ours = jade_runtime::RUNTIME_ABI_VERSION;
    if theirs == ours {
        return Ok(());
    }

    let advice = if theirs < ours {
        "It was built against an older Jade. Rebuild it with this toolchain, or \
         reinstall the providers that ship with your Jade release."
    } else {
        "It was built against a newer Jade than this one. Upgrade with `jade upgrade`."
    };
    Err(JadeError::IoError {
        message: format!(
            "native package '{}' speaks value ABI {theirs}, but this Jade speaks {ours}. {advice}",
            lib_path.display()
        ),
        span,
    })
}

pub fn load_native_package(lib_path: &Path, span: Span) -> Result<HashMap<String, VmValue>> {
    // Canonicalized first, and that is load-bearing rather than tidy. `dlopen`
    // keys a loaded image by the path it was asked for, so two spellings of one
    // file — a symlinked `libs/`, a project reached through one — produce two
    // independent instances with two sets of globals. A compiled artifact
    // resolves through `realpath` for the same reason, and the two engines have
    // to arrive at the same string or a package loaded by both is loaded twice.
    let canon = std::fs::canonicalize(lib_path).unwrap_or_else(|_| lib_path.to_path_buf());
    let lib = unsafe { libloading::Library::new(&canon) }.map_err(|e| JadeError::IoError {
        message: format!("could not load native library '{}': {}", lib_path.display(), e),
        span,
    })?;

    let lib = Arc::new(lib);

    check_package_abi(&lib, lib_path, span)?;

    let init_fn: libloading::Symbol<unsafe extern "C" fn(*mut JadeNativePkg) -> i32> =
        unsafe { lib.get(b"jade_pkg_init\0") }.map_err(|e| JadeError::IoError {
            // Naming the symbol alone is accurate and useless: the reader has
            // no reason to know what defines it. Every library that reaches
            // here without it is a plain C library that was never bound, so say
            // that and give the command.
            message: format!(
                "native library '{}' has no `jade_pkg_init`, so it is a plain C library rather \
                 than a Jade package.\n  Jade cannot load one directly — it needs a binding \
                 generated from the library's header:\n    \
                 jade pkg add <name> --path <the .dylib> --header <its header.h>\n  ({})",
                lib_path.display(),
                e
            ),
            span,
        })?;

    let mut pkg =
        JadeNativePkg { name: std::ptr::null(), bindings: std::ptr::null(), binding_count: 0 };
    let status = unsafe { init_fn(&mut pkg) };
    if status != 0 {
        return Err(JadeError::IoError {
            message: format!(
                "jade_pkg_init in '{}' returned error code {}",
                lib_path.display(),
                status
            ),
            span,
        });
    }

    if pkg.bindings.is_null() || pkg.binding_count == 0 {
        return Ok(HashMap::new());
    }

    let bindings = unsafe { std::slice::from_raw_parts(pkg.bindings, pkg.binding_count) };
    let mut map = HashMap::with_capacity(bindings.len());

    for binding in bindings {
        if binding.name.is_null() {
            continue;
        }
        let name = unsafe { CStr::from_ptr(binding.name) }.to_string_lossy().into_owned();
        let nfn = Arc::new(NativeLibFn {
            name: name.clone(),
            fn_ptr: binding.func,
            _lib: Arc::clone(&lib),
        });
        map.insert(name, VmValue::NativeLibFn(nfn));
    }

    Ok(map)
}

// ── Conversions ───────────────────────────────────────────────────────────────

// ── libc-heap helpers for the array/dict transport trees ──────────────────────

/// `malloc(n * size_of::<T>())`, aborting on failure. Returns null for `n == 0`.
unsafe fn ffi_alloc<T>(n: usize) -> *mut T {
    if n == 0 {
        return std::ptr::null_mut();
    }
    let p = unsafe { malloc(n * std::mem::size_of::<T>()) } as *mut T;
    if p.is_null() {
        std::alloc::handle_alloc_error(std::alloc::Layout::array::<T>(n).expect("ffi layout"));
    }
    p
}

/// Copy `s` into a libc-owned NUL-terminated buffer (a container-owned string).
unsafe fn ffi_strdup(s: &str) -> *const u8 {
    let bytes = s.as_bytes();
    let p = unsafe { malloc(bytes.len() + 1) } as *mut u8;
    if p.is_null() {
        std::alloc::handle_alloc_error(
            std::alloc::Layout::from_size_align(bytes.len() + 1, 1).expect("ffi layout"),
        );
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        *p.add(bytes.len()) = 0;
    }
    p
}

/// The name a struct crosses the ABI under: its source name, with any
/// import-mangling suffix removed.
///
/// `aot/imports.rs` renames a module-global `Foo` to `Foo$2` when flattening
/// imports, so two imported modules can each declare one. That number is a
/// property of the importing program's module graph, not of the type, so it means
/// nothing on the other side of an FFI call. Without stripping it, a provider
/// package built with `use ovata::infer` hands back a frame named `Token$0` and
/// the caller does not recognise its own protocol.
///
/// `$` cannot appear in a Jade identifier, so a trailing `$<digits>` is always the
/// mangling and never part of a name someone wrote. `runtime_aot/native.c` strips
/// the same thing on its side of the boundary.
pub fn abi_type_name(name: &str) -> &str {
    match name.rsplit_once('$') {
        // A non-empty base is required: stripping would otherwise turn a name that
        // is *only* a suffix into an empty type name, which is worse than leaving
        // the odd input alone.
        Some((base, id))
            if !base.is_empty() && !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) =>
        {
            base
        }
        _ => name,
    }
}

/// Marshal a `VmValue` into a `JadeVal` whose every owned part is libc heap:
/// strings are copied (via [`ffi_strdup`]) and arrays/dicts build nested trees,
/// so the whole thing is releasable with [`ffi_free`]. Used for container
/// elements at any depth, and for a top-level array/dict argument. A top-level
/// *string* argument instead borrows through [`vm_to_ffi`]'s `CString` scratch.
fn vm_to_ffi_owned(val: &VmValue) -> JadeVal {
    match val {
        VmValue::Nil => JadeVal::nil(),
        VmValue::Int(i) => {
            JadeVal { tag: JADE_TAG_INT, _pad: [0; 7], data: JadeValData { as_int: *i } }
        }
        VmValue::Float(f) => {
            JadeVal { tag: JADE_TAG_FLOAT, _pad: [0; 7], data: JadeValData { as_float: *f } }
        }
        VmValue::Bool(b) => JadeVal {
            tag: JADE_TAG_BOOL,
            _pad: [0; 7],
            data: JadeValData { as_bool: if *b { 1 } else { 0 } },
        },
        // Trust rides in `_pad[0]`. A char taken from a tainted string is still
        // tainted, and it has no header of its own to carry that in the way a
        // string does — so the padding the struct already had becomes the
        // provenance bit, rather than the tag growing a second field.
        VmValue::Char(c) => JadeVal {
            tag: JADE_TAG_CHAR,
            _pad: [c.trust(), 0, 0, 0, 0, 0, 0],
            data: JadeValData { as_char: c.ch() as u32 },
        },
        VmValue::Str(s) => JadeVal {
            tag: JADE_TAG_STR,
            _pad: [0; 7],
            data: JadeValData { as_str: unsafe { ffi_strdup(s.as_str()) } },
        },
        VmValue::Array(arc) => {
            let guard = arc.lock();
            let n = guard.len();
            let items = unsafe { ffi_alloc::<JadeVal>(n) };
            for (i, el) in guard.iter().enumerate() {
                unsafe { items.add(i).write(vm_to_ffi_owned(el)) };
            }
            let a = unsafe { ffi_alloc::<JadeArr>(1) };
            unsafe { a.write(JadeArr { items, len: n }) };
            JadeVal { tag: JADE_TAG_ARRAY, _pad: [0; 7], data: JadeValData { as_arr: a } }
        }
        VmValue::Dict(map) => {
            let n = map.len();
            let keys = unsafe { ffi_alloc::<*const u8>(n) };
            let vals = unsafe { ffi_alloc::<JadeVal>(n) };
            for (i, (k, v)) in map.iter().enumerate() {
                unsafe {
                    keys.add(i).write(ffi_strdup(k));
                    vals.add(i).write(vm_to_ffi_owned(v));
                }
            }
            let m = unsafe { ffi_alloc::<JadeMap>(1) };
            unsafe { m.write(JadeMap { keys, vals, len: n }) };
            JadeVal { tag: JADE_TAG_DICT, _pad: [0; 7], data: JadeValData { as_dict: m } }
        }
        VmValue::Struct(arc) => {
            let guard = arc.lock();
            let n = guard.len();
            let keys = unsafe { ffi_alloc::<*const u8>(n) };
            let vals = unsafe { ffi_alloc::<JadeVal>(n) };
            for (i, (k, v)) in guard.fields().iter().enumerate() {
                unsafe {
                    keys.add(i).write(ffi_strdup(k));
                    vals.add(i).write(vm_to_ffi_owned(v));
                }
            }
            let st = unsafe { ffi_alloc::<JadeStruct>(1) };
            unsafe {
                st.write(JadeStruct {
                    type_name: ffi_strdup(abi_type_name(guard.type_name())),
                    keys,
                    vals,
                    len: n,
                })
            };
            JadeVal { tag: JADE_TAG_STRUCT, _pad: [0; 7], data: JadeValData { as_struct: st } }
        }
        VmValue::Bytes(b) => {
            // Counted, and copied into libc heap: the far side may free it, and
            // this process holds two mimalloc instances that must not free each
            // other's allocations. Same rule as JadeArr/JadeMap above.
            let src = b.as_slice();
            let n = src.len();
            let data = unsafe { malloc(n.max(1)) } as *mut u8;
            if data.is_null() {
                std::alloc::handle_alloc_error(
                    std::alloc::Layout::from_size_align(n.max(1), 1).expect("ffi layout"),
                );
            }
            if n > 0 {
                unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), data, n) };
            }
            let bx = unsafe { malloc(std::mem::size_of::<JadeBytes>()) } as *mut JadeBytes;
            if bx.is_null() {
                std::alloc::handle_alloc_error(std::alloc::Layout::new::<JadeBytes>());
            }
            unsafe { std::ptr::write(bx, JadeBytes { data, len: n }) };
            JadeVal { tag: JADE_TAG_BYTES, _pad: [0; 7], data: JadeValData { as_bytes: bx } }
        }
        VmValue::Handle(h) => {
            // The wrapper is freshly allocated so `ffi_free` has something of
            // its own to release, and the name is copied for the same reason
            // every container-owned string is. The pointer itself is passed
            // straight back to the package that issued it — no copy, and
            // nothing to free.
            let hx = unsafe { malloc(std::mem::size_of::<JadeHandle>()) } as *mut JadeHandle;
            if hx.is_null() {
                std::alloc::handle_alloc_error(std::alloc::Layout::new::<JadeHandle>());
            }
            unsafe {
                std::ptr::write(
                    hx,
                    JadeHandle { ptr: h.ptr as *mut c_void, type_name: ffi_strdup(&h.type_name()) },
                )
            };
            JadeVal { tag: JADE_TAG_HANDLE, _pad: [0; 7], data: JadeValData { as_handle: hx } }
        }
        // A callable reaching here has no channel to post to, which happens
        // when one is nested inside a container rather than passed directly.
        // Nil is right: the worker-thread inversion is set up per *call*, and a
        // function buried in an array was never wired to it.
        //
        // Futures and prompts have no ABI representation at all.
        _ => JadeVal::nil(),
    }
}

/// Convert a `VmValue` to a `JadeVal` for a top-level argument. A string is
/// handed over as a borrowed pointer into a `CString` pushed onto `scratch`,
/// which the caller must keep alive for the native call; arrays and dicts become
/// libc-heap trees the caller releases with [`ffi_free`] afterward.
pub fn vm_to_ffi(val: &VmValue, scratch: &mut Vec<CString>) -> JadeVal {
    if let VmValue::Str(s) = val {
        let cs = CString::new(s.as_bytes()).unwrap_or_default();
        let ptr = cs.as_ptr() as *const u8;
        scratch.push(cs);
        return JadeVal { tag: JADE_TAG_STR, _pad: [0; 7], data: JadeValData { as_str: ptr } };
    }
    vm_to_ffi_owned(val)
}

/// Convert a `JadeVal` returned by a native function back to a `VmValue`,
/// deep-copying array/dict trees into owned collections.
pub fn ffi_to_vm(val: &JadeVal, span: Span) -> Result<VmValue> {
    match val.tag {
        JADE_TAG_NIL => Ok(VmValue::Nil),
        // Refused rather than carried. The interpreter's `VmValue::Int` is a
        // plain `i64`, so it *could* hold this — and did, which is why a hash
        // printed correctly here and came back off by 2^63 from the compiled
        // binary for the same call. But it was inert either way: Jade's own
        // arithmetic is 63-bit, so adding zero to it raised, and the value could
        // not even be written back into the source to test against, because the
        // lexer caps a literal at the same bound. A number you can print and
        // nothing else is worse than a refusal, and worse still when the two
        // engines disagree about which number it is. See TOOLCHAIN-BUGS #3.
        JADE_TAG_INT => {
            let i = unsafe { val.data.as_int };
            if !jade_runtime::JadeValue::int_fits(i) {
                return Err(JadeError::IoError {
                    message: format!(
                        "native call returned {i}, which is outside the range a Jade integer \
                         can hold ({} to {})",
                        jade_runtime::JadeValue::INT_MIN,
                        jade_runtime::JadeValue::INT_MAX
                    ),
                    span,
                });
            }
            Ok(VmValue::Int(i))
        }
        JADE_TAG_FLOAT => Ok(VmValue::Float(unsafe { val.data.as_float })),
        JADE_TAG_BOOL => Ok(VmValue::Bool(unsafe { val.data.as_bool } != 0)),
        JADE_TAG_CHAR => {
            let raw = unsafe { val.data.as_char };
            // A scalar a native package invented may not be one: the surrogate
            // range and anything past U+10FFFF are not characters, and Rust's
            // `char` cannot hold them. Refused by name rather than silently
            // replaced, which would corrupt the data it claims to carry.
            let ch = char::from_u32(raw).ok_or_else(|| JadeError::IoError {
                message: format!(
                    "native function returned {raw:#x} as a char, which is not a Unicode scalar"
                ),
                span,
            })?;
            // Tainted whatever the package said, exactly as a returned string
            // and a returned blob are. Data coming back from a native package is
            // from outside the program, and `TRUSTED` is zero — so honouring the
            // incoming bit would mark a char trusted for no better reason than
            // that the package zeroed its struct.
            Ok(VmValue::Char(jade_runtime::trust::JChar::with_trust(
                ch,
                jade_runtime::trust::TAINTED,
            )))
        }
        JADE_TAG_STR => {
            let s = unsafe {
                CStr::from_ptr(val.data.as_str as *const c_char).to_string_lossy().into_owned()
            };
            Ok(VmValue::Str(s.into()))
        }
        JADE_TAG_ARRAY => {
            let a = unsafe { val.data.as_arr };
            let mut items = Vec::new();
            if !a.is_null() {
                let len = unsafe { (*a).len };
                let base = unsafe { (*a).items };
                for i in 0..len {
                    items.push(ffi_to_vm(unsafe { &*base.add(i) }, span)?);
                }
            }
            Ok(make_array(items))
        }
        JADE_TAG_DICT => {
            let m = unsafe { val.data.as_dict };
            let mut d = DictObj::new();
            if !m.is_null() {
                let len = unsafe { (*m).len };
                let (keys, vals) = unsafe { ((*m).keys, (*m).vals) };
                for i in 0..len {
                    let key = unsafe {
                        CStr::from_ptr(*keys.add(i) as *const c_char).to_string_lossy().into_owned()
                    };
                    let value = ffi_to_vm(unsafe { &*vals.add(i) }, span)?;
                    d.insert(key, value);
                }
            }
            Ok(VmValue::dict(d))
        }
        JADE_TAG_BYTES => {
            let bp = unsafe { val.data.as_bytes };
            if bp.is_null() {
                return Ok(VmValue::Nil);
            }
            let (data, len) = unsafe { ((*bp).data, (*bp).len) };
            let slice = if data.is_null() || len == 0 {
                Vec::new()
            } else {
                unsafe { core::slice::from_raw_parts(data, len) }.to_vec()
            };
            // Data from a native package is from outside the program, exactly
            // as a file read is.
            Ok(VmValue::Bytes(std::sync::Arc::new(jade_runtime::bytesf::BytesObj::new(
                slice,
                jade_runtime::trust::TAINTED,
            ))))
        }
        // A JadeFn only ever travels *outward*. A package handing one back
        // would be offering the program a C function to call, which is the
        // opposite direction and has no representation on the Jade side.
        JADE_TAG_FN => Err(JadeError::IoError {
            message: "native function returned a callback, which Jade cannot hold".to_string(),
            span,
        }),
        JADE_TAG_HANDLE => {
            let hp = unsafe { val.data.as_handle };
            if hp.is_null() {
                return Ok(VmValue::Nil);
            }
            let (ptr, name) = unsafe { ((*hp).ptr, (*hp).type_name) };
            // An unnamed handle is allowed but matches nothing, so a binding
            // that forgot its type name fails a type check rather than passing
            // silently for anything.
            let type_name = if name.is_null() {
                std::ffi::CString::default()
            } else {
                let s = unsafe { CStr::from_ptr(name as *const c_char) };
                std::ffi::CString::from(s)
            };
            // No trust byte: a handle carries no data to taint. What the pointee
            // yields gets its trust when it crosses as bytes or a string.
            Ok(VmValue::Handle(Arc::new(jade_runtime::handle::HandleObj::new(
                ptr as usize,
                type_name,
            ))))
        }
        JADE_TAG_STRUCT => {
            let st = unsafe { val.data.as_struct };
            if st.is_null() {
                return Ok(VmValue::Nil);
            }
            let (type_name, keys, vals, len) =
                unsafe { ((*st).type_name, (*st).keys, (*st).vals, (*st).len) };
            let name = unsafe {
                CStr::from_ptr(type_name as *const c_char).to_string_lossy().into_owned()
            };
            let mut obj = StructObj::new(&name);
            for i in 0..len {
                let key = unsafe {
                    CStr::from_ptr(*keys.add(i) as *const c_char).to_string_lossy().into_owned()
                };
                let value = ffi_to_vm(unsafe { &*vals.add(i) }, span)?;
                obj.set_field(&key, value);
            }
            Ok(VmValue::Struct(Arc::new(Mutex::new(obj))))
        }
        JADE_TAG_ERROR => {
            let msg = unsafe {
                CStr::from_ptr(val.data.as_str as *const c_char).to_string_lossy().into_owned()
            };
            Err(JadeError::IoError { message: msg, span })
        }
        other => Err(JadeError::IoError {
            message: format!("native function returned unknown tag {other:#04x}"),
            span,
        }),
    }
}

/// Recursively free a container-owned node: its owned string, or its nested
/// arrays/dicts. Reached only through a container root, where string elements
/// are libc-owned (`ffi_strdup`).
unsafe fn ffi_free_node(v: &JadeVal) {
    match v.tag {
        JADE_TAG_STR | JADE_TAG_ERROR => unsafe { free(v.data.as_str as *mut c_void) },
        JADE_TAG_BYTES => {
            let bp = unsafe { v.data.as_bytes };
            if !bp.is_null() {
                unsafe {
                    free((*bp).data as *mut c_void);
                    free(bp as *mut c_void);
                }
            }
        }
        JADE_TAG_HANDLE => {
            let hp = unsafe { v.data.as_handle };
            if !hp.is_null() {
                unsafe {
                    // The name and the wrapper, and deliberately not `ptr`. See
                    // JADE_TAG_HANDLE — freeing the pointee here would hand the
                    // package's memory back to the wrong allocator.
                    free((*hp).type_name as *mut c_void);
                    free(hp as *mut c_void);
                }
            }
        }
        JADE_TAG_ARRAY => {
            let a = unsafe { v.data.as_arr };
            if !a.is_null() {
                let (base, len) = unsafe { ((*a).items, (*a).len) };
                for i in 0..len {
                    unsafe { ffi_free_node(&*base.add(i)) };
                }
                unsafe {
                    free(base as *mut c_void);
                    free(a as *mut c_void);
                }
            }
        }
        JADE_TAG_DICT => {
            let m = unsafe { v.data.as_dict };
            if !m.is_null() {
                let (keys, vals, len) = unsafe { ((*m).keys, (*m).vals, (*m).len) };
                for i in 0..len {
                    unsafe {
                        free(*keys.add(i) as *mut c_void);
                        ffi_free_node(&*vals.add(i));
                    }
                }
                unsafe {
                    free(keys as *mut c_void);
                    free(vals as *mut c_void);
                    free(m as *mut c_void);
                }
            }
        }
        JADE_TAG_STRUCT => {
            let st = unsafe { v.data.as_struct };
            if !st.is_null() {
                let (name, keys, vals, len) =
                    unsafe { ((*st).type_name, (*st).keys, (*st).vals, (*st).len) };
                for i in 0..len {
                    unsafe {
                        free(*keys.add(i) as *mut c_void);
                        ffi_free_node(&*vals.add(i));
                    }
                }
                unsafe {
                    free(name as *mut c_void);
                    free(keys as *mut c_void);
                    free(vals as *mut c_void);
                    free(st as *mut c_void);
                }
            }
        }
        _ => {} // scalar: nothing owned
    }
}

/// Release a marshalled `JadeVal` tree. Only container roots own libc heap;
/// scalars (including top-level non-owning strings) are left untouched, so this
/// is safe to call on any `JadeVal`.
///
/// # Safety
///
/// `v` must be a value a native call produced and no one has released yet: the
/// owning kinds are freed here, so calling this twice on one value is a double
/// free. A value the caller built itself is fine as long as its payload came
/// from the same allocator the shim uses.
pub unsafe fn ffi_free(v: &JadeVal) {
    if matches!(
        v.tag,
        JADE_TAG_ARRAY | JADE_TAG_DICT | JADE_TAG_STRUCT | JADE_TAG_BYTES | JADE_TAG_HANDLE
    ) {
        unsafe { ffi_free_node(v) };
    }
}

#[cfg(test)]
mod tests;
