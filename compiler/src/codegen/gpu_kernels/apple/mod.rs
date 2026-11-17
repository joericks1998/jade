//! The kernels for Apple's GPU, as Metal source.
//!
//! Apple publishes no instruction set for its GPUs, so these kernels are the
//! one GPU code jade does not write through LLVM: the program carries
//! `kernels.metal` as text, and Metal compiles it for the GPU the first time
//! the program runs a prompt. `codegen/infer/metal.rs` is the CPU code that
//! does that and launches them.

/// The Metal kernels, one per name in `crate::infer::plan::KERNELS`.
pub(crate) const SOURCE: &str = include_str!("kernels.metal");
