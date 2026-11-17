//! NVIDIA's kernels, built through LLVM's NVPTX backend into PTX.
//!
//! PTX is NVIDIA's portable GPU code. A program carries it as text, and the
//! driver compiles it for the card in the machine when the program loads it,
//! with `cuModuleLoadData`, so the program needs only the driver and no CUDA
//! toolkit. The PTX targets `sm_86`, the RTX 3090's generation, and runs on
//! any newer card too, such as the RTX 2000 Ada, `sm_89`: the driver compiles
//! it for the card it finds. LLVM 18 writes PTX version 7.1 for `sm_86`,
//! which drivers from CUDA 11.1 on read.
//!
//! Every kernel takes six parameters: the four source tensors and the
//! destination, each a device pointer to the tensor's first byte or 0 for
//! none, and the argument record, `ARGS_BYTES` bytes passed by value. It is
//! launched in blocks of `BLOCK` threads, as on Metal: the grid is the op's
//! threads divided by `BLOCK`, rounded up.

use std::sync::OnceLock;

use inkwell::OptimizationLevel;
use inkwell::context::Context;
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine, TargetTriple,
};

use super::builder::{Gpu, Vendor};

#[cfg(test)]
mod tests;

/// The target LLVM builds for: 64-bit PTX for CUDA.
const TRIPLE: &str = "nvptx64-nvidia-cuda";

/// The oldest GPU generation the PTX runs on.
pub(crate) const ARCH: &str = "sm_86";

/// Purpose: This function makes LLVM's machine for NVIDIA GPUs.
///
/// Outputs: the machine, or why LLVM could not make one.
fn machine() -> Result<TargetMachine, String> {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| Target::initialize_nvptx(&InitializationConfig::default()));
    let triple = TargetTriple::create(TRIPLE);
    let target =
        Target::from_triple(&triple).map_err(|e| format!("this LLVM has no NVPTX backend: {e}"))?;
    target
        .create_target_machine(
            &triple,
            ARCH,
            "",
            OptimizationLevel::Aggressive,
            RelocMode::Default,
            CodeModel::Default,
        )
        .ok_or_else(|| "LLVM could not make a target machine for NVIDIA GPUs".to_string())
}

/// Purpose: This function builds every kernel into PTX.
///
/// Outputs: the PTX text, one `.entry` per name in
/// `crate::infer::plan::KERNELS`, or LLVM's message.
pub(crate) fn ptx() -> Result<String, String> {
    let ctx = Context::create();
    let g = Gpu::new(&ctx, Vendor::Nvidia);
    let machine = machine()?;
    // Before any kernel is written, since each load takes its alignment from
    // the data layout: under LLVM's default one an i64 is 4-byte aligned, and
    // NVPTX splits every i64 load in two.
    g.module.set_triple(&machine.get_triple());
    g.module.set_data_layout(&machine.get_target_data().get_data_layout());
    super::write_all(&g);
    g.module
        .verify()
        .map_err(|e| format!("the NVIDIA kernels do not verify: {}", e.to_string()))?;
    g.module
        .run_passes("default<O3>", &machine, PassBuilderOptions::create())
        .map_err(|e| e.to_string())?;
    let buf =
        machine.write_to_memory_buffer(&g.module, FileType::Assembly).map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(buf.as_slice()).trim_end_matches('\0').to_string())
}
