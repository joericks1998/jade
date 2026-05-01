pub mod bytecode;
pub mod emit;
pub mod tir;
pub mod type_infer;
pub mod vm;

#[cfg(feature = "llvm")]
pub mod codegen;

/// Runtime shim exported as `#[no_mangle] extern "C"` symbols for LLVM-compiled
/// binaries.  Contains `jade_infer` (prompt dereference via /dev/jade).
pub mod runtime;
