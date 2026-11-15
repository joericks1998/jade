//! The conversions between a Rust value and its C form.
//!
//! Three traits, named for the two directions as UniFFI names them. [`Lift`] reads an
//! argument the Jade program lent for the call; [`Lower`] hands a result over, to be
//! copied by the program and then released with [`Lower::free`]. [`Return`] is what a
//! function's whole result implements, so `Result<T, E>` can lower as `T` and raise
//! its error, which no other position allows.
//!
//! A type with no impl here has no Jade form, and the build stops with the message on
//! [`Type`], which lists the types that do.

use crate::c::{COpt, CVec};
use crate::meta::Meta;

/// Purpose: This trait says what a type is called in Jade and what its C form is.
///
/// # Safety
///
/// `C` must be the C form the Jade compiler builds and reads for the type `JADE`
/// spells, and must be valid when zeroed. `#[jade::export]` implements it for a
/// struct; nothing else should.
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot cross between Rust and Jade",
    label = "Jade has no type for `{Self}`",
    note = "the types that cross are i64, isize, f64, bool, char, u8, i16, u16, i32, u32, \
            u64, usize, f32, &str, String, &[u8], Vec<u8>, Vec<T>, &[T], Option<T>, a struct \
            marked #[jade::export], and Result<T, E> as a whole result",
    note = "an iterator cannot be returned yet; return a Vec (iterators will connect to \
            Jade's pipes)"
)]
pub unsafe trait Type {
    /// The C form.
    type C: Copy + 'static;
    /// The type as a Jade program writes it, such as `[str]` or `int?`.
    const JADE: Meta;
}

/// Purpose: This trait reads an argument from the C form the Jade program built.
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be passed from Jade",
    label = "Jade has no type for `{Self}`",
    note = "a parameter takes i64, isize, f64, bool, char, u8, i16, u16, i32, u32, u64, usize, \
            f32, &str, String, &[u8], Vec<u8>, Vec<T>, &[T], Option<T>, a struct marked \
            #[jade::export], or a & of any of these"
)]
pub trait Lift<'a>: Type + Sized {
    /// Purpose: This function reads the value.
    ///
    /// Inputs:
    /// - `c`: the C form, lent for the call.
    ///
    /// Outputs: the value, or the message to raise when the C form holds something
    /// Rust cannot take, such as text that is not UTF-8.
    ///
    /// # Safety
    ///
    /// `c` must be a C form of `JADE`'s type, every pointer in it valid for `'a`.
    unsafe fn lift(c: &'a Self::C) -> Result<Self, String>;
}

/// Purpose: This trait hands a result over in its C form.
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be returned to Jade",
    label = "Jade has no type for `{Self}`",
    note = "a function returns i64, isize, f64, bool, char, u8, i16, u16, i32, u32, u64, usize, \
            f32, &str, String, &[u8], Vec<u8>, Vec<T>, &[T], Option<T>, a struct marked \
            #[jade::export], (), or Result<T, E> of one of these with E: Display",
    note = "an iterator cannot be returned yet; return a Vec (iterators will connect to \
            Jade's pipes)"
)]
pub trait Lower: Type + Sized {
    /// Purpose: This function makes the C form, allocating what it points at.
    ///
    /// Inputs: none.
    ///
    /// Outputs: the C form, which owns its allocations until [`Lower::free`].
    fn lower(self) -> Self::C;

    /// Purpose: This function releases what [`Lower::lower`] allocated.
    ///
    /// Inputs:
    /// - `c`: the C form, which is not read again.
    ///
    /// Outputs: none.
    ///
    /// # Safety
    ///
    /// `c` must have come from `lower` and not been freed already.
    unsafe fn free(c: &mut Self::C);
}

/// Purpose: This trait is what an exported function's whole result implements.
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be returned to Jade",
    note = "a function returns a type that crosses, `()`, or Result<T, E> where T crosses \
            and E implements Display",
    note = "an iterator cannot be returned yet; return a Vec (iterators will connect to \
            Jade's pipes)"
)]
pub trait Return {
    /// The C form of what a call that does not raise hands back.
    type C: Copy + 'static;
    /// The Jade type of the result.
    const JADE: Meta;
    /// Whether the function returns a `Result`.
    const RAISES: bool;

    /// Purpose: This function makes the result's C form.
    ///
    /// Inputs: none.
    ///
    /// Outputs: the C form, or the message to raise.
    fn lower_return(self) -> Result<Self::C, String>;

    /// Purpose: This function releases a C form `lower_return` made.
    ///
    /// # Safety
    ///
    /// As for [`Lower::free`].
    unsafe fn free(c: &mut Self::C);
}

impl<T: Lower> Return for T {
    type C = T::C;
    const JADE: Meta = T::JADE;
    const RAISES: bool = false;
    fn lower_return(self) -> Result<Self::C, String> {
        Ok(self.lower())
    }
    unsafe fn free(c: &mut Self::C) {
        T::free(c)
    }
}

impl<T: Lower, E: std::fmt::Display> Return for Result<T, E> {
    type C = T::C;
    const JADE: Meta = T::JADE;
    const RAISES: bool = true;
    fn lower_return(self) -> Result<Self::C, String> {
        self.map(Lower::lower).map_err(|e| e.to_string())
    }
    unsafe fn free(c: &mut Self::C) {
        T::free(c)
    }
}

/// Purpose: This trait marks every type that may be an element of a `Vec`, which is
/// every type that crosses but `u8`: a `Vec<u8>` is `bytes`, not an array of bytes.
///
/// # Safety
///
/// As for [`Type`].
pub unsafe trait Elem: Type {}

/// Purpose: This trait names the value the wrapper owns for a `&T` argument, which it
/// lifts and then lends to the function: `T` for a sized type, `Vec<T>` for `[T]`.
pub trait Borrowed {
    /// What is lifted and lent.
    type Owned: std::borrow::Borrow<Self>;
}

impl<T: Type> Borrowed for T {
    type Owned = T;
}

impl<T: Elem> Borrowed for [T] {
    type Owned = Vec<T>;
}

impl Borrowed for str {
    type Owned = String;
}

// ── Numbers ──────────────────────────────────────────────────────────────────

/// Implements the traits for a number whose C form is a number.
macro_rules! number {
    ($($rust:ty => $c:ty, $jade:literal;)*) => {$(
        unsafe impl Type for $rust {
            type C = $c;
            const JADE: Meta = Meta::new().str($jade);
        }
        unsafe impl Elem for $rust {}
        impl<'a> Lift<'a> for $rust {
            unsafe fn lift(c: &'a $c) -> Result<Self, String> {
                Ok(*c as $rust)
            }
        }
        impl Lower for $rust {
            fn lower(self) -> $c {
                self as $c
            }
            unsafe fn free(_: &mut $c) {}
        }
    )*};
}

number! {
    i64 => i64, "int";
    isize => i64, "int";
    f64 => f64, "float";
    i16 => i16, "short";
    u16 => u16, "ushort";
    i32 => i32, "int32";
    u32 => u32, "uint32";
    u64 => u64, "ulong";
    usize => u64, "ulong";
    f32 => f32, "float32";
}

// `u8` is a number too, but not an `Elem`.
unsafe impl Type for u8 {
    type C = u8;
    const JADE: Meta = Meta::new().str("byte");
}
impl<'a> Lift<'a> for u8 {
    unsafe fn lift(c: &'a u8) -> Result<Self, String> {
        Ok(*c)
    }
}
impl Lower for u8 {
    fn lower(self) -> u8 {
        self
    }
    unsafe fn free(_: &mut u8) {}
}

unsafe impl Type for bool {
    type C = u8;
    const JADE: Meta = Meta::new().str("bool");
}
unsafe impl Elem for bool {}
impl<'a> Lift<'a> for bool {
    unsafe fn lift(c: &'a u8) -> Result<Self, String> {
        Ok(*c != 0)
    }
}
impl Lower for bool {
    fn lower(self) -> u8 {
        u8::from(self)
    }
    unsafe fn free(_: &mut u8) {}
}

unsafe impl Type for char {
    type C = u32;
    const JADE: Meta = Meta::new().str("char");
}
unsafe impl Elem for char {}
impl<'a> Lift<'a> for char {
    unsafe fn lift(c: &'a u32) -> Result<Self, String> {
        char::from_u32(*c).ok_or_else(|| format!("{c:#x} is not a Unicode scalar"))
    }
}
impl Lower for char {
    fn lower(self) -> u32 {
        u32::from(self)
    }
    unsafe fn free(_: &mut u32) {}
}

/// `()` is a function that returns nothing: Jade's `nil`.
unsafe impl Type for () {
    type C = u8;
    const JADE: Meta = Meta::new().str("nil");
}
impl Lower for () {
    fn lower(self) -> u8 {
        0
    }
    unsafe fn free(_: &mut u8) {}
}

// ── Text and bytes ───────────────────────────────────────────────────────────

/// Purpose: This function reads text the Jade program lent.
fn utf8(b: &[u8]) -> Result<&str, String> {
    std::str::from_utf8(b).map_err(|e| format!("a str argument is not UTF-8: {e}"))
}

/// Purpose: This function releases the bytes a `Lower` impl handed over.
///
/// # Safety
///
/// `c` must hold a `Vec<u8>` handed over by `CVec::from_vec`.
unsafe fn free_bytes(c: &mut CVec<u8>) {
    drop(c.into_vec());
}

unsafe impl Type for &str {
    type C = CVec<u8>;
    const JADE: Meta = Meta::new().str("str");
}
unsafe impl Elem for &str {}
impl<'a> Lift<'a> for &'a str {
    unsafe fn lift(c: &'a CVec<u8>) -> Result<Self, String> {
        utf8(c.as_slice())
    }
}
/// A borrowed result is copied before the arguments it may borrow from are let go.
impl Lower for &str {
    fn lower(self) -> CVec<u8> {
        CVec::from_vec(self.as_bytes().to_vec())
    }
    unsafe fn free(c: &mut CVec<u8>) {
        free_bytes(c)
    }
}

unsafe impl Type for String {
    type C = CVec<u8>;
    const JADE: Meta = Meta::new().str("str");
}
unsafe impl Elem for String {}
impl<'a> Lift<'a> for String {
    unsafe fn lift(c: &'a CVec<u8>) -> Result<Self, String> {
        utf8(c.as_slice()).map(str::to_owned)
    }
}
impl Lower for String {
    fn lower(self) -> CVec<u8> {
        CVec::from_vec(self.into_bytes())
    }
    unsafe fn free(c: &mut CVec<u8>) {
        free_bytes(c)
    }
}

unsafe impl Type for &[u8] {
    type C = CVec<u8>;
    const JADE: Meta = Meta::new().str("bytes");
}
unsafe impl Elem for &[u8] {}
impl<'a> Lift<'a> for &'a [u8] {
    unsafe fn lift(c: &'a CVec<u8>) -> Result<Self, String> {
        Ok(c.as_slice())
    }
}
impl Lower for &[u8] {
    fn lower(self) -> CVec<u8> {
        CVec::from_vec(self.to_vec())
    }
    unsafe fn free(c: &mut CVec<u8>) {
        free_bytes(c)
    }
}

unsafe impl Type for Vec<u8> {
    type C = CVec<u8>;
    const JADE: Meta = Meta::new().str("bytes");
}
unsafe impl Elem for Vec<u8> {}
impl<'a> Lift<'a> for Vec<u8> {
    unsafe fn lift(c: &'a CVec<u8>) -> Result<Self, String> {
        Ok(c.as_slice().to_vec())
    }
}
impl Lower for Vec<u8> {
    fn lower(self) -> CVec<u8> {
        CVec::from_vec(self)
    }
    unsafe fn free(c: &mut CVec<u8>) {
        free_bytes(c)
    }
}

// ── Arrays and optionals ─────────────────────────────────────────────────────

unsafe impl<T: Elem> Type for Vec<T> {
    type C = CVec<T::C>;
    const JADE: Meta = Meta::new().str("[").meta(&T::JADE).str("]");
}
unsafe impl<T: Elem> Elem for Vec<T> {}
impl<'a, T: Elem + Lift<'a>> Lift<'a> for Vec<T> {
    unsafe fn lift(c: &'a CVec<T::C>) -> Result<Self, String> {
        c.as_slice().iter().map(|x| T::lift(x)).collect()
    }
}
impl<T: Elem + Lower> Lower for Vec<T> {
    fn lower(self) -> CVec<T::C> {
        CVec::from_vec(self.into_iter().map(Lower::lower).collect())
    }
    unsafe fn free(c: &mut CVec<T::C>) {
        let mut items = c.into_vec();
        for x in items.iter_mut() {
            T::free(x);
        }
    }
}

/// A borrowed array result is copied, element by element, as `Vec<T>` would be.
unsafe impl<T: Elem> Type for &[T] {
    type C = CVec<T::C>;
    const JADE: Meta = Meta::new().str("[").meta(&T::JADE).str("]");
}
impl<T: Elem + Lower + Clone> Lower for &[T] {
    fn lower(self) -> CVec<T::C> {
        CVec::from_vec(self.iter().cloned().map(Lower::lower).collect())
    }
    unsafe fn free(c: &mut CVec<T::C>) {
        <Vec<T> as Lower>::free(c)
    }
}

unsafe impl<T: Type> Type for Option<T> {
    type C = COpt<T::C>;
    const JADE: Meta = Meta::new().meta(&T::JADE).str("?");
}
unsafe impl<T: Type> Elem for Option<T> {}
impl<'a, T: Lift<'a>> Lift<'a> for Option<T> {
    unsafe fn lift(c: &'a COpt<T::C>) -> Result<Self, String> {
        if c.some == 0 {
            return Ok(None);
        }
        T::lift(&c.value).map(Some)
    }
}
impl<T: Lower> Lower for Option<T> {
    fn lower(self) -> COpt<T::C> {
        match self {
            Some(v) => COpt { some: 1, value: v.lower() },
            // SAFETY: every C form is plain data, valid when zeroed (see `Type`).
            None => COpt { some: 0, value: unsafe { std::mem::zeroed() } },
        }
    }
    unsafe fn free(c: &mut COpt<T::C>) {
        if c.some != 0 {
            T::free(&mut c.value);
        }
    }
}
