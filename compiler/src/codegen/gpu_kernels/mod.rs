//! The GPU code a program runs a model with: one kernel per op of a model's
//! forward step, for each kind of GPU.
//!
//! Codegen writes two kinds of code. The rest of `codegen/` writes the CPU
//! code of a program, `codegen/infer/` included, which opens the model's
//! memory and launches each step. This directory writes the code that runs on
//! the GPU. The kernels are the same for every program built against a plan:
//! each takes up to four source tensors, a destination, and an argument record
//! saying every tensor's shape and type and the op's parameters, so one
//! kernel serves every shape the plan gives it.
//!
//! - *`apple/`* holds Apple's kernels as Metal source, since Apple publishes
//!   no instruction set for its GPUs; Metal compiles them when the program
//!   starts.
//! - *`nvidia/`* builds the same kernels through LLVM's NVPTX backend into
//!   PTX, which NVIDIA's driver compiles for the card when the program loads
//!   it.
//! - *`builder.rs`* is `K`, the builder the LLVM kernels are written in, and
//!   the few things that differ between GPUs.
//! - *`values.rs`* reads and writes one value of each type the kernels take,
//!   and decodes 32 weights of a block type at a time.
//! - *`elementwise.rs`*, *`norm.rs`*, *`matmul.rs`* and *`attention.rs`* are
//!   the kernels, in groups by how their threads share the work.
//!
//! `crate::infer::plan::KERNELS` is the contract between a plan and every GPU
//! here: an op record names a kernel by its place in that list, and every GPU
//! has a kernel of each name.

// The CPU code that launches the NVIDIA kernels comes in the next release;
// until then only the tests build them.
#![cfg_attr(not(test), allow(dead_code))]

pub(crate) mod apple;
pub(crate) mod nvidia;

mod attention;
mod builder;
mod elementwise;
mod matmul;
mod norm;
mod values;

#[cfg(test)]
mod emulate;
#[cfg(test)]
mod tests;

/// The bytes of the argument record every kernel takes: five tensors of 96
/// bytes, `ne` and `nb` four times each and the ggml type; then eight f32
/// parameters and eight i64 parameters.
pub(crate) const ARGS_BYTES: i64 = 5 * 96 + 8 * 4 + 8 * 8;

/// Purpose: This function writes every kernel into a module, in the order of
/// `crate::infer::plan::KERNELS`.
///
/// Inputs:
/// - `g`: the module.
fn write_all(g: &builder::Gpu<'_>) {
    elementwise::get_rows(g);
    norm::rms_norm(g);
    elementwise::mul(g);
    elementwise::add(g);
    matmul::mul_mat(g);
    elementwise::rope(g);
    elementwise::cpy(g);
    elementwise::set_rows(g);
    elementwise::swiglu(g);
    attention::flash_attn(g);
    matmul::mul_mm(g);
}
