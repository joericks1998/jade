//! `BytesObj` — a binary blob on the heap, shared by both engines.
//!
//! A `bytes` value is a counted sequence of raw octets. It is deliberately *not*
//! a string: Jade strings are UTF-8 and NUL-terminated, and neither holds for
//! arbitrary bytes. A PNG contains NUL bytes and byte sequences that are not
//! valid UTF-8, so storing one in a `str` would truncate it at the first NUL and
//! corrupt it on any operation that assumes valid UTF-8. Conversion between the
//! two is explicit in both directions (`encode` / `decode`), never implicit.
//!
//! ## Trust
//!
//! `BytesObj` carries a trust byte, exactly as [`crate::trust::JStr`] does, and
//! for the same reason: `fs.read_bytes` returns data from outside the program.
//! Without it, `fs.read_bytes(p).decode()` would hand back a *clean* string and
//! walk straight past the check in `sh.exec` that `fs.read(p)` cannot. The trust
//! byte lives in the object rather than at `data[-1]` the way a string's does,
//! because a bytes payload has no reserved prefix byte to hide one in.
//!
//! ## Reclamation
//!
//! The payload is a `Vec<u8>` — *not* tagged words. So the `free_obj` arm
//! reclaims the block with no child cascade, the way [`crate::task::FutureObj`]
//! does and unlike `ArrayObj`, whose arm decrefs every element. Getting those
//! two the wrong way round is a double free in one direction and a leak in the
//! other, which is why they are named here.

use core::ffi::c_void;

use crate::heap::{ObjHeader, ObjKind};
use crate::trust::TRUSTED;

/// A binary blob: a header, a trust byte, and owned octets.
///
/// `repr(C)` and header-first like every other kind — `gc::free_obj` and the
/// refcount ops read the kind byte at offset 8 before they know what they are
/// looking at.
#[repr(C)]
pub struct BytesObj {
    /// Kind = [`ObjKind::Bytes`].
    pub header: ObjHeader,
    /// Provenance, not identity: two blobs with the same octets are equal
    /// whatever they were derived from.
    pub trust: u8,
    /// The octets. Owned outright; there are no tagged child words here.
    pub data: Vec<u8>,
}

impl BytesObj {
    pub fn new(data: Vec<u8>, trust: u8) -> Self {
        let len = data.len();
        BytesObj { header: ObjHeader::new(ObjKind::Bytes, len as u32), trust, data }
    }

    /// A blob built by the program itself.
    pub fn trusted(data: Vec<u8>) -> Self {
        Self::new(data, TRUSTED)
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    pub fn is_tainted(&self) -> bool {
        crate::trust::is_tainted(self.trust)
    }
}

impl PartialEq for BytesObj {
    fn eq(&self, other: &Self) -> bool {
        self.data == other.data
    }
}
impl Eq for BytesObj {}

impl core::fmt::Debug for BytesObj {
    /// Length and trust, never the payload: a blob can be megabytes, and a
    /// panic message that dumps a file is not a useful panic message.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "BytesObj({} byte(s), trust={})", self.data.len(), self.trust)
    }
}

// ── Construction ──────────────────────────────────────────────────────────────
//
// Until v1.3.27 a program could not build a blob at all. The only two sources
// were `str.encode()` and reading one off a disk or a socket, and `encode` is
// not a way in: a Jade string is UTF-8 and NUL-terminated, so a zero byte
// truncates it and any value above 127 encodes as two octets rather than one.
// A program with a pixel buffer to hand to a C library had nothing to hand it.
//
// The three below are that way in. They live here rather than in either engine
// because both call them, and because a program can *catch* what they raise —
// so the message text is part of the language and has to be one copy, not two.

/// The largest blob these will build.
///
/// [`ObjHeader::len`] is a `u32` and [`BytesObj::new`] fills it from the initial
/// length, so a longer payload would make the header disagree with `data.len()`
/// — and `len(b)` reads the header on the AOT path while `jrt_bytes_len` reads
/// the vector. Refusing past the boundary keeps the two answers the same.
pub const MAX_LEN: usize = u32::MAX as usize;

/// `bytes.zeros(n)` — `n` zeroed octets.
///
/// The length is checked before anything is allocated. A negative `n` cast
/// straight to `usize` asks for sixteen exabytes, and a failed allocation
/// aborts the process rather than raising, which is not a failure a Jade
/// program can catch.
pub fn zeros(n: i64) -> Result<Vec<u8>, String> {
    if n < 0 {
        return Err(format!("bytes.zeros(): length cannot be negative, got {n}"));
    }
    let n = n as u64;
    if n > MAX_LEN as u64 {
        return Err(format!("bytes.zeros(): length {n} is past the {MAX_LEN} octet limit"));
    }
    let n = n as usize;
    let mut data: Vec<u8> = Vec::new();
    data.try_reserve_exact(n).map_err(|_| format!("bytes.zeros(): cannot allocate {n} octets"))?;
    data.resize(n, 0);
    Ok(data)
}

/// One element of a `bytes.from_ints` array, range-checked.
///
/// Both engines walk their own array representation — the VM holds `VmValue`s
/// and the compiled backend holds tagged words — but both come here for the
/// check and the wording.
pub fn octet(index: usize, value: i64) -> Result<u8, String> {
    u8::try_from(value).map_err(|_| {
        format!("bytes.from_ints(): element {index} is {value}, which is not an octet in 0 to 255")
    })
}

/// The message for a `bytes.from_ints` element that is not an int at all.
///
/// It names the position and not the kind it found. The two engines name a kind
/// differently — the VM has a `VmValue` and the compiled backend has a tagged
/// word — and a program can catch this message, so the one wording that is
/// certainly the same under both is the one that leaves the kind out.
pub fn non_int_element(index: usize) -> String {
    format!("bytes.from_ints(): element {index} is not an int")
}

/// The message for `bytes.from_ints` handed something other than an array.
pub fn not_an_array() -> String {
    "bytes.from_ints(): expected an array of ints".to_string()
}

/// The message for an octet written past the end of a blob.
pub fn index_out_of_range(index: i64, len: usize) -> String {
    format!("index {index} out of bounds (length {len})")
}

/// The message for a value written into a blob that is not an octet.
pub fn value_out_of_range(value: i64) -> String {
    format!("a bytes element is an octet in 0 to 255, got {value}")
}

/// The trust a joined blob carries: the more restrictive of its two inputs.
///
/// One function, because both engines have to pick the same one and the choice
/// is the whole security property. The other choice would make concatenation a
/// way to launder: joining a file's contents onto an empty buffer the program
/// built itself would hand back a *trusted* blob holding the file, and walk
/// straight past the check in `sh.exec`.
pub fn concat_trust(a: u8, b: u8) -> u8 {
    crate::trust::combine(a, b)
}

/// The length of a joined blob, or why it cannot be built.
///
/// [`MAX_LEN`] applies here for the same reason it applies to [`zeros`]: two
/// blobs that each fit in a `u32` can add up to one that does not, and
/// `ObjHeader::len` would then hold the sum modulo 2^32 while the payload held
/// the real thing. The compiled backend answers `len(b)` from that header and
/// the VM answers from the vector, so the two engines would disagree about the
/// same value.
pub fn joined_len(a: usize, b: usize) -> Result<usize, String> {
    let total = a as u64 + b as u64;
    if total > MAX_LEN as u64 {
        return Err(format!(
            "bytes.concat(): the result would be {total} octets, past the {MAX_LEN} octet limit"
        ));
    }
    Ok(total as usize)
}

/// `bytes.concat(a, b)` — the octets of `a` followed by those of `b`.
///
/// A fresh object rather than an extension of either input, because
/// [`ObjHeader::len`] is filled once at construction and `BytesObj` has no
/// `sync_len` the way `ArrayObj` does.
///
/// Used by the compiled backend, which holds raw pointers. The VM does the same
/// two appends itself rather than calling this, because it would need both
/// blobs locked at once to reach two `&BytesObj` and that is a deadlock waiting
/// for a caller who writes `concat(b, a)` on another thread. Both go through
/// [`concat_trust`] and [`joined_len`], which is where the decisions live.
pub fn concat(a: &BytesObj, b: &BytesObj) -> Result<BytesObj, String> {
    let n = joined_len(a.data.len(), b.data.len())?;
    let mut data = Vec::with_capacity(n);
    data.extend_from_slice(&a.data);
    data.extend_from_slice(&b.data);
    Ok(BytesObj::new(data, concat_trust(a.trust, b.trust)))
}

/// Write one octet, or say why it could not be written.
///
/// Shared so `b[i] = v` fails the same way under both engines. The bounds check
/// is the blob's own, not the caller's, because the AOT side reaches this
/// through a C forwarder that has no length of its own.
pub fn set(b: &mut BytesObj, index: i64, value: i64) -> Result<(), String> {
    let len = b.data.len();
    if index < 0 || index as usize >= len {
        return Err(index_out_of_range(index, len));
    }
    let octet = u8::try_from(value).map_err(|_| value_out_of_range(value))?;
    b.data[index as usize] = octet;
    Ok(())
}

// ── C ABI ─────────────────────────────────────────────────────────────────────

/// Allocate a bytes value from `len` octets at `src`. Returns the raw pointer;
/// codegen tags it `TAG_PTR`.
///
/// # Safety
/// `src` must point at `len` readable bytes, or be null when `len` is 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_bytes_new(src: *const u8, len: usize, trust: u8) -> *mut c_void {
    let data = if src.is_null() || len == 0 {
        Vec::new()
    } else {
        unsafe { core::slice::from_raw_parts(src, len) }.to_vec()
    };
    crate::gc::leak_obj(BytesObj::new(data, trust))
}

/// Octet count.
///
/// # Safety
/// `p` must point at a live [`BytesObj`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_bytes_len(p: *const c_void) -> i64 {
    unsafe { (*(p as *const BytesObj)).data.len() as i64 }
}

/// The `i`-th octet as a plain `i64` in 0..=255, or -1 if out of range.
///
/// An octet is an *int*, not a char: a byte is not a Unicode scalar, and
/// pretending otherwise would make `b[0]` and `s[0]` look interchangeable when
/// they mean different things on any non-ASCII input.
///
/// # Safety
/// `p` must point at a live [`BytesObj`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_bytes_get(p: *const c_void, i: i64) -> i64 {
    let b = unsafe { &*(p as *const BytesObj) };
    if i < 0 || i as usize >= b.data.len() {
        return -1;
    }
    b.data[i as usize] as i64
}

/// Borrow the payload. Valid until the object is freed.
///
/// # Safety
/// `p` must point at a live [`BytesObj`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_bytes_data(p: *const c_void) -> *const u8 {
    unsafe { (*(p as *const BytesObj)).data.as_ptr() }
}

/// The trust byte a blob is carrying.
///
/// # Safety
/// `p` must point at a live [`BytesObj`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_bytes_trust(p: *const c_void) -> u8 {
    unsafe { (*(p as *const BytesObj)).trust }
}

// ── AOT operations ────────────────────────────────────────────────────────────
//
// `decode` can fail, and a Jade raise is a `longjmp` that must not unwind
// through a Rust frame. So these use the same pending-error channel `fsf` does:
// the Rust side records a message and returns a sentinel, and the C forwarder
// in `common.c` turns that into a catchable exception.

use core::cell::Cell;
use core::ffi::c_char;

thread_local! {
    static PENDING: Cell<*mut c_char> = const { Cell::new(core::ptr::null_mut()) };
}

fn set_err(msg: String) {
    let s = crate::cstr::emit(msg.as_bytes(), TRUSTED);
    PENDING.with(|p| {
        let old = p.replace(s);
        if !old.is_null() {
            crate::string::free_str(old as *mut u8);
        }
    });
}

/// Drain the pending bytes error (a tagged string the caller owns), or null.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_bytes_take_error() -> *mut c_char {
    PENDING.with(|p| p.replace(core::ptr::null_mut()))
}

/// `s.encode()` — the UTF-8 octets of a tagged string, as a bytes value.
/// The string's trust travels with the octets.
///
/// # Safety
/// `s` must be a live NUL-terminated Jade string with a trust byte at `[-1]`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_bytes_encode(s: *const u8) -> *mut c_void {
    if s.is_null() {
        return crate::gc::leak_obj(BytesObj::trusted(Vec::new()));
    }
    let trust = crate::string::trust_of(s);
    let data = unsafe { crate::cstr::borrow_bytes(s as *const c_char) }.to_vec();
    crate::gc::leak_obj(BytesObj::new(data, trust))
}

/// `b.decode()` — the octets as UTF-8 text. Returns null and records a pending
/// error on invalid UTF-8; reporting beats substituting replacement characters,
/// because a caller that assumed the bytes were text needs to hear otherwise.
///
/// # Safety
/// `p` must point at a live [`BytesObj`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_bytes_decode(p: *const c_void) -> *mut c_char {
    let b = unsafe { &*(p as *const BytesObj) };
    match core::str::from_utf8(&b.data) {
        Ok(s) => crate::cstr::emit(s.as_bytes(), b.trust),
        Err(e) => {
            set_err(format!("bytes.decode(): not valid UTF-8 at byte {}", e.valid_up_to()));
            core::ptr::null_mut()
        }
    }
}

/// `b.slice(start, end)` — a sub-blob, `end` exclusive, both clamped.
///
/// # Safety
/// `p` must point at a live [`BytesObj`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_bytes_slice(p: *const c_void, s: i64, e: i64) -> *mut c_void {
    let b = unsafe { &*(p as *const BytesObj) };
    let len = b.data.len() as i64;
    let start = s.clamp(0, len) as usize;
    let end = e.clamp(start as i64, len) as usize;
    crate::gc::leak_obj(BytesObj::new(b.data[start..end].to_vec(), b.trust))
}

/// `bytes.zeros(n)`. Returns null and records a pending error on a bad length.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_bytes_zeros(n: i64) -> *mut c_void {
    match zeros(n) {
        Ok(data) => crate::gc::leak_obj(BytesObj::trusted(data)),
        Err(m) => {
            set_err(m);
            core::ptr::null_mut()
        }
    }
}

/// `bytes.concat(a, b)`.
///
/// # Safety
/// `a` and `b` must each point at a live [`BytesObj`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_bytes_concat(a: *const c_void, b: *const c_void) -> *mut c_void {
    let (a, b) = unsafe { (&*(a as *const BytesObj), &*(b as *const BytesObj)) };
    match concat(a, b) {
        Ok(out) => crate::gc::leak_obj(out),
        Err(m) => {
            set_err(m);
            core::ptr::null_mut()
        }
    }
}

/// `bytes.from_ints(arr)`. Returns null and records a pending error if `arr` is
/// not an array, or if any element is not an octet.
///
/// The kind is checked before the cast. Nothing upstream proves the argument is
/// an array — the type checker types a package call as `Unknown` — so reading a
/// dict or a struct pointer as an `ArrayObj` would be a wild read rather than a
/// catchable type error.
///
/// # Safety
/// `arr` must be a tagged word from a live Jade value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_bytes_from_ints(arr: i64) -> *mut c_void {
    let w = crate::value::JadeValue::from_bits(arr as u64);
    if !w.is_ptr() {
        set_err(not_an_array());
        return core::ptr::null_mut();
    }
    let p = w.as_ptr() as *const c_void;
    if crate::ffi_coll::jrt_kind_of(p) != crate::heap::ObjKind::Array as i64 {
        set_err(not_an_array());
        return core::ptr::null_mut();
    }
    let len = crate::ffi_coll::jrt_coll_array_len(p);
    let mut data: Vec<u8> = Vec::with_capacity(len.max(0) as usize);
    for i in 0..len {
        let el =
            crate::value::JadeValue::from_bits(crate::ffi_coll::jrt_coll_array_get(p, i) as u64);
        if !el.is_int() {
            set_err(non_int_element(i as usize));
            return core::ptr::null_mut();
        }
        match octet(i as usize, el.as_int()) {
            Ok(b) => data.push(b),
            Err(m) => {
                set_err(m);
                return core::ptr::null_mut();
            }
        }
    }
    crate::gc::leak_obj(BytesObj::trusted(data))
}

/// `b[i] = v`. Returns 0 and records a pending error when the index is past the
/// end or the value is not an octet. An `i32` flag rather than a `bool`, to
/// match `jrt_obj_unique` and the rest of the C surface, which never uses
/// `<stdbool.h>`.
///
/// Deliberately *not* modelled on `jrt_coll_array_set`, which retains what it
/// writes: an array element can be a heap pointer and an octet never is, so
/// increfing here would bump the refcount of whatever unrelated object happens
/// to live at that address.
///
/// # Safety
/// `p` must point at a live [`BytesObj`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jrt_bytes_set(p: *mut c_void, i: i64, v: i64) -> i32 {
    let b = unsafe { &mut *(p as *mut BytesObj) };
    match set(b, i, v) {
        Ok(()) => 1,
        Err(m) => {
            set_err(m);
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_carries_its_kind_and_length() {
        let b = BytesObj::trusted(vec![1, 2, 3]);
        assert_eq!(b.header.kind, ObjKind::Bytes as u8);
        assert_eq!(b.len(), 3);
        assert!(!b.is_tainted());
    }

    /// The whole reason `bytes` is not modelled as a string: a NUL terminates a
    /// Jade string, and 0xFF is not valid UTF-8.
    #[test]
    fn a_blob_survives_a_nul_byte_and_invalid_utf8() {
        let raw = vec![b'a', 0, b'b', 0xFF, 0xFE];
        let b = BytesObj::trusted(raw.clone());
        assert_eq!(b.len(), 5, "a NUL must not terminate the payload");
        assert_eq!(b.as_slice(), &raw[..]);
        assert!(core::str::from_utf8(b.as_slice()).is_err());
    }

    #[test]
    fn equality_ignores_trust() {
        let a = BytesObj::new(vec![7, 8], crate::trust::TRUSTED);
        let b = BytesObj::new(vec![7, 8], crate::trust::TAINTED);
        assert_eq!(a, b, "trust is provenance, not identity");
        assert!(b.is_tainted() && !a.is_tainted());
    }

    #[test]
    fn zeros_refuses_a_negative_length_before_allocating() {
        assert!(zeros(-1).is_err(), "a negative length cast to usize asks for 16 exabytes");
        assert_eq!(zeros(0).expect("zero is a length"), Vec::<u8>::new());
        assert_eq!(zeros(3).expect("three octets"), vec![0, 0, 0]);
    }

    /// `ObjHeader::len` is a `u32` filled once at construction, and the AOT path
    /// answers `len(b)` from it while `jrt_bytes_len` answers from the vector.
    /// Past the boundary the two would disagree, so nothing is built there.
    #[test]
    fn zeros_refuses_a_length_the_header_cannot_hold() {
        assert!(zeros(MAX_LEN as i64 + 1).is_err());
    }

    #[test]
    fn an_octet_is_checked_against_its_range_and_names_its_position() {
        assert_eq!(octet(0, 255).expect("255 is an octet"), 255u8);
        assert_eq!(octet(0, 0).expect("0 is an octet"), 0u8);
        let e = octet(3, 300).expect_err("300 is not an octet");
        assert!(e.contains('3'), "names the position: {e}");
        assert!(e.contains("300"), "names the value: {e}");
        assert!(octet(0, -1).is_err(), "an octet is unsigned");
    }

    /// Trust is the more restrictive of the two. The other choice would make
    /// concatenation a laundering path: joining a file's contents onto an empty
    /// buffer the program built would hand back a *trusted* blob holding it.
    #[test]
    fn concat_joins_the_octets_and_keeps_the_stricter_trust() {
        let clean = BytesObj::trusted(vec![1, 2]);
        let dirty = BytesObj::new(vec![3], crate::trust::TAINTED);
        let joined = concat(&clean, &clean).expect("joins");
        assert_eq!(joined.as_slice(), &[1, 2, 1, 2]);
        assert!(!joined.is_tainted());
        assert!(concat(&clean, &dirty).expect("joins").is_tainted(), "tainted right taints");
        assert!(concat(&dirty, &clean).expect("joins").is_tainted(), "tainted left taints");
    }

    /// The trust rule stands on its own, because the VM cannot call `concat`:
    /// it would need both blobs locked at once, and two tasks joining the same
    /// pair in opposite orders would then deadlock. Both engines read it here.
    #[test]
    fn the_trust_rule_is_one_function_both_engines_read() {
        use crate::trust::{TAINTED, TRUSTED};
        assert_eq!(concat_trust(TRUSTED, TRUSTED), TRUSTED);
        assert_eq!(concat_trust(TRUSTED, TAINTED), TAINTED);
        assert_eq!(concat_trust(TAINTED, TRUSTED), TAINTED);
        assert_eq!(concat_trust(TAINTED, TAINTED), TAINTED);
    }

    /// Two blobs that each fit in the header's `u32` can add up to one that does
    /// not. `zeros` refused past the limit from the start and `concat` did not,
    /// so a compiled binary answered `len()` modulo 2^32 where the VM answered
    /// the real length.
    #[test]
    fn concat_refuses_a_result_the_header_cannot_hold() {
        assert_eq!(joined_len(2, 3).expect("small"), 5);
        assert!(joined_len(MAX_LEN, 1).is_err());
        assert!(joined_len(MAX_LEN, MAX_LEN).is_err());
        assert_eq!(joined_len(MAX_LEN, 0).expect("exactly the limit"), MAX_LEN);
    }

    /// A fresh object rather than an extension of either input: `header.len` is
    /// filled at construction and `BytesObj` has no `sync_len`.
    #[test]
    fn concat_builds_an_object_whose_header_matches_its_payload() {
        let out =
            concat(&BytesObj::trusted(vec![1, 2]), &BytesObj::trusted(vec![3])).expect("joins");
        assert_eq!(out.header.len as usize, out.data.len());
        assert_eq!(out.header.len, 3);
    }

    #[test]
    fn set_writes_one_octet_and_reports_both_ways_of_missing() {
        let mut b = BytesObj::trusted(vec![0, 0, 0]);
        set(&mut b, 1, 200).expect("in range");
        assert_eq!(b.as_slice(), &[0, 200, 0]);

        let e = set(&mut b, 3, 1).expect_err("past the end");
        assert!(e.contains("out of bounds"), "{e}");
        assert!(set(&mut b, -1, 1).is_err(), "a negative index is out of bounds");

        let e = set(&mut b, 0, 256).expect_err("not an octet");
        assert!(e.contains("0 to 255"), "names the range: {e}");
        assert!(set(&mut b, 0, -1).is_err(), "an octet is unsigned");
        assert_eq!(b.as_slice(), &[0, 200, 0], "a refused write changes nothing");
    }

    /// The bounds wording is the VM's `IndexOutOfBounds` display minus the span
    /// prefix a compiled binary does not carry. Keeping them the same sentence
    /// is what lets a program catch one message rather than two.
    #[test]
    fn the_bounds_message_matches_what_the_vm_prints() {
        assert_eq!(index_out_of_range(5, 3), "index 5 out of bounds (length 3)");
    }

    #[test]
    fn an_octet_reads_back_as_an_int_in_range() {
        let b = BytesObj::trusted(vec![0, 127, 255]);
        let p = &b as *const BytesObj as *const c_void;
        assert_eq!(unsafe { jrt_bytes_get(p, 0) }, 0);
        assert_eq!(unsafe { jrt_bytes_get(p, 2) }, 255);
        assert_eq!(unsafe { jrt_bytes_get(p, 3) }, -1, "out of range reports, not panics");
        assert_eq!(unsafe { jrt_bytes_get(p, -1) }, -1);
    }
}
