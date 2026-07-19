//! # jade-runtime
//!
//! The shared runtime for the Jade language. It exists to hold Jade's value
//! semantics in **one** place so the two execution engines cannot drift:
//!
//!  * the bytecode **VM** (`jade run`) links this crate as an `rlib` and calls
//!    it natively;
//!  * **AOT-compiled binaries** (`jade build`) link it as a C-ABI `staticlib`
//!    (`#[no_mangle] extern "C"` entry points), replacing the hand-rolled
//!    semantics that used to live in `jade-buildd/runtime_lib/common.c`.
//!
//! Historically the VM (Rust, `VmValue` + `Arc`) and the AOT backend (C,
//! `jrt_*_any`) were two independent implementations of the same language, and
//! every divergence between them was a bug reconciled after the fact. This
//! crate is the structural fix: shared code, one behavior.
//!
//! ## Contents (built out across the migration stages)
//!
//!  * [`value`] — the tagged 64-bit value ABI (`JadeValue`), byte-identical to
//!    `runtime.h`. Pure bit-twiddling, no allocation. **(Stage 0)**
//!  * [`heap`] — the unified [`heap::ObjHeader`] for reference types:
//!    refcount + cycle-collector color/flags. **(Stage 0)**
//!  * [`sys`] — system-allocator bindings (`malloc`/`free`) so heap objects are
//!    interchangeable with the C runtime. **(Stage 1)**
//!  * [`float`] — boxed floats; [`num`] — integer pow. **(Stage 1)**
//!  * [`ffi`] — the `#[no_mangle] extern "C"` `jrt_*` surface AOT binaries
//!    link against. **(Stage 1)**
//!  * [`ops`] — dynamic (tag-erased) arithmetic/comparison/truthiness, the
//!    divergence-prone core, returning errors as values; [`strval`] — the
//!    string-value helpers they need. **(Stage 1)**
//!  * [`string`] — the tagged-string allocator (`new`/`dup`/`free`/`concat`/
//!    `trust_of`). **(Stage 1)**
//!  * [`coll`] — the shared heap collections (dict/array/struct), generic over
//!    the element word type so the VM (`VmValue`) and AOT (`i64`) share one
//!    implementation; value/reference semantics fall out of `T: Clone`. The
//!    refcount/cycle-collector wiring on their [`heap::ObjHeader`] is a later
//!    increment. **(this brick)**
//!
//! It is intentionally dependency-free and LLVM-free so it builds everywhere
//! `jade run` runs.

pub mod coll;
pub mod dynop;
pub mod envf;
pub mod ffi;
pub mod ffi_coll;
pub mod float;
pub mod fsf;
pub mod gc;
pub mod grammarf;
pub mod heap;
pub mod httpf;
pub mod jsonf;
pub mod mathf;
pub mod methods;
pub mod num;
pub mod ops;
pub mod pathf;
pub mod randomf;
pub mod render;
pub mod shf;
pub mod string;
pub mod task;
pub mod strval;
pub mod sys;
pub mod timef;
pub mod value;

pub use coll::{ArrayObj, DictObj, StructObj};
pub use heap::{Color, ObjHeader, ObjKind};
pub use value::{JadeValue, FALSE, NIL, TRUE};

/// ABI version of this runtime. Bumped when the shared value/heap ABI changes
/// so a mismatched AOT binary and daemon can be detected. Exposed over the C
/// ABI so AOT-linked binaries can assert compatibility.
pub const RUNTIME_ABI_VERSION: u32 = 1;

/// C-ABI accessor for [`RUNTIME_ABI_VERSION`]. Also serves as a trivial
/// exported symbol proving the `staticlib` links.
#[unsafe(no_mangle)]
pub extern "C" fn jrt_abi_version() -> u32 {
    RUNTIME_ABI_VERSION
}
