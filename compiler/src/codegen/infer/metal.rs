//! Everything that talks to Metal, Apple's GPU API: `setup` builds a device,
//! compiles `kernels.metal`, makes a pipeline per kernel and a buffer over each
//! region without copying it; `step` fills the step's inputs, walks the op
//! table and encodes one dispatch per op into one command buffer, then waits
//! for it.
//!
//! Metal is an Objective-C API, so every call is `objc_msgSend` with the
//! selector's own signature; on Apple Silicon a struct of more than 16 bytes,
//! such as `MTLSize`, goes by pointer. Apple publishes no instruction set for
//! its GPUs, so the kernels go into the program as Metal source, and Metal
//! compiles them for the GPU the first time the program runs a prompt.

use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue, PointerValue};

use crate::codegen::libcache::shared::Slot;
use crate::codegen::{Cx, F};
use crate::infer::plan::{Addr, KERNELS, MUL_MM, OP_WORDS, threads};
use crate::mir::Ty;

/// The Metal kernels, compiled when the program first runs a prompt.
const KERNELS_SOURCE: &str = include_str!("../../infer/kernels.metal");

/// The size of the record each kernel takes as buffer 5: five tensors of twelve
/// words, eight floats and eight ints.
const ARGS_BYTES: i64 = 5 * 96 + 32 + 64;

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns a selector, registered once and kept.
    fn sel(&self, f: &F<'_, 'ctx>, name: &str) -> PointerValue<'ctx> {
        let g = self.infer_global(&format!("jade.infer.sel.{name}"), Slot::Ptr);
        let v = f.var(f.load_ptr(g));
        f.if_(f.is_null(f.load_ptr(v)), |f| {
            let s = f.call_ptr(self.objc_fn("sel_registerName"), &[self.cstr(name).into()]);
            f.store(g, s);
            f.store(v, s);
        });
        f.load_ptr(v)
    }

    /// Purpose: This function sends an Objective-C message.
    ///
    /// Inputs:
    /// - `f`: the function being written.
    /// - `ret`: the method's result type, or `None` for void.
    /// - `recv`: the receiver.
    /// - `sel`: the selector's name.
    /// - `args`: the arguments after the receiver and selector.
    ///
    /// Outputs: the result, if the method has one.
    fn send(
        &self,
        f: &F<'_, 'ctx>,
        ret: Option<BasicTypeEnum<'ctx>>,
        recv: PointerValue<'ctx>,
        sel: &str,
        args: &[BasicValueEnum<'ctx>],
    ) -> Option<BasicValueEnum<'ctx>> {
        let s = self.sel(f, sel);
        let mut ps: Vec<BasicMetadataTypeEnum> = vec![self.ptrt().into(), self.ptrt().into()];
        ps.extend(args.iter().map(|a| BasicMetadataTypeEnum::from(a.get_type())));
        let ty = match ret {
            Some(r) => r.fn_type(&ps, false),
            None => self.ctx.void_type().fn_type(&ps, false),
        };
        let mut av: Vec<BasicMetadataValueEnum> = vec![recv.into(), s.into()];
        av.extend(args.iter().map(|a| BasicMetadataValueEnum::from(*a)));
        let target = self.objc_fn("objc_msgSend").as_global_value().as_pointer_value();
        let cs = f.b.build_indirect_call(ty, target, &av, "").expect("positioned");
        match cs.try_as_basic_value() {
            inkwell::values::ValueKind::Basic(v) => Some(v),
            inkwell::values::ValueKind::Instruction(_) => None,
        }
    }

    /// Purpose: This function sends a message whose result is an object.
    fn send_ptr(
        &self,
        f: &F<'_, 'ctx>,
        recv: PointerValue<'ctx>,
        sel: &str,
        args: &[BasicValueEnum<'ctx>],
    ) -> PointerValue<'ctx> {
        self.send(f, Some(self.ptrt().into()), recv, sel, args)
            .expect("an object result")
            .into_pointer_value()
    }

    /// Purpose: This function makes an `NSString` from C text.
    fn nsstring(&self, f: &F<'_, 'ctx>, text: PointerValue<'ctx>) -> PointerValue<'ctx> {
        let class = f.call_ptr(self.objc_fn("objc_getClass"), &[self.cstr("NSString").into()]);
        self.send_ptr(f, class, "stringWithUTF8String:", &[text.into()])
    }

    /// Purpose: This function fails with a message and an `NSError`'s
    /// description.
    fn fail_ns(&self, f: &F<'_, 'ctx>, what: &str, err: PointerValue<'ctx>) {
        let text = f.var(self.cstr("no reason given"));
        f.if_(f.not(f.is_null(err)), |f| {
            let d = self.send_ptr(f, err, "localizedDescription", &[]);
            let c = self.send_ptr(f, d, "UTF8String", &[]);
            f.store(text, c);
        });
        let c = f.load_ptr(text);
        let n = f.call_int(self.libc("strlen"), &[c.into()]);
        let reason = f.call_ptr(self.str_from_fn(), &[c.into(), n.into()]);
        let msg = f.str_cat(&[self.lit(what), reason]);
        f.release(&Ty::Str, reason.into());
        f.fail_str(msg);
    }

    fn infer_obj(&self, name: &str) -> PointerValue<'ctx> {
        self.infer_global(&format!("jade.infer.{name}"), Slot::Ptr)
    }

    fn infer_pipelines(&self) -> PointerValue<'ctx> {
        self.infer_global("jade.infer.pipelines", Slot::Ptrs(KERNELS.len() as u32))
    }

    /// Purpose: This function returns the body of `__infer.setup()`, called
    /// with the lock held inside an autorelease pool: the device, the queue,
    /// the kernels, and a buffer over each region.
    pub(super) fn infer_setup_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.infer.setup.body", None, &[], |f| {
            let cx = f.cx;
            let ptr = cx.ptrt();
            let i64t = cx.i64t();
            f.if_(f.ne(f.load_i64(cx.infer_ready()), f.i64(0)), |f| f.ret_void());
            let dev = f.call_ptr(cx.objc_fn("MTLCreateSystemDefaultDevice"), &[]);
            f.fail_if(f.is_null(dev), "this machine has no Metal device to run the model on");
            let queue = cx.send_ptr(f, dev, "newCommandQueue", &[]);
            let err = f.var(f.null());
            let src = cx.nsstring(f, cx.cstr(KERNELS_SOURCE));
            let lib = cx.send_ptr(
                f,
                dev,
                "newLibraryWithSource:options:error:",
                &[src.into(), f.null().into(), err.into()],
            );
            f.if_(f.is_null(lib), |f| {
                let e = f.load_ptr(err);
                cx.fail_ns(f, "the GPU kernels did not compile: ", e);
            });
            let pipes = cx.infer_pipelines();
            for (k, name) in KERNELS.iter().enumerate() {
                let n = cx.nsstring(f, cx.cstr(name));
                let func = cx.send_ptr(f, lib, "newFunctionWithName:", &[n.into()]);
                f.fail_if(f.is_null(func), "a GPU kernel is missing from the compiled library");
                let p = cx.send_ptr(
                    f,
                    dev,
                    "newComputePipelineStateWithFunction:error:",
                    &[func.into(), err.into()],
                );
                f.if_(f.is_null(p), |f| {
                    let e = f.load_ptr(err);
                    cx.fail_ns(f, "a GPU kernel could not be prepared: ", e);
                });
                f.store(f.elem(ptr, pipes, f.i64(k as i64)), p);
            }
            let n = cx.plan().regions.len() as i64;
            f.for_range(f.i64(0), f.i64(n), |f, r, _| {
                let map = f.load_ptr(f.elem(ptr, cx.infer_regions("map"), r));
                f.fail_if(f.is_null(map), "a region of the model was not mapped before setup");
                let len = f.load_i64(f.elem(i64t, cx.infer_regions("maplen"), r));
                let buf = cx.send_ptr(
                    f,
                    dev,
                    "newBufferWithBytesNoCopy:length:options:deallocator:",
                    &[map.into(), len.into(), f.i64(0).into(), f.null().into()],
                );
                f.fail_if(f.is_null(buf), "Metal would not take the model's memory as a buffer");
                f.store(f.elem(ptr, cx.infer_regions("buf"), r), buf);
            });
            f.store(cx.infer_obj("device"), dev);
            f.store(cx.infer_obj("queue"), queue);
            f.store(cx.infer_ready(), f.i64(1));
            f.ret_void();
        })
    }

    /// Purpose: This function returns the address of a tensor on the host.
    pub(super) fn host_addr(&self, f: &F<'_, 'ctx>, a: Addr) -> PointerValue<'ctx> {
        let base = f.load_ptr(f.elem(self.ptrt(), self.infer_regions("base"), f.i64(a.region)));
        f.at(base, f.i64(a.offset))
    }

    /// Purpose: This function returns the op table, a constant array of
    /// [`OP_WORDS`] words per op.
    fn op_table(&self) -> PointerValue<'ctx> {
        if let Some(g) = self.module.get_global("jade.infer.ops") {
            return g.as_pointer_value();
        }
        let vals: Vec<_> = self
            .plan()
            .ops
            .iter()
            .flat_map(|r| r.iter())
            .map(|w| self.i64t().const_int(*w as u64, true))
            .collect();
        let arr = self.i64t().const_array(&vals);
        let g = self.module.add_global(arr.get_type(), None, "jade.infer.ops");
        g.set_initializer(&arr);
        g.set_constant(true);
        g.set_linkage(Linkage::Private);
        g.as_pointer_value()
    }

    /// Purpose: This function returns `jade.infer.step(tokens, n, pos)`: run
    /// one forward step over `n` tokens, whose ids are `tokens`, an array of
    /// ints, at positions `pos` onward.
    pub(super) fn infer_step_fn(&self) -> FunctionValue<'ctx> {
        let (ptr, i64t) = (self.ptrt(), self.i64t());
        self.support("jade.infer.step", None, &[ptr.into(), i64t.into(), i64t.into()], |f| {
            let cx = f.cx;
            let p = cx.plan().clone();
            let (i32t, i16t) = (cx.i32t(), cx.ctx.i16_type());
            let (tokens, n, pos) = (f.param_ptr(0), f.param_int(1), f.param_int(2));
            // The inputs: ids, positions, the cache cell each token writes, and
            // a mask that lets each token see itself and what came before it.
            let (tp, pp, kp, vp) = (
                cx.host_addr(f, p.tokens),
                cx.host_addr(f, p.positions),
                cx.host_addr(f, p.k_cells),
                cx.host_addr(f, p.v_cells),
            );
            let mp = cx.host_addr(f, p.mask);
            f.for_range(f.i64(0), n, |f, k, _| {
                let id = f.load_i64(f.elem(i64t, tokens, k));
                f.store(f.elem(i32t, tp, k), f.trunc(id, i32t));
                let at = f.add(pos, k);
                f.store(f.elem(i32t, pp, k), f.trunc(at, i32t));
                f.store(f.elem(i64t, kp, k), at);
                f.store(f.elem(i64t, vp, k), at);
                let row = f.at(mp, f.mul(k, f.i64(p.n_ctx * 2)));
                f.for_range(f.i64(0), f.i64(p.n_ctx), |f, j, _| {
                    let seen = f.sle(j, at);
                    // f16 zero, or f16 minus infinity.
                    let bits = f.select_int(seen, i16t.const_zero(), i16t.const_int(0xFC00, false));
                    f.store(f.elem(i16t, row, j), bits);
                });
            });
            let oi = cx.host_addr(f, p.out_ids);
            f.store(oi, f.trunc(f.sub(n, f.i64(1)), i32t));

            // One command buffer: every op, in order.
            let queue = f.load_ptr(cx.infer_obj("queue"));
            let cb = cx.send_ptr(f, queue, "commandBuffer", &[]);
            let enc = cx.send_ptr(f, cb, "computeCommandEncoder", &[]);
            let args = f.alloca(cx.i8t().array_type(ARGS_BYTES as u32));
            let groups = f.alloca(i64t.array_type(3));
            let width = f.alloca(i64t.array_type(3));
            f.store(width, f.i64(64));
            f.store(f.at_c(width, 8), f.i64(1));
            f.store(f.at_c(width, 16), f.i64(1));
            let table = cx.op_table();
            let pipes = cx.infer_pipelines();
            let bufs = cx.infer_regions("buf");
            let deltas = cx.infer_regions("delta");
            f.for_range(f.i64(0), f.i64(p.ops.len() as i64), |f, op, _| {
                let rec = f.elem(i64t, table, f.mul(op, f.i64(OP_WORDS as i64)));
                let word = |f: &F<'_, 'ctx>, i: i64| f.load_i64(f.elem(i64t, rec, f.i64(i)));
                // Each tensor's shape, with the real token count where the plan
                // has the step's.
                for slot in 0..5i64 {
                    let b = 1 + slot * 12;
                    let axis = word(f, b + 11);
                    for d in 0..4i64 {
                        let ne = word(f, b + 2 + d);
                        let ne = f.select_int(f.eq(axis, f.i64(d)), n, ne);
                        f.store(f.at_c(args, slot * 96 + d * 8), ne);
                        f.store(f.at_c(args, slot * 96 + 32 + d * 8), word(f, b + 6 + d));
                    }
                    f.store(f.at_c(args, slot * 96 + 64), word(f, b + 10));
                }
                for k in 0..4i64 {
                    f.store(f.at_c(args, 480 + k * 8), word(f, 61 + k));
                }
                for k in 0..8i64 {
                    f.store(f.at_c(args, 512 + k * 8), word(f, 65 + k));
                }
                // The cache cells that hold tokens once this step writes its own.
                f.store(f.at_c(args, 512 + 6 * 8), f.add(pos, n));
                let ne = |f: &F<'_, 'ctx>, slot: i64, d: i64| {
                    f.load_i64(f.at_c(args, slot * 96 + d * 8))
                };
                let all = |f: &F<'_, 'ctx>, slot: i64| {
                    let a = f.mul(ne(f, slot, 0), ne(f, slot, 1));
                    f.mul(a, f.mul(ne(f, slot, 2), ne(f, slot, 3)))
                };
                let rule = word(f, 73);
                let count = f.var(all(f, 4));
                f.if_(f.eq(rule, f.i64(threads::DST_ROWS)), |f| {
                    f.store(count, f.mul(ne(f, 4, 1), f.mul(ne(f, 4, 2), ne(f, 4, 3))));
                });
                f.if_(f.eq(rule, f.i64(threads::SRC1)), |f| f.store(count, all(f, 1)));
                f.if_(f.eq(rule, f.i64(threads::ATTENTION)), |f| {
                    f.store(count, f.mul(ne(f, 0, 1), ne(f, 0, 2)));
                });
                // A mul_mat over more than one token runs as tiles.
                let tiled = f.and(f.eq(rule, f.i64(threads::MAT)), f.sgt(ne(f, 4, 1), f.i64(1)));
                f.if_(tiled, |f| {
                    let tile = f.i64(threads::TILE);
                    let rows = f.sdiv(f.add(ne(f, 4, 0), f.i64(threads::TILE - 1)), tile);
                    let cols = f.sdiv(f.add(ne(f, 4, 1), f.i64(threads::TILE - 1)), tile);
                    f.store(count, f.mul(f.mul(rows, cols), f.mul(ne(f, 4, 2), ne(f, 4, 3))));
                });
                let count = f.load_i64(count);
                f.store(f.at_c(args, 512 + 7 * 8), count);
                let per_item = f.select_int(tiled, f.i64(threads::TILE_THREADS), word(f, 74));
                let threads = f.mul(count, per_item);

                let kernel = f.select_int(tiled, f.i64(MUL_MM), word(f, 0));
                let pipe = f.load_ptr(f.elem(cx.ptrt(), pipes, kernel));
                cx.send(f, None, enc, "setComputePipelineState:", &[pipe.into()]);
                for slot in 0..5i64 {
                    let b = 1 + slot * 12;
                    let region = word(f, b);
                    f.if_(f.sge(region, f.i64(0)), |f| {
                        let buf = f.load_ptr(f.elem(cx.ptrt(), bufs, region));
                        let delta = f.load_i64(f.elem(i64t, deltas, region));
                        let offset = f.add(delta, word(f, b + 1));
                        cx.send(
                            f,
                            None,
                            enc,
                            "setBuffer:offset:atIndex:",
                            &[buf.into(), offset.into(), f.i64(slot).into()],
                        );
                    });
                }
                cx.send(
                    f,
                    None,
                    enc,
                    "setBytes:length:atIndex:",
                    &[args.into(), f.i64(ARGS_BYTES).into(), f.i64(5).into()],
                );
                f.if_(f.sgt(threads, f.i64(0)), |f| {
                    f.store(groups, f.sdiv(f.add(threads, f.i64(63)), f.i64(64)));
                    f.store(f.at_c(groups, 8), f.i64(1));
                    f.store(f.at_c(groups, 16), f.i64(1));
                    // MTLSize is 24 bytes, so Apple Silicon passes it by pointer.
                    cx.send(
                        f,
                        None,
                        enc,
                        "dispatchThreadgroups:threadsPerThreadgroup:",
                        &[groups.into(), width.into()],
                    );
                });
            });
            cx.send(f, None, enc, "endEncoding", &[]);
            cx.send(f, None, cb, "commit", &[]);
            cx.send(f, None, cb, "waitUntilCompleted", &[]);
            let status = cx
                .send(f, Some(i64t.into()), cb, "status", &[])
                .expect("a status")
                .into_int_value();
            // MTLCommandBufferStatusCompleted.
            f.if_(f.ne(status, f.i64(4)), |f| {
                let e = cx.send_ptr(f, cb, "error", &[]);
                cx.fail_ns(f, "the GPU could not run the model's step: ", e);
            });
            f.ret_void();
        })
    }
}
