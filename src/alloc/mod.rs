//! The `jade` binary's global allocators.
//!
//! Two of them, and only ever one at a time:
//!
//!  * [`pool`] — the production allocator, a thin `GlobalAlloc` shell over the
//!    size-classed free list in `jade_runtime::pool`.
//!  * [`profile`] — a measuring allocator behind `--features alloc-profile`,
//!    which wraps the system allocator and records a size-class histogram.
//!
//! **Host-only by construction.** Both are declared as `#[global_allocator]` in
//! `main.rs`, in the *binary*, and never in the shared `jade-runtime` crate.
//! That placement is the whole point rather than an accident of layout: a global
//! allocator declared in `jade-runtime` is linked into every native package too,
//! so a process that `dlopen`s one ends up holding two allocator instances whose
//! duplicate symbols interpose across the boundary. That is exactly what mimalloc
//! did here — it corrupted the heap and deadlocked tokio's shutdown, and removing
//! it is why this module exists in the shape it does.
//!
//! The pool *implementation* still lives in `jade-runtime`, because the AOT path
//! calls it directly from `gc::leak_obj`/`free_obj` and both engines should share
//! one free list. What lives here is only the `GlobalAlloc` adapter. The
//! distinction is worth holding onto: sharing an allocator's *code* is safe,
//! sharing its *global-allocator registration* is not.

pub mod pool;

#[cfg(feature = "alloc-profile")]
pub mod profile;
