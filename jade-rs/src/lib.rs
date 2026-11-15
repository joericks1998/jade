//! Write Jade packages in Rust.
//!
//! A Rust package is a library crate that marks what a Jade program may call with
//! `#[jade::export]`:
//!
//! ```
//! #[jade::export]
//! pub struct Point {
//!     pub x: f64,
//!     pub y: f64,
//! }
//!
//! #[jade::export]
//! pub fn midpoint(a: Point, b: Point) -> Point {
//!     Point { x: (a.x + b.x) / 2.0, y: (a.y + b.y) / 2.0 }
//! }
//!
//! #[jade::export]
//! pub fn shout(s: &str) -> Result<String, String> {
//!     if s.is_empty() { Err("nothing to shout".into()) } else { Ok(s.to_uppercase()) }
//! }
//! ```
//!
//! A Jade project names the crate in `jade.toml` with `abi = "rust"`, and calls
//! `pkg.midpoint(...)` and `pkg.shout(...)` like any package. An `Err` raises its
//! text in the Jade program, and so does a panic, which never unwinds into Jade code.
//!
//! On a function, the attribute writes a `#[no_mangle] extern "C"` wrapper and a
//! companion that frees what the wrapper returned. On a struct, it writes a
//! `#[repr(C)]` mirror, which the Rust compiler lays out by C's rules. On both, it
//! adds a description to the built library, which the Jade compiler reads in place of
//! a C header. `design/v2-rust-packages.md` in the Jade repo has the design.
//!
//! *The types that cross:* `i64`, `isize` (`int`), `f64` (`float`), `bool`, `char`,
//! `u8` (`byte`), `i16`, `u16`, `i32`, `u32`, `u64`, `usize` (`ulong`), `f32`, `&str`
//! and `String` (`str`), `&[u8]` and `Vec<u8>` (`bytes`), `Vec<T>` and `&[T]` (`[T]`),
//! `Option<T>` (`T?`), an exported struct (a class with public fields), and, as the
//! whole result, `Result<T, E>` with `E: Display`.

// The generated code names this crate `::jade`, which is how a package sees it. This
// line makes that path work inside the crate too, for its own tests.
extern crate self as jade;

mod c;
mod call;
mod convert;
mod meta;

pub use jade_rs_macros::export;

/// The parts of this crate the generated code calls. Not an API: a package never
/// names anything here, and it changes with the description's format.
#[doc(hidden)]
pub mod __private {
    pub use crate::c::{COpt, CVec};
    pub use crate::call::{call, free, lift};
    pub use crate::convert::{Borrowed, Elem, Lift, Lower, Return, Type};
    pub use crate::meta::{Meta, FORMAT};
}

#[cfg(test)]
mod tests;
