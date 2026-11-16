//! The support code codegen writes into each program: what v1 called the
//! runtime, emitted as internal LLVM functions named `jade.*` the first time
//! something asks for one.
//!
//! Each file adds methods to [`Cx`](super::Cx) for one kind of value or one
//! service:
//!
//! - `text.rs`: strings, and every string method.
//! - `unicode.rs`: the case tables behind `upper`, `lower` and `capitalize`.
//! - `bytes.rs`: binary blobs.
//! - `collections.rs`: arrays and dicts.
//! - `compare.rs`: equality and ordering, per type.
//! - `format.rs`: text for values, output, input, and conversions.
//! - `sized.rs`: arithmetic at each number width.
//! - `todict.rs`: `toDict(text)`, a JSON reader per dict type.
//! - `tasks.rs`: the job queue behind `async fn`, `await` and `join`.

mod bytes;
mod collections;
mod compare;
mod format;
mod sized;
pub(super) mod tasks;
mod text;
mod todict;
mod unicode;
