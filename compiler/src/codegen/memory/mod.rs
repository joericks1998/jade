//! How a value sits in memory, and when it is freed.
//!
//! `layout.rs` maps each Jade type to the LLVM type that holds it, and names
//! the byte offsets inside heap objects. `rc.rs` writes retain and release:
//! every heap object carries a reference count, and the last release frees
//! the object and releases what it holds.

pub(super) mod layout;
pub(super) mod rc;
