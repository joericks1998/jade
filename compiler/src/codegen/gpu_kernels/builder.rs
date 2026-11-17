//! The builder every GPU kernel is written in.
//!
//! A kernel is an LLVM function in a module of its own, built for the GPU
//! rather than for the machine the compiler runs on. `Gpu` is that module and
//! `K` builds one function in it, in the style of `codegen::F`: methods return
//! values, and `if_`, `if_else` and `for_step` take closures. `K` knows nothing
//! of Jade's values, reference counts or errors; a kernel reads and writes the
//! model's memory and nothing else.
//!
//! What differs between GPUs is kept to a few methods: which thread this is,
//! adding a value across the 32 threads of a warp, waiting for the block,
//! memory the block shares, and the fast math functions. Each is a match on
//! [`Vendor`]. The kernels themselves are written once, against these methods.
//!
//! [`Vendor::Host`], in tests only, builds the same kernels for the CPU the
//! tests run on, so `emulate.rs` can run them with one CPU thread per GPU
//! thread and check their math on a machine with no GPU.

use std::cell::Cell;

use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::types::{AnyType, BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValue, BasicValueEnum, FloatValue, FunctionValue, IntValue,
    PointerValue,
};
use inkwell::{AddressSpace, FloatPredicate, IntPredicate};

/// The threads in one block. Every kernel is launched in blocks of this many,
/// as on Metal, and `k_mul_mm` counts on exactly this many.
pub(crate) const BLOCK: i64 = 64;

/// The threads that run in lockstep: a warp on NVIDIA, a SIMD group on Apple.
pub(crate) const WARP: i64 = 32;

/// The most memory one block may share, in bytes: what CUDA allows a kernel
/// to declare statically.
pub(crate) const SHARED_MAX: u32 = 48 * 1024;

/// LLVM's calling convention number for a PTX kernel, `ptx_kernel`.
const PTX_KERNEL: u32 = 71;

/// Purpose: This enum names the GPU a module is built for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Vendor {
    /// NVIDIA, through LLVM's NVPTX backend into PTX.
    Nvidia,
    /// The CPU the tests run on, for `emulate.rs`.
    #[cfg(test)]
    Host,
}

/// Purpose: This struct holds the module the kernels are built in.
pub(crate) struct Gpu<'ctx> {
    pub ctx: &'ctx Context,
    pub module: Module<'ctx>,
    pub vendor: Vendor,
}

/// Purpose: This function unwraps a builder result.
///
/// Inputs:
/// - `r`: the result of a `build_*` call.
///
/// Outputs: the value. The builder errors only when it has no insertion
/// point, which `K` never allows, so an error here is a bug in this module.
fn ok<T, E: std::fmt::Debug>(r: Result<T, E>) -> T {
    r.expect("the kernel builder is always positioned in a block")
}

impl<'ctx> Gpu<'ctx> {
    /// Purpose: This function starts an empty module of kernels.
    ///
    /// Inputs:
    /// - `ctx`: the LLVM context.
    /// - `vendor`: the GPU the kernels are for.
    ///
    /// Outputs: the module.
    pub fn new(ctx: &'ctx Context, vendor: Vendor) -> Self {
        Gpu { ctx, module: ctx.create_module("jade.gpu_kernels"), vendor }
    }

    pub fn ptr(&self) -> inkwell::types::PointerType<'ctx> {
        self.ctx.ptr_type(AddressSpace::default())
    }

    /// Purpose: This function declares a function the kernels call but do not
    /// define: an LLVM intrinsic, or on the host one of `emulate.rs`'s.
    ///
    /// Inputs:
    /// - `name`: its name.
    /// - `ret`: its result type, or `None` for void.
    /// - `params`: its parameter types.
    ///
    /// Outputs: the declaration, made once.
    pub fn declare(
        &self,
        name: &str,
        ret: Option<BasicTypeEnum<'ctx>>,
        params: &[BasicMetadataTypeEnum<'ctx>],
    ) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function(name) {
            return f;
        }
        let ty = match ret {
            Some(r) => r.fn_type(params, false),
            None => self.ctx.void_type().fn_type(params, false),
        };
        self.module.add_function(name, ty, None)
    }

    /// Purpose: This function writes a kernel: a function the CPU launches,
    /// taking the four source tensors and the destination, each a pointer to
    /// the tensor's first byte or null, and the op's argument record.
    ///
    /// Inputs:
    /// - `name`: the kernel's name, one of `crate::infer::plan::KERNELS`.
    /// - `body`: writes the body.
    ///
    /// Outputs: the kernel. On NVIDIA the record is passed by value, as a
    /// kernel parameter of `ARGS_BYTES` bytes; on the host by pointer, with one
    /// more pointer to the thread's place in the grid.
    pub fn kernel(&self, name: &str, body: impl FnOnce(&K<'_, 'ctx>)) -> FunctionValue<'ctx> {
        let ptr = self.ptr();
        // The host's kernels take one more pointer: where the thread is.
        #[cfg(test)]
        let extra = usize::from(self.vendor == Vendor::Host);
        #[cfg(not(test))]
        let extra = 0;
        let params: Vec<BasicMetadataTypeEnum> = vec![ptr.into(); 6 + extra];
        let ty = self.ctx.void_type().fn_type(&params, false);
        let f = self.module.add_function(name, ty, Some(Linkage::External));
        if self.vendor == Vendor::Nvidia {
            f.set_call_conventions(PTX_KERNEL);
            let record = self.ctx.i8_type().array_type(super::ARGS_BYTES as u32);
            let byval = self.ctx.create_type_attribute(
                Attribute::get_named_enum_kind_id("byval"),
                record.as_any_type_enum(),
            );
            f.add_attribute(AttributeLoc::Param(5), byval);
            let align =
                self.ctx.create_enum_attribute(Attribute::get_named_enum_kind_id("align"), 8);
            f.add_attribute(AttributeLoc::Param(5), align);
            // The most threads a block launches, which lets ptxas give each
            // thread more registers.
            let note = self.ctx.metadata_node(&[
                f.as_global_value().as_pointer_value().into(),
                self.ctx.metadata_string("maxntidx").into(),
                self.ctx.i32_type().const_int(BLOCK as u64, false).into(),
            ]);
            self.module
                .add_global_metadata("nvvm.annotations", &note)
                .expect("nvvm.annotations is a valid name");
        }
        let k = K::new(self, f, name);
        body(&k);
        k.ret_void();
        f
    }

    /// Purpose: This function writes a helper the kernels call, once.
    ///
    /// Inputs:
    /// - `name`: its name.
    /// - `ret`: its result type, or `None` for void.
    /// - `params`: its parameter types.
    /// - `inline`: whether every call is replaced by its body.
    /// - `body`: writes the body.
    ///
    /// Outputs: the helper.
    pub fn helper(
        &self,
        name: &str,
        ret: Option<BasicTypeEnum<'ctx>>,
        params: &[BasicMetadataTypeEnum<'ctx>],
        inline: bool,
        body: impl FnOnce(&K<'_, 'ctx>),
    ) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function(name) {
            return f;
        }
        let ty = match ret {
            Some(r) => r.fn_type(params, false),
            None => self.ctx.void_type().fn_type(params, false),
        };
        let f = self.module.add_function(name, ty, Some(Linkage::Internal));
        if inline {
            let a = self
                .ctx
                .create_enum_attribute(Attribute::get_named_enum_kind_id("alwaysinline"), 0);
            f.add_attribute(AttributeLoc::Function, a);
        }
        let k = K::new(self, f, name);
        body(&k);
        f
    }
}

/// One case of [`K::switch`]: a value, and the code for it.
pub(crate) type Case<'b, 'a, 'ctx> = (i64, Box<dyn FnOnce(&K<'a, 'ctx>) + 'b>);

/// Purpose: This function makes one case of [`K::switch`].
///
/// Inputs:
/// - `v`: the value.
/// - `body`: the code for it.
///
/// Outputs: the case.
pub(crate) fn case<'b, 'a, 'ctx>(
    v: i64,
    body: impl FnOnce(&K<'a, 'ctx>) + 'b,
) -> Case<'b, 'a, 'ctx> {
    (v, Box::new(body))
}

/// Purpose: This struct builds the body of one kernel or helper.
pub(crate) struct K<'a, 'ctx> {
    pub g: &'a Gpu<'ctx>,
    pub b: Builder<'ctx>,
    pub func: FunctionValue<'ctx>,
    name: String,
    entry: BasicBlock<'ctx>,
    /// Bytes of block-shared memory this function has asked for so far.
    shared: Cell<u32>,
}

impl<'a, 'ctx> K<'a, 'ctx> {
    fn new(g: &'a Gpu<'ctx>, func: FunctionValue<'ctx>, name: &str) -> Self {
        let b = g.ctx.create_builder();
        let entry = g.ctx.append_basic_block(func, "entry");
        b.position_at_end(entry);
        K { g, b, func, name: name.to_string(), entry, shared: Cell::new(0) }
    }

    // ── constants and parameters ─────────────────────────────────────────

    pub fn i64(&self, v: i64) -> IntValue<'ctx> {
        self.g.ctx.i64_type().const_int(v as u64, true)
    }

    pub fn i32(&self, v: i64) -> IntValue<'ctx> {
        self.g.ctx.i32_type().const_int(v as u64, true)
    }

    pub fn f32(&self, v: f32) -> FloatValue<'ctx> {
        self.g.ctx.f32_type().const_float(f64::from(v))
    }

    pub fn param(&self, i: u32) -> BasicValueEnum<'ctx> {
        self.func.get_nth_param(i).expect("parameter exists")
    }

    pub fn param_ptr(&self, i: u32) -> PointerValue<'ctx> {
        self.param(i).into_pointer_value()
    }

    pub fn param_int(&self, i: u32) -> IntValue<'ctx> {
        self.param(i).into_int_value()
    }

    pub fn param_f32(&self, i: u32) -> FloatValue<'ctx> {
        self.param(i).into_float_value()
    }

    /// Purpose: This function gives a kernel's source tensor `i`, 0 to 3, or
    /// its destination, 4: a pointer to the tensor's first byte.
    pub fn tensor_ptr(&self, i: u32) -> PointerValue<'ctx> {
        self.param_ptr(i)
    }

    /// Purpose: This function gives a kernel's argument record.
    pub fn args_ptr(&self) -> PointerValue<'ctx> {
        self.param_ptr(5)
    }

    // ── integers ─────────────────────────────────────────────────────────

    pub fn add(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_int_add(a, b, ""))
    }

    pub fn sub(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_int_sub(a, b, ""))
    }

    pub fn mul(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_int_mul(a, b, ""))
    }

    /// Purpose: This function divides two ints the kernels know are not
    /// negative, which is cheaper than a signed division.
    pub fn div(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_int_unsigned_div(a, b, ""))
    }

    /// Purpose: This function gives the remainder of two ints the kernels know
    /// are not negative.
    pub fn rem(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_int_unsigned_rem(a, b, ""))
    }

    pub fn and(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_and(a, b, ""))
    }

    pub fn or(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_or(a, b, ""))
    }

    pub fn xor(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_xor(a, b, ""))
    }

    pub fn not(&self, a: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_not(a, ""))
    }

    pub fn shl(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_left_shift(a, b, ""))
    }

    pub fn lshr(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_right_shift(a, b, false, ""))
    }

    pub fn icmp(&self, p: IntPredicate, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_int_compare(p, a, b, ""))
    }

    pub fn eq(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        self.icmp(IntPredicate::EQ, a, b)
    }

    pub fn ne(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        self.icmp(IntPredicate::NE, a, b)
    }

    pub fn slt(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        self.icmp(IntPredicate::SLT, a, b)
    }

    pub fn sge(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        self.icmp(IntPredicate::SGE, a, b)
    }

    pub fn ugt(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        self.icmp(IntPredicate::UGT, a, b)
    }

    pub fn uge(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        self.icmp(IntPredicate::UGE, a, b)
    }

    pub fn ult(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        self.icmp(IntPredicate::ULT, a, b)
    }

    pub fn select_int(
        &self,
        c: IntValue<'ctx>,
        a: IntValue<'ctx>,
        b: IntValue<'ctx>,
    ) -> IntValue<'ctx> {
        ok(self.b.build_select(c, a, b, "")).into_int_value()
    }

    pub fn select_ptr(
        &self,
        c: IntValue<'ctx>,
        a: PointerValue<'ctx>,
        b: PointerValue<'ctx>,
    ) -> PointerValue<'ctx> {
        ok(self.b.build_select(c, a, b, "")).into_pointer_value()
    }

    /// Purpose: This function gives the smaller of two signed ints.
    pub fn min(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> IntValue<'ctx> {
        self.select_int(self.slt(a, b), a, b)
    }

    pub fn zext(&self, v: IntValue<'ctx>, t: inkwell::types::IntType<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_int_z_extend(v, t, ""))
    }

    pub fn sext(&self, v: IntValue<'ctx>, t: inkwell::types::IntType<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_int_s_extend(v, t, ""))
    }

    pub fn trunc(&self, v: IntValue<'ctx>, t: inkwell::types::IntType<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_int_truncate(v, t, ""))
    }

    /// Purpose: This function widens a 32-bit int to 64 bits, keeping its sign.
    pub fn wide(&self, v: IntValue<'ctx>) -> IntValue<'ctx> {
        self.sext(v, self.g.ctx.i64_type())
    }

    // ── floats ───────────────────────────────────────────────────────────

    pub fn fadd(&self, a: FloatValue<'ctx>, b: FloatValue<'ctx>) -> FloatValue<'ctx> {
        ok(self.b.build_float_add(a, b, ""))
    }

    pub fn fsub(&self, a: FloatValue<'ctx>, b: FloatValue<'ctx>) -> FloatValue<'ctx> {
        ok(self.b.build_float_sub(a, b, ""))
    }

    pub fn fmul(&self, a: FloatValue<'ctx>, b: FloatValue<'ctx>) -> FloatValue<'ctx> {
        ok(self.b.build_float_mul(a, b, ""))
    }

    pub fn fdiv(&self, a: FloatValue<'ctx>, b: FloatValue<'ctx>) -> FloatValue<'ctx> {
        ok(self.b.build_float_div(a, b, ""))
    }

    pub fn fneg(&self, a: FloatValue<'ctx>) -> FloatValue<'ctx> {
        ok(self.b.build_float_neg(a, ""))
    }

    pub fn fcmp(
        &self,
        p: FloatPredicate,
        a: FloatValue<'ctx>,
        b: FloatValue<'ctx>,
    ) -> IntValue<'ctx> {
        ok(self.b.build_float_compare(p, a, b, ""))
    }

    pub fn select_f(
        &self,
        c: IntValue<'ctx>,
        a: FloatValue<'ctx>,
        b: FloatValue<'ctx>,
    ) -> FloatValue<'ctx> {
        ok(self.b.build_select(c, a, b, "")).into_float_value()
    }

    /// Purpose: This function turns a signed int, of any width, into an f32.
    pub fn itof(&self, v: IntValue<'ctx>) -> FloatValue<'ctx> {
        ok(self.b.build_signed_int_to_float(v, self.g.ctx.f32_type(), ""))
    }

    pub fn bits_to_f32(&self, v: IntValue<'ctx>) -> FloatValue<'ctx> {
        ok(self.b.build_bit_cast(v, self.g.ctx.f32_type(), "")).into_float_value()
    }

    pub fn f32_to_bits(&self, v: FloatValue<'ctx>) -> IntValue<'ctx> {
        ok(self.b.build_bit_cast(v, self.g.ctx.i32_type(), "")).into_int_value()
    }

    /// Purpose: This function calls a one-argument float intrinsic.
    fn unary(&self, name: &str, x: FloatValue<'ctx>) -> FloatValue<'ctx> {
        let t = x.get_type();
        let f = self.g.declare(name, Some(t.into()), &[t.into()]);
        self.call(f, &[x.into()]).expect("a float result").into_float_value()
    }

    /// Purpose: This function gives 2 to the power `x`, to about 22 bits on
    /// NVIDIA, where it is one instruction.
    pub fn exp2(&self, x: FloatValue<'ctx>) -> FloatValue<'ctx> {
        match self.g.vendor {
            Vendor::Nvidia => self.unary("llvm.nvvm.ex2.approx.f", x),
            #[cfg(test)]
            Vendor::Host => self.unary("llvm.exp2.f32", x),
        }
    }

    /// Purpose: This function gives e to the power `x`.
    pub fn exp(&self, x: FloatValue<'ctx>) -> FloatValue<'ctx> {
        self.exp2(self.fmul(x, self.f32(std::f32::consts::LOG2_E)))
    }

    /// Purpose: This function gives the square root, rounded correctly.
    pub fn sqrt(&self, x: FloatValue<'ctx>) -> FloatValue<'ctx> {
        self.unary("llvm.sqrt.f32", x)
    }

    /// Purpose: This function gives the larger of two floats.
    pub fn fmax(&self, a: FloatValue<'ctx>, b: FloatValue<'ctx>) -> FloatValue<'ctx> {
        let t = self.g.ctx.f32_type();
        let f = self.g.declare("llvm.maxnum.f32", Some(t.into()), &[t.into(), t.into()]);
        self.call(f, &[a.into(), b.into()]).expect("a float result").into_float_value()
    }

    /// Purpose: This function gives the sine and cosine of an angle of any
    /// size.
    ///
    /// Inputs:
    /// - `theta`: the angle in radians.
    ///
    /// Outputs: `(sin, cos)`. NVIDIA's fast sine and cosine are accurate only
    /// near zero, and rope's angles reach thousands of radians, so the angle
    /// is first brought into -pi to pi in f64.
    pub fn sin_cos(&self, theta: FloatValue<'ctx>) -> (FloatValue<'ctx>, FloatValue<'ctx>) {
        let f64t = self.g.ctx.f64_type();
        let t = ok(self.b.build_float_ext(theta, f64t, ""));
        let turns =
            ok(self.b.build_float_mul(t, f64t.const_float(1.0 / std::f64::consts::TAU), ""));
        let rint = self.g.declare("llvm.rint.f64", Some(f64t.into()), &[f64t.into()]);
        let whole = self.call(rint, &[turns.into()]).expect("a float result").into_float_value();
        let back = ok(self.b.build_float_mul(whole, f64t.const_float(std::f64::consts::TAU), ""));
        let r = ok(self.b.build_float_sub(t, back, ""));
        let r = ok(self.b.build_float_trunc(r, self.g.ctx.f32_type(), ""));
        match self.g.vendor {
            Vendor::Nvidia => {
                (self.unary("llvm.nvvm.sin.approx.f", r), self.unary("llvm.nvvm.cos.approx.f", r))
            }
            #[cfg(test)]
            Vendor::Host => (self.unary("llvm.sin.f32", r), self.unary("llvm.cos.f32", r)),
        }
    }

    /// Purpose: This function widens an IEEE half, given as its 16 bits, to
    /// an f32.
    pub fn half_to_f32(&self, bits: IntValue<'ctx>) -> FloatValue<'ctx> {
        match self.g.vendor {
            Vendor::Nvidia => {
                let h = ok(self.b.build_bit_cast(bits, self.g.ctx.f16_type(), ""));
                ok(self.b.build_float_ext(h.into_float_value(), self.g.ctx.f32_type(), ""))
            }
            #[cfg(test)]
            Vendor::Host => self.half_to_f32_bits(bits),
        }
    }

    /// Purpose: This function rounds an f32 to the nearest IEEE half, ties to
    /// even, and gives its 16 bits.
    pub fn f32_to_half(&self, v: FloatValue<'ctx>) -> IntValue<'ctx> {
        match self.g.vendor {
            Vendor::Nvidia => {
                let h = ok(self.b.build_float_trunc(v, self.g.ctx.f16_type(), ""));
                ok(self.b.build_bit_cast(h, self.g.ctx.i16_type(), "")).into_int_value()
            }
            #[cfg(test)]
            Vendor::Host => self.f32_to_half_bits(v),
        }
    }

    /// Purpose: This function widens a half to an f32 with integer
    /// operations only, so the host needs no conversion routine from a
    /// library. Fabian Giesen's `half_to_float`, from his public domain
    /// conversions.
    #[cfg(test)]
    fn half_to_f32_bits(&self, bits: IntValue<'ctx>) -> FloatValue<'ctx> {
        let i32t = self.g.ctx.i32_type();
        let h = self.zext(bits, i32t);
        let o = self.shl(self.and(h, self.i32(0x7fff)), self.i32(13));
        let exp = self.and(o, self.i32(0x7c00 << 13));
        let o = self.add(o, self.i32((127 - 15) << 23));
        let inf_nan = self.add(o, self.i32((128 - 16) << 23));
        let small = self.bits_to_f32(self.add(o, self.i32(1 << 23)));
        let small = self.f32_to_bits(self.fsub(small, self.bits_to_f32(self.i32(113 << 23))));
        let o = self.select_int(self.eq(exp, self.i32(0)), small, o);
        let o = self.select_int(self.eq(exp, self.i32(0x7c00 << 13)), inf_nan, o);
        let sign = self.shl(self.and(h, self.i32(0x8000)), self.i32(16));
        self.bits_to_f32(self.or(o, sign))
    }

    /// Purpose: This function rounds an f32 to a half with integer and f32
    /// operations only, ties to even. Fabian Giesen's
    /// `float_to_half_fast3_rtne`.
    #[cfg(test)]
    fn f32_to_half_bits(&self, v: FloatValue<'ctx>) -> IntValue<'ctx> {
        let u = self.f32_to_bits(v);
        let sign = self.and(u, self.i32(0x8000_0000u32 as i64));
        let u = self.xor(u, sign);
        let f16max = self.i32((127 + 16) << 23);
        let infty = self.i32(255 << 23);
        let magic = self.i32(((127 - 15) + (23 - 10) + 1) << 23);
        let big = self.select_int(self.ugt(u, infty), self.i32(0x7e00), self.i32(0x7c00));
        let tiny = self.fadd(self.bits_to_f32(u), self.bits_to_f32(magic));
        let tiny = self.sub(self.f32_to_bits(tiny), magic);
        let odd = self.and(self.lshr(u, self.i32(13)), self.i32(1));
        let normal = self.add(u, self.i32((((15 - 127) << 23) as i64) + 0xfff));
        let normal = self.lshr(self.add(normal, odd), self.i32(13));
        let o = self.select_int(self.ult(u, self.i32(113 << 23)), tiny, normal);
        let o = self.select_int(self.uge(u, f16max), big, o);
        let o = self.or(o, self.lshr(sign, self.i32(16)));
        self.trunc(o, self.g.ctx.i16_type())
    }

    // ── memory ───────────────────────────────────────────────────────────

    /// Purpose: This function makes a local slot in the entry block, where
    /// LLVM turns it into a register.
    pub fn alloca<T: BasicType<'ctx>>(&self, t: T) -> PointerValue<'ctx> {
        let tmp = self.g.ctx.create_builder();
        match self.entry.get_first_instruction() {
            Some(i) => tmp.position_before(&i),
            None => tmp.position_at_end(self.entry),
        }
        ok(tmp.build_alloca(t, ""))
    }

    /// Purpose: This function makes a local variable holding a starting value.
    pub fn var<V: BasicValue<'ctx>>(&self, v: V) -> PointerValue<'ctx> {
        let v = v.as_basic_value_enum();
        let p = self.alloca(v.get_type());
        self.store(p, v);
        p
    }

    pub fn load<T: BasicType<'ctx>>(&self, t: T, p: PointerValue<'ctx>) -> BasicValueEnum<'ctx> {
        ok(self.b.build_load(t, p, ""))
    }

    pub fn store<V: BasicValue<'ctx>>(&self, p: PointerValue<'ctx>, v: V) {
        ok(self.b.build_store(p, v));
    }

    pub fn get_f32(&self, p: PointerValue<'ctx>) -> FloatValue<'ctx> {
        self.load(self.g.ctx.f32_type(), p).into_float_value()
    }

    pub fn get_i64(&self, p: PointerValue<'ctx>) -> IntValue<'ctx> {
        self.load(self.g.ctx.i64_type(), p).into_int_value()
    }

    /// Purpose: This function reads a byte as an unsigned 32-bit int.
    pub fn u8_at(&self, p: PointerValue<'ctx>) -> IntValue<'ctx> {
        let v = self.load(self.g.ctx.i8_type(), p).into_int_value();
        self.zext(v, self.g.ctx.i32_type())
    }

    /// Purpose: This function reads a byte as a signed 32-bit int.
    pub fn i8_at(&self, p: PointerValue<'ctx>) -> IntValue<'ctx> {
        let v = self.load(self.g.ctx.i8_type(), p).into_int_value();
        self.sext(v, self.g.ctx.i32_type())
    }

    /// Purpose: This function reads 16 bits, as they are.
    pub fn u16_at(&self, p: PointerValue<'ctx>) -> IntValue<'ctx> {
        self.load(self.g.ctx.i16_type(), p).into_int_value()
    }

    pub fn i32_at(&self, p: PointerValue<'ctx>) -> IntValue<'ctx> {
        self.load(self.g.ctx.i32_type(), p).into_int_value()
    }

    pub fn i64_at(&self, p: PointerValue<'ctx>) -> IntValue<'ctx> {
        self.get_i64(p)
    }

    pub fn f32_at(&self, p: PointerValue<'ctx>) -> FloatValue<'ctx> {
        self.get_f32(p)
    }

    /// Purpose: This function offsets a pointer by a number of bytes.
    pub fn at(&self, p: PointerValue<'ctx>, off: IntValue<'ctx>) -> PointerValue<'ctx> {
        // SAFETY: a kernel offsets only within the tensors the plan placed,
        // and within its own block-shared arrays.
        ok(unsafe { self.b.build_gep(self.g.ctx.i8_type(), p, &[off], "") })
    }

    /// Purpose: This function offsets a pointer by a constant number of bytes.
    pub fn at_c(&self, p: PointerValue<'ctx>, off: i64) -> PointerValue<'ctx> {
        self.at(p, self.i64(off))
    }

    // ── calls ────────────────────────────────────────────────────────────

    pub fn call(
        &self,
        f: FunctionValue<'ctx>,
        args: &[BasicValueEnum<'ctx>],
    ) -> Option<BasicValueEnum<'ctx>> {
        let args: Vec<BasicMetadataValueEnum> = args.iter().map(|a| (*a).into()).collect();
        let cs = ok(self.b.build_call(f, &args, ""));
        match cs.try_as_basic_value() {
            inkwell::values::ValueKind::Basic(v) => Some(v),
            inkwell::values::ValueKind::Instruction(_) => None,
        }
    }

    // ── control flow ─────────────────────────────────────────────────────

    pub fn block(&self) -> BasicBlock<'ctx> {
        self.g.ctx.append_basic_block(self.func, "")
    }

    pub fn position(&self, bb: BasicBlock<'ctx>) {
        self.b.position_at_end(bb);
    }

    pub fn terminated(&self) -> bool {
        self.b.get_insert_block().and_then(|b| b.get_terminator()).is_some()
    }

    pub fn br(&self, bb: BasicBlock<'ctx>) {
        if !self.terminated() {
            ok(self.b.build_unconditional_branch(bb));
        }
    }

    pub fn cond_br(&self, c: IntValue<'ctx>, a: BasicBlock<'ctx>, b: BasicBlock<'ctx>) {
        if !self.terminated() {
            ok(self.b.build_conditional_branch(c, a, b));
        }
    }

    pub fn ret_void(&self) {
        if !self.terminated() {
            ok(self.b.build_return(None));
        }
    }

    pub fn ret<V: BasicValue<'ctx>>(&self, v: V) {
        if !self.terminated() {
            ok(self.b.build_return(Some(&v)));
        }
    }

    /// Purpose: This function ends the thread's work when a condition holds,
    /// and carries on building after it otherwise.
    pub fn exit_if(&self, c: IntValue<'ctx>) {
        let leave = self.block();
        let stay = self.block();
        self.cond_br(c, leave, stay);
        self.position(leave);
        self.ret_void();
        self.position(stay);
    }

    /// Purpose: This function runs code only when a condition holds.
    pub fn if_(&self, c: IntValue<'ctx>, then: impl FnOnce(&Self)) {
        let t = self.block();
        let end = self.block();
        self.cond_br(c, t, end);
        self.position(t);
        then(self);
        self.br(end);
        self.position(end);
    }

    /// Purpose: This function runs one of two pieces of code.
    pub fn if_else(&self, c: IntValue<'ctx>, then: impl FnOnce(&Self), els: impl FnOnce(&Self)) {
        let t = self.block();
        let e = self.block();
        let end = self.block();
        self.cond_br(c, t, e);
        self.position(t);
        then(self);
        self.br(end);
        self.position(e);
        els(self);
        self.br(end);
        self.position(end);
    }

    /// Purpose: This function runs code for one of several constant values of
    /// an int, or a default.
    ///
    /// Inputs:
    /// - `v`: the int.
    /// - `cases`: the values, each with the code for it, made with [`case`].
    /// - `default`: the code for any other value.
    pub fn switch(
        &self,
        v: IntValue<'ctx>,
        cases: Vec<Case<'_, 'a, 'ctx>>,
        default: impl FnOnce(&Self),
    ) {
        let end = self.block();
        let other = self.block();
        let mut targets = Vec::new();
        let mut bodies = Vec::new();
        for (c, body) in cases {
            let bb = self.block();
            targets.push((v.get_type().const_int(c as u64, true), bb));
            bodies.push((bb, body));
        }
        if !self.terminated() {
            ok(self.b.build_switch(v, other, &targets));
        }
        for (bb, body) in bodies {
            self.position(bb);
            body(self);
            self.br(end);
        }
        self.position(other);
        default(self);
        self.br(end);
        self.position(end);
    }

    /// Purpose: This function runs code for `start`, `start + step` and so on
    /// while below `end`, all 64-bit.
    pub fn for_step(
        &self,
        start: IntValue<'ctx>,
        end: IntValue<'ctx>,
        step: IntValue<'ctx>,
        body: impl FnOnce(&Self, IntValue<'ctx>),
    ) {
        let i = self.var(start);
        let head = self.block();
        let bb = self.block();
        let done = self.block();
        self.br(head);
        self.position(head);
        let cur = self.get_i64(i);
        self.cond_br(self.slt(cur, end), bb, done);
        self.position(bb);
        body(self, cur);
        let cur = self.get_i64(i);
        self.store(i, self.add(cur, step));
        self.br(head);
        self.position(done);
    }

    // ── the GPU ──────────────────────────────────────────────────────────

    /// Purpose: This function reads one of NVIDIA's special registers, such
    /// as the thread's index in its block, as a 64-bit int.
    fn sreg(&self, name: &str) -> IntValue<'ctx> {
        let i32t = self.g.ctx.i32_type();
        let f = self.g.declare(name, Some(i32t.into()), &[]);
        let v = self.call(f, &[]).expect("an int result").into_int_value();
        self.zext(v, self.g.ctx.i64_type())
    }

    /// Purpose: This function reads word `i` of the host's record of where
    /// this thread is: its index, its block's and the block's size.
    #[cfg(test)]
    fn env_word(&self, i: i64) -> IntValue<'ctx> {
        self.get_i64(self.at_c(self.param_ptr(6), i * 8))
    }

    /// Purpose: This function gives the thread's index in its block.
    pub fn thread_x(&self) -> IntValue<'ctx> {
        match self.g.vendor {
            Vendor::Nvidia => self.sreg("llvm.nvvm.read.ptx.sreg.tid.x"),
            #[cfg(test)]
            Vendor::Host => self.env_word(0),
        }
    }

    /// Purpose: This function gives the block's index in the grid.
    pub fn block_x(&self) -> IntValue<'ctx> {
        match self.g.vendor {
            Vendor::Nvidia => self.sreg("llvm.nvvm.read.ptx.sreg.ctaid.x"),
            #[cfg(test)]
            Vendor::Host => self.env_word(1),
        }
    }

    /// Purpose: This function gives the threads in a block.
    pub fn block_dim(&self) -> IntValue<'ctx> {
        match self.g.vendor {
            Vendor::Nvidia => self.sreg("llvm.nvvm.read.ptx.sreg.ntid.x"),
            #[cfg(test)]
            Vendor::Host => self.env_word(2),
        }
    }

    /// Purpose: This function gives the thread's index in the whole grid.
    pub fn gid(&self) -> IntValue<'ctx> {
        self.add(self.mul(self.block_x(), self.block_dim()), self.thread_x())
    }

    /// Purpose: This function gives the thread's index in its warp.
    pub fn lane(&self) -> IntValue<'ctx> {
        self.and(self.thread_x(), self.i64(WARP - 1))
    }

    /// Purpose: This function adds a value across the 32 threads of a warp.
    ///
    /// Inputs:
    /// - `v`: this thread's value. Every thread of the warp must call it.
    ///
    /// Outputs: the sum, the same in every thread.
    pub fn warp_sum(&self, v: FloatValue<'ctx>) -> FloatValue<'ctx> {
        match self.g.vendor {
            Vendor::Nvidia => {
                let (i32t, f32t) = (self.g.ctx.i32_type(), self.g.ctx.f32_type());
                let f = self.g.declare(
                    "llvm.nvvm.shfl.sync.bfly.f32",
                    Some(f32t.into()),
                    &[i32t.into(), f32t.into(), i32t.into(), i32t.into()],
                );
                let mut sum = v;
                for lanes in [16, 8, 4, 2, 1] {
                    let other = self
                        .call(
                            f,
                            &[
                                self.i32(-1).into(),
                                sum.into(),
                                self.i32(lanes).into(),
                                self.i32(31).into(),
                            ],
                        )
                        .expect("a float result")
                        .into_float_value();
                    sum = self.fadd(sum, other);
                }
                sum
            }
            #[cfg(test)]
            Vendor::Host => {
                let (ptr, f32t) = (self.g.ptr(), self.g.ctx.f32_type());
                let f = self.g.declare(
                    "jade_gpu_warp_sum",
                    Some(f32t.into()),
                    &[ptr.into(), f32t.into()],
                );
                let env = self.param_ptr(6);
                self.call(f, &[env.into(), v.into()]).expect("a float result").into_float_value()
            }
        }
    }

    /// Purpose: This function waits until every thread of the block reaches
    /// this point, and sees what each wrote to shared memory before it.
    pub fn barrier(&self) {
        match self.g.vendor {
            Vendor::Nvidia => {
                let f = self.g.declare("llvm.nvvm.barrier0", None, &[]);
                self.call(f, &[]);
            }
            #[cfg(test)]
            Vendor::Host => {
                let f = self.g.declare("jade_gpu_barrier", None, &[self.g.ptr().into()]);
                let env = self.param_ptr(6);
                self.call(f, &[env.into()]);
            }
        }
    }

    /// Purpose: This function gives an array of floats the threads of a block
    /// share.
    ///
    /// Inputs:
    /// - `floats`: its length.
    ///
    /// Outputs: a pointer to its first float.
    pub fn shared(&self, floats: u32) -> PointerValue<'ctx> {
        let start = self.shared.get();
        let bytes = floats * 4;
        assert!(
            start + bytes <= SHARED_MAX,
            "{} asks for more shared memory than a block has",
            self.name
        );
        self.shared.set(start + bytes);
        match self.g.vendor {
            Vendor::Nvidia => {
                let f32t = self.g.ctx.f32_type();
                let ty = f32t.array_type(floats);
                let g = self.g.module.add_global(
                    ty,
                    Some(AddressSpace::from(3u16)),
                    &format!("{}.shared.{start}", self.name),
                );
                g.set_initializer(&ty.get_undef());
                g.set_linkage(Linkage::Internal);
                g.set_alignment(16);
                ok(self.b.build_address_space_cast(g.as_pointer_value(), self.g.ptr(), ""))
            }
            #[cfg(test)]
            Vendor::Host => {
                let base =
                    self.load(self.g.ptr(), self.at_c(self.param_ptr(6), 24)).into_pointer_value();
                self.at_c(base, i64::from(start))
            }
        }
    }
}
