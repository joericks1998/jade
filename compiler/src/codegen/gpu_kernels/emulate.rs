//! Running the kernels on the CPU, for tests, so their math is checked on a
//! machine with no GPU.
//!
//! The kernels are built for this machine, with [`Vendor::Host`], and
//! compiled in this process with LLVM's JIT. Each block of a launch runs as
//! `BLOCK` CPU threads, one per GPU thread, so the code between a warp sum or
//! a barrier runs as freely as on a GPU, and those two meet the way a GPU's
//! threads do: a warp sum waits for the warp's 32 threads and adds their
//! values in the same order NVIDIA's shuffles do, and a barrier waits for the
//! block. Only the few methods of `builder.rs` that differ per GPU are not
//! checked here; the NVIDIA tests check those on a GPU.

use std::collections::HashMap;
use std::sync::Barrier;
use std::sync::atomic::{AtomicU32, Ordering};

use inkwell::OptimizationLevel;
use inkwell::context::Context;
use inkwell::execution_engine::ExecutionEngine;
use inkwell::passes::PassBuilderOptions;

use super::builder::{BLOCK, Gpu, SHARED_MAX, Vendor, WARP};
use super::tests::Runner;
use crate::infer::plan::KERNELS;

/// Purpose: This struct holds where one emulated thread is, as the host's
/// kernels read it: words 0 to 2 and the pointer at byte 24, per
/// `builder.rs`.
#[repr(C)]
struct Env {
    tid: i64,
    ctaid: i64,
    ntid: i64,
    shared: *mut u8,
    block: *const Block,
}

/// Purpose: This struct holds what the threads of one block meet at.
struct Block {
    all: Barrier,
    warps: Vec<Warp>,
}

/// Purpose: This struct holds what the threads of one warp meet at for a sum.
struct Warp {
    meet: Barrier,
    values: [AtomicU32; WARP as usize],
}

/// Purpose: This function is the host's barrier: it waits for every thread
/// of the block.
extern "C" fn jade_gpu_barrier(env: *const Env) {
    // SAFETY: the kernel passes on the pointer `run` gave it, to an `Env`
    // and a `Block` that outlive the block's threads.
    let block = unsafe { &*(*env).block };
    block.all.wait();
}

/// Purpose: This function is the host's warp sum: it waits for the warp's 32
/// threads and adds their values as NVIDIA's butterfly shuffles do, so every
/// thread gets the same sum in the same order.
extern "C" fn jade_gpu_warp_sum(env: *const Env, v: f32) -> f32 {
    // SAFETY: as in `jade_gpu_barrier`.
    let (env, block) = unsafe { (&*env, &*(*env).block) };
    let lane = (env.tid % WARP) as usize;
    let warp = &block.warps[(env.tid / WARP) as usize];
    warp.values[lane].store(v.to_bits(), Ordering::Relaxed);
    warp.meet.wait();
    let mut vals: Vec<f32> =
        warp.values.iter().map(|x| f32::from_bits(x.load(Ordering::Relaxed))).collect();
    for lanes in [16, 8, 4, 2, 1] {
        let prev = vals.clone();
        for (l, v) in vals.iter_mut().enumerate() {
            *v = prev[l] + prev[l ^ lanes];
        }
    }
    // No thread may write the next sum's value before every thread has read
    // this one's.
    warp.meet.wait();
    vals[lane]
}

/// A kernel built for the host: the five tensors, the argument record, and
/// where the thread is.
type KernelFn = unsafe extern "C" fn(usize, usize, usize, usize, usize, *const u8, *const Env);

/// Purpose: This struct holds the kernels compiled for this machine.
pub(super) struct Emulator {
    kernels: HashMap<&'static str, usize>,
    _engine: ExecutionEngine<'static>,
    _gpu: Gpu<'static>,
}

impl Emulator {
    /// Purpose: This function builds every kernel for this machine and
    /// compiles them.
    ///
    /// Outputs: the emulator, or LLVM's message.
    pub fn new() -> Result<Self, String> {
        // The engine borrows the context for as long as the tests run.
        let ctx: &'static Context = Box::leak(Box::new(Context::create()));
        let g = Gpu::new(ctx, Vendor::Host);
        let machine = crate::codegen::target_machine()?;
        g.module.set_triple(&machine.get_triple());
        g.module.set_data_layout(&machine.get_target_data().get_data_layout());
        super::write_all(&g);
        g.module.verify().map_err(|e| e.to_string())?;
        g.module
            .run_passes("default<O2>", &machine, PassBuilderOptions::create())
            .map_err(|e| e.to_string())?;
        ExecutionEngine::link_in_mc_jit();
        let engine = g
            .module
            .create_jit_execution_engine(OptimizationLevel::Default)
            .map_err(|e| e.to_string())?;
        if let Some(f) = g.module.get_function("jade_gpu_barrier") {
            engine.add_global_mapping(&f, jade_gpu_barrier as *const () as usize);
        }
        if let Some(f) = g.module.get_function("jade_gpu_warp_sum") {
            engine.add_global_mapping(&f, jade_gpu_warp_sum as *const () as usize);
        }
        let mut kernels = HashMap::new();
        for name in KERNELS {
            let addr = engine.get_function_address(name).map_err(|e| format!("{name}: {e:?}"))?;
            kernels.insert(*name, addr);
        }
        Ok(Emulator { kernels, _engine: engine, _gpu: g })
    }
}

impl Runner for Emulator {
    fn name(&self) -> &str {
        "the CPU"
    }

    fn run(
        &mut self,
        kernel: &str,
        bufs: &mut [Vec<u8>; 5],
        args: &[u8],
        blocks: u64,
    ) -> Result<(), String> {
        let addr = *self.kernels.get(kernel).ok_or_else(|| format!("no kernel {kernel}"))?;
        // SAFETY: the address is the JIT's code for a kernel of this
        // signature, per `Gpu::kernel` on the host.
        let f: KernelFn = unsafe { std::mem::transmute::<usize, KernelFn>(addr) };
        let ptrs: Vec<usize> = bufs
            .iter_mut()
            .map(|b| if b.is_empty() { 0 } else { b.as_mut_ptr() as usize })
            .collect();
        let args = args.as_ptr() as usize;
        for block in 0..blocks {
            let mut memory = vec![0u8; SHARED_MAX as usize];
            let shared = memory.as_mut_ptr() as usize;
            let b = Block {
                all: Barrier::new(BLOCK as usize),
                warps: (0..BLOCK / WARP)
                    .map(|_| Warp {
                        meet: Barrier::new(WARP as usize),
                        values: std::array::from_fn(|_| AtomicU32::new(0)),
                    })
                    .collect(),
            };
            let b = &b;
            let ptrs = &ptrs;
            std::thread::scope(|s| {
                for tid in 0..BLOCK {
                    s.spawn(move || {
                        let env = Env {
                            tid,
                            ctaid: block as i64,
                            ntid: BLOCK,
                            shared: shared as *mut u8,
                            block: b,
                        };
                        // SAFETY: each pointer is a live buffer or 0 for a
                        // tensor the op lacks, and the threads of a block
                        // write disjoint values, as on a GPU.
                        unsafe {
                            f(ptrs[0], ptrs[1], ptrs[2], ptrs[3], ptrs[4], args as *const u8, &env)
                        };
                    });
                }
            });
        }
        Ok(())
    }
}
