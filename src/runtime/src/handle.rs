//! `HandleObj` — an opaque foreign pointer, shared by both engines.
//!
//! A handle is what a C library hands you when it will not tell you what is
//! inside: `sqlite3*`, `SNDFILE*`, `gzFile`, `FT_Face`. Jade holds it, passes it
//! back, and never looks at it. That is the whole type — a value with no
//! operations.
//!
//! Handles exist because an entire class of library is otherwise unreachable.
//! SQLite, libsndfile, PCRE2, FreeType, libcurl and libarchive are all organised
//! around a pointer the caller keeps between calls, and before this tag existed
//! there was nowhere in the Jade value ABI to put one — it marshalled to `nil`,
//! so none of them could be bound even in principle.
//!
//! ## The pointer is stored as a `usize`, deliberately
//!
//! Not `*mut c_void`. Two things follow, and both are the point:
//!
//!  * **Jade cannot dereference it by accident.** Reading the pointee requires
//!    an explicit cast that no code in this crate performs. The type enforces
//!    the contract rather than a comment asking for it.
//!  * **The value is `Send`/`Sync` without an `unsafe impl`.** A raw pointer is
//!    neither, and `VmValue` must be both to cross a task boundary. Note this
//!    makes the *value* thread-safe to move, which is not the same as the
//!    *pointee* being thread-safe to use — see the note on tasks below.
//!
//! ## Type names
//!
//! A handle carries the C type it came from, so `handle<sqlite3>` and
//! `handle<sqlite3_stmt>` are distinct and passing one where the other is
//! expected raises instead of corrupting memory inside the library. This is the
//! same reasoning that makes a struct carry its type name across the FFI: a
//! value that is structurally interchangeable but semantically different needs
//! something for the receiver to check.
//!
//! The name is a [`CString`] rather than a `String` because both sides want it.
//! Rust reads it for rendering and type checks; `runtime_aot/native.c` needs a
//! NUL-terminated pointer to copy into the wire struct, and holding it in C form
//! means handing that over costs nothing.
//!
//! ## Ownership: Jade frees the wrapper, never the pointee
//!
//! This is the one rule that makes handles different from every other heap kind.
//! Dropping a `HandleObj` reclaims the header and the type name. It does **not**
//! touch whatever `ptr` addresses, because Jade has no idea what that is or
//! which allocator produced it — freeing a `sqlite3*` with anything but
//! `sqlite3_close` corrupts the library's state.
//!
//! The consequence is honest and worth stating: a handle that goes out of scope
//! without its close function being called leaks whatever the C library
//! allocated. Closing is an explicit call the binding exposes.
//!
//! ## Tasks
//!
//! A handle is shared mutable state that Jade cannot see mutating — there are no
//! field writes or mutating methods to watch, only native calls whose effects
//! are entirely inside the library. `compiler/taskcheck.rs` therefore refuses to
//! pass a handle that came from a parameter or a global into a native call
//! inside a spawned function; a task opens its own.

use core::ffi::{c_char, c_void};
use std::ffi::CString;

use crate::heap::{ObjHeader, ObjKind};

/// An opaque pointer from a native package, plus the C type it came from.
///
/// `repr(C)` and header-first like every other kind — `gc::free_obj` and the
/// refcount ops read the kind byte at offset 8 before they know what they are
/// looking at.
#[repr(C)]
pub struct HandleObj {
    /// Kind = [`ObjKind::Handle`].
    pub header: ObjHeader,
    /// The foreign pointer, as an integer. Never dereferenced here; see the
    /// module note on why this is not a `*mut c_void`.
    pub ptr: usize,
    /// The C type name, e.g. `sqlite3`. NUL-terminated so `native.c` can copy it
    /// straight onto the wire.
    pub type_name: CString,
}

impl HandleObj {
    pub fn new(ptr: usize, type_name: CString) -> Self {
        HandleObj { header: ObjHeader::new(ObjKind::Handle, 0), ptr, type_name }
    }

    /// The C type name as text. Lossy on invalid UTF-8, which cannot arise from
    /// a generated binding but can from a hand-written package.
    pub fn type_name(&self) -> std::borrow::Cow<'_, str> {
        self.type_name.to_string_lossy()
    }

    /// Whether this handle came from `name`.
    pub fn is_type(&self, name: &str) -> bool {
        self.type_name.as_bytes() == name.as_bytes()
    }

    /// A null handle is what a failed `open` returns before its error convention
    /// is applied. Bindings should turn one into a raise rather than hand it on.
    pub fn is_null(&self) -> bool {
        self.ptr == 0
    }
}

/// Identity, not equality of the pointee. Two handles are the same handle when
/// they address the same object *and* claim the same type — the type is part of
/// what the value means, so a `sqlite3` and a `sqlite3_stmt` that happen to
/// share an address (the first member of a struct, say) are not interchangeable.
impl PartialEq for HandleObj {
    fn eq(&self, other: &Self) -> bool {
        self.ptr == other.ptr && self.type_name == other.type_name
    }
}
impl Eq for HandleObj {}

impl core::fmt::Debug for HandleObj {
    /// The type and whether it is null, never the address. A pointer value in a
    /// panic message is noise that also varies run to run, which would make any
    /// test asserting on it flaky.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "HandleObj(handle<{}>{})",
            self.type_name(),
            if self.is_null() { ", null" } else { "" }
        )
    }
}

/// How a handle prints: `handle<sqlite3>`.
///
/// Deliberately *not* the address. Both engines must produce the same text — the
/// parity gate diffs stdout — and an address differs on every run, so printing
/// one would make every program holding a handle fail the gate.
pub fn render(h: &HandleObj) -> String {
    format!("handle<{}>", h.type_name())
}

// ── C ABI ─────────────────────────────────────────────────────────────────────

/// Allocate a handle value. Returns the raw pointer; codegen tags it `TAG_PTR`.
///
/// `type_name` is copied. A null `type_name` becomes the empty name, which
/// renders as `handle<>` and matches nothing — visible, and better than a name
/// read off a dangling pointer.
///
/// # Safety
/// `type_name` must be NUL-terminated or null. `ptr` is stored verbatim and
/// never dereferenced, so it may be any value including null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_handle_new(ptr: *mut c_void, type_name: *const c_char) -> *mut c_void {
    let name = if type_name.is_null() {
        CString::default()
    } else {
        // `to_string` handles the copy and any invalid UTF-8; rebuilding the
        // CString from it cannot fail because a Rust String holds no interior
        // NUL.
        let s = unsafe { crate::cstr::to_string(type_name) };
        CString::new(s).unwrap_or_default()
    };
    crate::gc::leak_obj(HandleObj::new(ptr as usize, name))
}

/// The foreign pointer.
///
/// # Safety
/// `p` must point at a live [`HandleObj`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_handle_ptr(p: *const c_void) -> *mut c_void {
    unsafe { (*(p as *const HandleObj)).ptr as *mut c_void }
}

/// The C type name, borrowed. Valid for as long as the handle is.
///
/// # Safety
/// `p` must point at a live [`HandleObj`]; the result must not outlive it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_handle_type(p: *const c_void) -> *const c_char {
    unsafe { (*(p as *const HandleObj)).type_name.as_ptr() }
}

/// Whether the handle at `p` claims type `name`. Returns 1 or 0.
///
/// This is what a binding calls before untagging a receiver it did not
/// statically type, the way `jrt_require_kind` guards a primitive method.
///
/// # Safety
/// `p` must point at a live [`HandleObj`]; `name` must be NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_handle_is_type(p: *const c_void, name: *const c_char) -> i32 {
    if name.is_null() {
        return 0;
    }
    let h = unsafe { &*(p as *const HandleObj) };
    let want = unsafe { core::ffi::CStr::from_ptr(name) };
    i32::from(h.type_name.as_c_str() == want)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(ptr: usize, name: &str) -> HandleObj {
        HandleObj::new(ptr, CString::new(name).unwrap())
    }

    #[test]
    fn a_handle_carries_its_kind_byte() {
        assert_eq!(h(0x1000, "sqlite3").header.kind, ObjKind::Handle as u8);
    }

    #[test]
    fn identity_covers_both_the_address_and_the_type() {
        assert_eq!(h(0x1000, "sqlite3"), h(0x1000, "sqlite3"));
        // Same address, different type: not the same handle. This is the case
        // that makes the type name load-bearing rather than decorative.
        assert_ne!(h(0x1000, "sqlite3"), h(0x1000, "sqlite3_stmt"));
        assert_ne!(h(0x1000, "sqlite3"), h(0x2000, "sqlite3"));
    }

    #[test]
    fn rendering_names_the_type_and_never_the_address() {
        let rendered = render(&h(0xDEADBEEF, "SNDFILE"));
        assert_eq!(rendered, "handle<SNDFILE>");
        // Two handles onto different objects of the same type must print
        // identically, or the parity gate would diff on the address.
        assert_eq!(render(&h(0x1, "SNDFILE")), render(&h(0x2, "SNDFILE")));
    }

    #[test]
    fn null_is_recognised() {
        assert!(h(0, "gzFile").is_null());
        assert!(!h(0x8, "gzFile").is_null());
    }

    #[test]
    fn type_checks_compare_the_whole_name() {
        let db = h(0x10, "sqlite3");
        assert!(db.is_type("sqlite3"));
        // A prefix is not a match — `sqlite3` must not accept `sqlite3_stmt`.
        assert!(!db.is_type("sqlite3_stmt"));
        assert!(!db.is_type("sqlite"));
    }

    #[test]
    fn the_c_constructor_copies_the_name_and_keeps_the_pointer() {
        let name = CString::new("FT_Face").unwrap();
        let p = unsafe { jrt_handle_new(0x1234 as *mut c_void, name.as_ptr()) };
        assert_eq!(unsafe { jrt_handle_ptr(p) } as usize, 0x1234);
        assert_eq!(unsafe { crate::cstr::to_string(jrt_handle_type(p)) }, "FT_Face");
        assert_eq!(unsafe { jrt_handle_is_type(p, name.as_ptr()) }, 1);

        // The name is copied, not borrowed: dropping the source leaves it valid.
        drop(name);
        assert_eq!(unsafe { crate::cstr::to_string(jrt_handle_type(p)) }, "FT_Face");

        unsafe { crate::gc::free_obj(p) };
    }

    #[test]
    fn a_null_type_name_is_empty_rather_than_a_read_of_null() {
        let p = unsafe { jrt_handle_new(core::ptr::null_mut(), core::ptr::null()) };
        assert_eq!(unsafe { crate::cstr::to_string(jrt_handle_type(p)) }, "");
        unsafe { crate::gc::free_obj(p) };
    }
}
