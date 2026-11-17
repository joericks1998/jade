//! Reading and writing the values of a model's tensors, of every type the
//! kernels take.
//!
//! A kernel finds each tensor's shape in its argument record, `Args` in
//! `apple/kernels.metal` and the same bytes here: four sources then the
//! destination, 96 bytes each, `ne` four times, `nb` four times and the ggml
//! type; then eight f32 parameters and eight i64 parameters. Shapes follow
//! ggml: `ne` counts elements per dimension, fastest first, and `nb` is each
//! dimension's stride in bytes. A quantized tensor's `nb[0]` is its block's
//! size, and its rows are whole blocks.
//!
//! The block layouts are ggml's, from `ggml-common.h`, the same ones
//! `kernels.metal` decodes:
//!
//! - q8_0: an f16 scale `d`, then 32 signed bytes; value `j` is `d * q[j]`.
//! - q4_0: `d`, then 16 bytes; value `j` is `d * ((q[j] & 15) - 8)` and value
//!   `j + 16` is `d * ((q[j] >> 4) - 8)`.
//! - q4_K: `d` and `dmin`, 12 bytes of 6-bit scales and mins, 128 bytes of
//!   4-bit values; each 32 values share a scale and a min.
//! - q5_K: as q4_K, with 32 bytes of fifth bits before the 4-bit values.
//! - q6_K: 128 bytes of low 4 bits, 64 of high 2 bits, 16 signed scales, `d`.

use inkwell::values::{FloatValue, IntValue, PointerValue};

use super::builder::{Gpu, K, case};

/// ggml's element types, by the numbers GGUF files use.
pub(crate) mod ty {
    pub const F32: i64 = 0;
    pub const F16: i64 = 1;
    pub const Q4_0: i64 = 2;
    pub const Q8_0: i64 = 8;
    pub const Q4_K: i64 = 12;
    pub const Q5_K: i64 = 13;
    pub const Q6_K: i64 = 14;
    pub const I32: i64 = 26;
    pub const I64: i64 = 27;
    pub const BF16: i64 = 30;
}

/// The argument record's slot for the destination; sources are 0 to 3.
pub(crate) const DST: u32 = 4;

/// Purpose: This enum names a weight type, so code for it can be written
/// knowing it, rather than checking it value by value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Wt {
    F32,
    F16,
    Bf16,
    Q8_0,
    Q4_0,
    Q4K,
    Q5K,
    Q6K,
}

impl Wt {
    /// Every weight type a matrix multiplication reads.
    pub const ALL: [Wt; 8] =
        [Wt::F32, Wt::F16, Wt::Bf16, Wt::Q8_0, Wt::Q4_0, Wt::Q4K, Wt::Q5K, Wt::Q6K];

    /// Purpose: This function gives the type's ggml number.
    pub fn code(self) -> i64 {
        match self {
            Wt::F32 => ty::F32,
            Wt::F16 => ty::F16,
            Wt::Bf16 => ty::BF16,
            Wt::Q8_0 => ty::Q8_0,
            Wt::Q4_0 => ty::Q4_0,
            Wt::Q4K => ty::Q4_K,
            Wt::Q5K => ty::Q5_K,
            Wt::Q6K => ty::Q6_K,
        }
    }

    /// Purpose: This function says whether the type is one float per value,
    /// rather than blocks.
    pub fn is_float(self) -> bool {
        matches!(self, Wt::F32 | Wt::F16 | Wt::Bf16)
    }
}

/// Purpose: This struct holds one tensor's shape and type, read from the
/// argument record.
#[derive(Clone, Copy)]
pub(crate) struct Tensor<'ctx> {
    pub ne: [IntValue<'ctx>; 4],
    pub nb: [IntValue<'ctx>; 4],
    pub ty: IntValue<'ctx>,
}

impl<'ctx> K<'_, 'ctx> {
    // ── the argument record ──────────────────────────────────────────────

    /// Purpose: This function reads a tensor's shape and type.
    ///
    /// Inputs:
    /// - `slot`: 0 to 3 for a source, [`DST`] for the destination.
    pub fn tensor(&self, slot: u32) -> Tensor<'ctx> {
        let a = self.args_ptr();
        let base = i64::from(slot) * 96;
        let word = |off: i64| self.i64_at(self.at_c(a, base + off));
        Tensor {
            ne: [word(0), word(8), word(16), word(24)],
            nb: [word(32), word(40), word(48), word(56)],
            ty: word(64),
        }
    }

    /// Purpose: This function reads float parameter `i` of the op.
    pub fn fparam(&self, i: i64) -> FloatValue<'ctx> {
        self.f32_at(self.at_c(self.args_ptr(), 480 + i * 4))
    }

    /// Purpose: This function reads integer parameter `i` of the op. The
    /// step sets the last two: 6 is the cache cells that hold tokens, and 7
    /// how many outputs the op computes.
    pub fn iparam(&self, i: i64) -> IntValue<'ctx> {
        self.i64_at(self.at_c(self.args_ptr(), 512 + i * 8))
    }

    /// Purpose: This function splits a flat index into four dimensions.
    ///
    /// Inputs:
    /// - `g`: the index, not negative.
    /// - `ne`: the dimensions' sizes.
    ///
    /// Outputs: the index in each dimension, fastest first.
    pub fn unflatten(&self, g: IntValue<'ctx>, ne: &[IntValue<'ctx>; 4]) -> [IntValue<'ctx>; 4] {
        let i0 = self.rem(g, ne[0]);
        let g = self.div(g, ne[0]);
        let i1 = self.rem(g, ne[1]);
        let g = self.div(g, ne[1]);
        let i2 = self.rem(g, ne[2]);
        let i3 = self.div(g, ne[2]);
        [i0, i1, i2, i3]
    }

    // ── one value ────────────────────────────────────────────────────────

    /// Purpose: This function reads an f16 as an f32.
    pub fn half(&self, p: PointerValue<'ctx>) -> FloatValue<'ctx> {
        self.half_to_f32(self.u16_at(p))
    }

    /// Purpose: This function reads a bf16 as an f32: its bits are the top
    /// half of the f32's.
    pub fn bf16(&self, p: PointerValue<'ctx>) -> FloatValue<'ctx> {
        let bits = self.zext(self.u16_at(p), self.g.ctx.i32_type());
        self.bits_to_f32(self.shl(bits, self.i32(16)))
    }

    /// Purpose: This function reads one f32, f16 or bf16 value, the type
    /// known when the kernel is written.
    pub fn float_of(&self, p: PointerValue<'ctx>, w: Wt) -> FloatValue<'ctx> {
        match w {
            Wt::F16 => self.half(p),
            Wt::Bf16 => self.bf16(p),
            _ => self.f32_at(p),
        }
    }

    /// Purpose: This function reads value `k` of an f32, f16 or bf16 row.
    ///
    /// Inputs:
    /// - `row`: the row's first byte.
    /// - `nb0`: the stride between values, in bytes.
    /// - `t`: the element type; anything else reads as f32.
    /// - `k`: the value's index in the row.
    pub fn float_at(
        &self,
        row: PointerValue<'ctx>,
        nb0: IntValue<'ctx>,
        t: IntValue<'ctx>,
        k: IntValue<'ctx>,
    ) -> FloatValue<'ctx> {
        let f = float_fn(self.g);
        self.call(f, &[row.into(), nb0.into(), t.into(), k.into()])
            .expect("a float result")
            .into_float_value()
    }

    /// Purpose: This function reads value `k` of a row of any type the
    /// kernels take: f32, f16, bf16, i32 or a block type.
    pub fn value_at(
        &self,
        row: PointerValue<'ctx>,
        nb0: IntValue<'ctx>,
        t: IntValue<'ctx>,
        k: IntValue<'ctx>,
    ) -> FloatValue<'ctx> {
        let f = value_fn(self.g);
        self.call(f, &[row.into(), nb0.into(), t.into(), k.into()])
            .expect("a float result")
            .into_float_value()
    }

    /// Purpose: This function reads one element of a tensor as a float.
    ///
    /// Inputs:
    /// - `base`: the tensor's first byte.
    /// - `t`: its shape and type.
    /// - `i`: the element's index in each dimension.
    pub fn read(
        &self,
        base: PointerValue<'ctx>,
        t: &Tensor<'ctx>,
        i: [IntValue<'ctx>; 4],
    ) -> FloatValue<'ctx> {
        let off = self.add(
            self.mul(i[1], t.nb[1]),
            self.add(self.mul(i[2], t.nb[2]), self.mul(i[3], t.nb[3])),
        );
        self.value_at(self.at(base, off), t.nb[0], t.ty, i[0])
    }

    /// Purpose: This function writes one float into an element of an f32,
    /// f16 or bf16 tensor.
    pub fn write(
        &self,
        base: PointerValue<'ctx>,
        t: &Tensor<'ctx>,
        i: [IntValue<'ctx>; 4],
        v: FloatValue<'ctx>,
    ) {
        let off = self.add(
            self.add(self.mul(i[0], t.nb[0]), self.mul(i[1], t.nb[1])),
            self.add(self.mul(i[2], t.nb[2]), self.mul(i[3], t.nb[3])),
        );
        let f = store_fn(self.g);
        self.call(f, &[self.at(base, off).into(), t.ty.into(), v.into()]);
    }

    /// Purpose: This function reads an integer index, i32 or i64.
    ///
    /// Inputs:
    /// - `base`: the index tensor's first byte.
    /// - `t`: its shape and type.
    /// - `i0`, `i1`: the index's place in its first two dimensions.
    pub fn index_at(
        &self,
        base: PointerValue<'ctx>,
        t: &Tensor<'ctx>,
        i0: IntValue<'ctx>,
        i1: IntValue<'ctx>,
    ) -> IntValue<'ctx> {
        let p = self.at(base, self.add(self.mul(i0, t.nb[0]), self.mul(i1, t.nb[1])));
        let v = self.var(self.i64(0));
        self.if_else(
            self.eq(t.ty, self.i64(ty::I64)),
            |k| k.store(v, k.i64_at(p)),
            |k| k.store(v, k.wide(k.i32_at(p))),
        );
        self.get_i64(v)
    }

    // ── 32 values at a time ──────────────────────────────────────────────

    /// Purpose: This function reads 32 values of an f32, f16 or bf16 row,
    /// the type known.
    ///
    /// Inputs:
    /// - `row`: the row's first byte.
    /// - `nb0`: the stride between its values, in bytes.
    /// - `w`: its type.
    /// - `k0`: the first of the 32.
    /// - `len`: how many of the 32 the row has, or `None` for all of them;
    ///   the rest read as zero, and nothing past the row is read.
    pub fn floats32(
        &self,
        row: PointerValue<'ctx>,
        nb0: IntValue<'ctx>,
        w: Wt,
        k0: IntValue<'ctx>,
        len: Option<IntValue<'ctx>>,
    ) -> Vec<FloatValue<'ctx>> {
        (0..32)
            .map(|l| {
                let at = match len {
                    None => self.add(k0, self.i64(l)),
                    Some(n) => self.add(k0, self.min(self.i64(l), self.sub(n, self.i64(1)))),
                };
                let v = self.float_of(self.at(row, self.mul(at, nb0)), w);
                match len {
                    None => v,
                    Some(n) => self.select_f(self.slt(self.i64(l), n), v, self.f32(0.0)),
                }
            })
            .collect()
    }

    /// Purpose: This function reads 32 values of an f32, f16 or bf16 row whose
    /// type is known only when the kernel runs.
    ///
    /// Inputs: as [`K::floats32`], with `t` the type's ggml number; anything
    /// but f16 and bf16 reads as f32.
    pub fn floats32_any(
        &self,
        row: PointerValue<'ctx>,
        nb0: IntValue<'ctx>,
        t: IntValue<'ctx>,
        k0: IntValue<'ctx>,
        len: Option<IntValue<'ctx>>,
    ) -> Vec<FloatValue<'ctx>> {
        let slots: Vec<PointerValue<'ctx>> = (0..32).map(|_| self.var(self.f32(0.0))).collect();
        let put = |k: &Self, w: Wt| {
            for (slot, v) in slots.iter().zip(k.floats32(row, nb0, w, k0, len)) {
                k.store(*slot, v);
            }
        };
        self.switch(
            t,
            vec![case(ty::F16, |k| put(k, Wt::F16)), case(ty::BF16, |k| put(k, Wt::Bf16))],
            |k| put(k, Wt::F32),
        );
        slots.iter().map(|s| self.get_f32(*s)).collect()
    }

    /// Purpose: This function reads the 6-bit scale and min that 32 values of
    /// a q4_K or q5_K block share: ggml's `get_scale_min_k4`.
    ///
    /// Inputs:
    /// - `sc`: the block's 12 bytes of scales, 4 bytes into the block.
    /// - `is`: which 32 values, 0 to 7, as a 64-bit int.
    ///
    /// Outputs: `(scale, min)` as 32-bit ints. Both forms read only bytes of
    /// the block, so both are read and one is kept.
    pub fn scale_min_k4(
        &self,
        sc: PointerValue<'ctx>,
        is: IntValue<'ctx>,
    ) -> (IntValue<'ctx>, IntValue<'ctx>) {
        let byte = |off: i64| self.u8_at(self.at(sc, self.add(is, self.i64(off))));
        let (lo, hi, back) = (byte(0), byte(4), byte(-4));
        let s_low = self.and(lo, self.i32(63));
        let m_low = self.and(hi, self.i32(63));
        let s_high = self
            .or(self.and(hi, self.i32(15)), self.shl(self.lshr(back, self.i32(6)), self.i32(4)));
        let m_high =
            self.or(self.lshr(hi, self.i32(4)), self.shl(self.lshr(lo, self.i32(6)), self.i32(4)));
        let low = self.slt(is, self.i64(4));
        (self.select_int(low, s_low, s_high), self.select_int(low, m_low, m_high))
    }

    /// Purpose: This function decodes 32 values of a weight row into floats,
    /// reading their scale once for all 32.
    ///
    /// Inputs:
    /// - `w`: the row's first byte.
    /// - `wt`: its type.
    /// - `wnb`: the stride between values, in bytes, for f32, f16 and bf16.
    /// - `k0`: the first of the 32, a multiple of 32.
    /// - `len`: how many of the 32 the row has, or `None` for all; fewer only
    ///   at the end of a float row whose length is not a multiple of 32.
    pub fn decode32(
        &self,
        w: PointerValue<'ctx>,
        wt: Wt,
        wnb: IntValue<'ctx>,
        k0: IntValue<'ctx>,
        len: Option<IntValue<'ctx>>,
    ) -> Vec<FloatValue<'ctx>> {
        let i32t = self.g.ctx.i32_type();
        match wt {
            Wt::F32 | Wt::F16 | Wt::Bf16 => self.floats32(w, wnb, wt, k0, len),
            Wt::Q8_0 => {
                let b = self.at(w, self.mul(self.div(k0, self.i64(32)), self.i64(34)));
                let d = self.half(b);
                (0..32).map(|l| self.fmul(d, self.itof(self.i8_at(self.at_c(b, 2 + l))))).collect()
            }
            Wt::Q4_0 => {
                let b = self.at(w, self.mul(self.div(k0, self.i64(32)), self.i64(18)));
                let d = self.half(b);
                let mut out = vec![self.f32(0.0); 32];
                for l in 0..16 {
                    let q = self.u8_at(self.at_c(b, 2 + l as i64));
                    let lo = self.sub(self.and(q, self.i32(15)), self.i32(8));
                    let hi = self.sub(self.lshr(q, self.i32(4)), self.i32(8));
                    out[l] = self.fmul(d, self.itof(lo));
                    out[l + 16] = self.fmul(d, self.itof(hi));
                }
                out
            }
            Wt::Q4K | Wt::Q5K => {
                let size = if wt == Wt::Q4K { 144 } else { 176 };
                let blk = self.div(k0, self.i64(256));
                let is = self.div(self.rem(k0, self.i64(256)), self.i64(32));
                let b = self.at(w, self.mul(blk, self.i64(size)));
                let (s, m) = self.scale_min_k4(self.at_c(b, 4), is);
                // Each value is d * s * q - dmin * m.
                let scale = self.fmul(self.half(b), self.itof(s));
                let low = self.fmul(self.half(self.at_c(b, 2)), self.itof(m));
                let group = self.div(is, self.i64(2));
                let shift = self.trunc(self.mul(self.rem(is, self.i64(2)), self.i64(4)), i32t);
                let isb = self.trunc(is, i32t);
                let first = if wt == Wt::Q4K { 16 } else { 48 };
                let qs = self.at(b, self.add(self.i64(first), self.mul(group, self.i64(32))));
                (0..32)
                    .map(|l| {
                        let byte = self.u8_at(self.at_c(qs, l));
                        let mut q = self.and(self.lshr(byte, shift), self.i32(15));
                        if wt == Wt::Q5K {
                            let hb = self.u8_at(self.at_c(b, 16 + l));
                            let bit = self.and(self.lshr(hb, isb), self.i32(1));
                            q = self.or(q, self.shl(bit, self.i32(4)));
                        }
                        self.fsub(self.fmul(scale, self.itof(q)), low)
                    })
                    .collect()
            }
            Wt::Q6K => {
                let q6 = Q6::new(self, w, k0);
                let d = self.half(self.at_c(q6.b, 208));
                let first = self.fmul(d, self.itof(self.i8_at(q6.scales)));
                let second = self.fmul(d, self.itof(self.i8_at(self.at_c(q6.scales, 1))));
                (0..32)
                    .map(|l| {
                        let q = self.sub(q6.q(self, l), self.i32(32));
                        self.fmul(if l < 16 { first } else { second }, self.itof(q))
                    })
                    .collect()
            }
        }
    }
}

/// Purpose: This struct holds where 32 values of a q6_K block are: which
/// bytes hold their low and high bits, and their two scales.
pub(crate) struct Q6<'ctx> {
    pub b: PointerValue<'ctx>,
    ql: PointerValue<'ctx>,
    qh: PointerValue<'ctx>,
    pub scales: PointerValue<'ctx>,
    lo_shift: IntValue<'ctx>,
    hi_shift: IntValue<'ctx>,
}

impl<'ctx> Q6<'ctx> {
    /// Purpose: This function finds the 32 values from `k0` of a q6_K row.
    ///
    /// Inputs:
    /// - `k`: the function being written.
    /// - `w`: the row's first byte.
    /// - `k0`: the first of the 32, a multiple of 32.
    pub fn new(k: &K<'_, 'ctx>, w: PointerValue<'ctx>, k0: IntValue<'ctx>) -> Self {
        let i32t = k.g.ctx.i32_type();
        let blk = k.div(k0, k.i64(256));
        let is = k.div(k.rem(k0, k.i64(256)), k.i64(32));
        let b = k.at(w, k.mul(blk, k.i64(210)));
        let half_n = k.div(is, k.i64(4));
        let quarter = k.rem(is, k.i64(4));
        let ql =
            k.at(b, k.add(k.mul(half_n, k.i64(64)), k.mul(k.rem(quarter, k.i64(2)), k.i64(32))));
        let qh = k.at(b, k.add(k.i64(128), k.mul(half_n, k.i64(32))));
        let scales =
            k.at(b, k.add(k.i64(192), k.add(k.mul(half_n, k.i64(8)), k.mul(quarter, k.i64(2)))));
        let lo_shift = k.trunc(k.mul(k.div(quarter, k.i64(2)), k.i64(4)), i32t);
        let hi_shift = k.trunc(k.mul(quarter, k.i64(2)), i32t);
        Q6 { b, ql, qh, scales, lo_shift, hi_shift }
    }

    /// Purpose: This function gives value `l` of the 32, 0 to 63, before 32 is
    /// taken off.
    pub fn q(&self, k: &K<'_, 'ctx>, l: i64) -> IntValue<'ctx> {
        let low = k.and(k.lshr(k.u8_at(k.at_c(self.ql, l)), self.lo_shift), k.i32(15));
        let high = k.and(k.lshr(k.u8_at(k.at_c(self.qh, l)), self.hi_shift), k.i32(3));
        k.or(low, k.shl(high, k.i32(4)))
    }
}

/// Purpose: This function gives the helper behind [`K::float_at`]:
/// `float(row, nb0, type, k)`.
fn float_fn<'ctx>(g: &Gpu<'ctx>) -> inkwell::values::FunctionValue<'ctx> {
    let (ptr, i64t, f32t) = (g.ptr(), g.ctx.i64_type(), g.ctx.f32_type());
    g.helper(
        "jade.gpu.float",
        Some(f32t.into()),
        &[ptr.into(), i64t.into(), i64t.into(), i64t.into()],
        true,
        |k| {
            let (row, nb0, t, i) = (k.param_ptr(0), k.param_int(1), k.param_int(2), k.param_int(3));
            let p = k.at(row, k.mul(i, nb0));
            let out = k.var(k.f32(0.0));
            k.switch(
                t,
                vec![
                    case(ty::F16, |k| k.store(out, k.half(p))),
                    case(ty::BF16, |k| k.store(out, k.bf16(p))),
                ],
                |k| k.store(out, k.f32_at(p)),
            );
            k.ret(k.get_f32(out));
        },
    )
}

/// Purpose: This function gives the helper behind [`K::value_at`]:
/// `value(row, nb0, type, k)`.
fn value_fn<'ctx>(g: &Gpu<'ctx>) -> inkwell::values::FunctionValue<'ctx> {
    let (ptr, i64t, f32t) = (g.ptr(), g.ctx.i64_type(), g.ctx.f32_type());
    let quant = quant_fn(g);
    g.helper(
        "jade.gpu.value",
        Some(f32t.into()),
        &[ptr.into(), i64t.into(), i64t.into(), i64t.into()],
        false,
        |k| {
            let (row, nb0, t, i) = (k.param_ptr(0), k.param_int(1), k.param_int(2), k.param_int(3));
            let p = k.at(row, k.mul(i, nb0));
            let out = k.var(k.f32(0.0));
            k.switch(
                t,
                vec![
                    case(ty::F32, |k| k.store(out, k.f32_at(p))),
                    case(ty::F16, |k| k.store(out, k.half(p))),
                    case(ty::BF16, |k| k.store(out, k.bf16(p))),
                    case(ty::I32, |k| k.store(out, k.itof(k.i32_at(p)))),
                ],
                |k| {
                    let v =
                        k.call(quant, &[row.into(), i.into(), t.into()]).expect("a float result");
                    k.store(out, v);
                },
            );
            k.ret(k.get_f32(out));
        },
    )
}

/// Purpose: This function gives `quant(row, k, type)`: value `k` of a row of
/// q8_0, q4_0, q4_K, q5_K or q6_K blocks, decoded alone. Matrix
/// multiplication decodes 32 at a time instead; this serves the ops that read
/// a weight here and there, such as looking up a token's embedding.
fn quant_fn<'ctx>(g: &Gpu<'ctx>) -> inkwell::values::FunctionValue<'ctx> {
    let (ptr, i64t, f32t) = (g.ptr(), g.ctx.i64_type(), g.ctx.f32_type());
    g.helper(
        "jade.gpu.quant",
        Some(f32t.into()),
        &[ptr.into(), i64t.into(), i64t.into()],
        false,
        |k| {
            let (row, i, t) = (k.param_ptr(0), k.param_int(1), k.param_int(2));
            let out = k.var(k.f32(0.0));
            let i32t = k.g.ctx.i32_type();
            let q8_0 = |k: &K<'_, 'ctx>| {
                let b = k.at(row, k.mul(k.div(i, k.i64(32)), k.i64(34)));
                let q = k.i8_at(k.at(b, k.add(k.i64(2), k.rem(i, k.i64(32)))));
                k.store(out, k.fmul(k.half(b), k.itof(q)));
            };
            let q4_0 = |k: &K<'_, 'ctx>| {
                let b = k.at(row, k.mul(k.div(i, k.i64(32)), k.i64(18)));
                let j = k.rem(i, k.i64(32));
                let q = k.u8_at(k.at(b, k.add(k.i64(2), k.rem(j, k.i64(16)))));
                let v = k.select_int(k.slt(j, k.i64(16)), k.and(q, k.i32(15)), k.lshr(q, k.i32(4)));
                k.store(out, k.fmul(k.half(b), k.itof(k.sub(v, k.i32(8)))));
            };
            let qk = |k: &K<'_, 'ctx>, five: bool| {
                let size = if five { 176 } else { 144 };
                let b = k.at(row, k.mul(k.div(i, k.i64(256)), k.i64(size)));
                let j = k.rem(i, k.i64(256));
                let group = k.div(j, k.i64(64));
                let sub = k.div(k.rem(j, k.i64(64)), k.i64(32));
                let is = k.add(k.mul(group, k.i64(2)), sub);
                let (s, m) = k.scale_min_k4(k.at_c(b, 4), is);
                let l = k.rem(j, k.i64(32));
                let first = if five { 48 } else { 16 };
                let byte = k.u8_at(k.at(b, k.add(k.i64(first), k.add(k.mul(group, k.i64(32)), l))));
                let shift = k.trunc(k.mul(sub, k.i64(4)), i32t);
                let mut q = k.and(k.lshr(byte, shift), k.i32(15));
                if five {
                    let hb = k.u8_at(k.at(b, k.add(k.i64(16), l)));
                    let bit = k.and(k.lshr(hb, k.trunc(is, i32t)), k.i32(1));
                    q = k.or(q, k.shl(bit, k.i32(4)));
                }
                let d = k.half(b);
                let dmin = k.half(k.at_c(b, 2));
                let v = k.fsub(k.fmul(k.fmul(d, k.itof(s)), k.itof(q)), k.fmul(dmin, k.itof(m)));
                k.store(out, v);
            };
            let q6_k = |k: &K<'_, 'ctx>| {
                let b = k.at(row, k.mul(k.div(i, k.i64(256)), k.i64(210)));
                let j = k.rem(i, k.i64(256));
                let half_n = k.div(j, k.i64(128));
                let r = k.rem(j, k.i64(128));
                let quarter = k.div(r, k.i64(32));
                let l = k.rem(r, k.i64(32));
                let ql = k.at(
                    b,
                    k.add(
                        k.mul(half_n, k.i64(64)),
                        k.add(k.mul(k.rem(quarter, k.i64(2)), k.i64(32)), l),
                    ),
                );
                let qh = k.at(b, k.add(k.i64(128), k.add(k.mul(half_n, k.i64(32)), l)));
                let lo_shift = k.trunc(k.mul(k.div(quarter, k.i64(2)), k.i64(4)), i32t);
                let hi_shift = k.trunc(k.mul(quarter, k.i64(2)), i32t);
                let low = k.and(k.lshr(k.u8_at(ql), lo_shift), k.i32(15));
                let high = k.and(k.lshr(k.u8_at(qh), hi_shift), k.i32(3));
                let q = k.sub(k.or(low, k.shl(high, k.i32(4))), k.i32(32));
                let s = k.add(
                    k.mul(half_n, k.i64(8)),
                    k.add(k.div(l, k.i64(16)), k.mul(quarter, k.i64(2))),
                );
                let scale = k.i8_at(k.at(b, k.add(k.i64(192), s)));
                let d = k.half(k.at_c(b, 208));
                k.store(out, k.fmul(k.fmul(d, k.itof(scale)), k.itof(q)));
            };
            k.switch(
                t,
                vec![
                    case(ty::Q8_0, q8_0),
                    case(ty::Q4_0, q4_0),
                    case(ty::Q4_K, |k| qk(k, false)),
                    case(ty::Q5_K, |k| qk(k, true)),
                    case(ty::Q6_K, q6_k),
                ],
                |_| {},
            );
            k.ret(k.get_f32(out));
        },
    )
}

/// Purpose: This function gives the helper behind [`K::write`]:
/// `store(p, type, v)`, writing an f32, f16 or bf16.
fn store_fn<'ctx>(g: &Gpu<'ctx>) -> inkwell::values::FunctionValue<'ctx> {
    let (ptr, i64t, f32t) = (g.ptr(), g.ctx.i64_type(), g.ctx.f32_type());
    g.helper("jade.gpu.store", None, &[ptr.into(), i64t.into(), f32t.into()], true, |k| {
        let (p, t, v) = (k.param_ptr(0), k.param_int(1), k.param_f32(2));
        let i16t = k.g.ctx.i16_type();
        k.switch(
            t,
            vec![
                case(ty::F16, |k| k.store(p, k.f32_to_half(v))),
                case(ty::BF16, |k| {
                    // ggml's rounding: to nearest, ties to even, and a NaN
                    // kept a quiet NaN.
                    let u = k.f32_to_bits(v);
                    let nan = k.ugt(k.and(u, k.i32(0x7fff_ffff)), k.i32(0x7f80_0000));
                    let odd = k.and(k.lshr(u, k.i32(16)), k.i32(1));
                    let round = k.lshr(k.add(u, k.add(k.i32(0x7fff), odd)), k.i32(16));
                    let quiet = k.or(k.lshr(u, k.i32(16)), k.i32(64));
                    let bits = k.select_int(nan, quiet, round);
                    k.store(p, k.trunc(bits, i16t));
                }),
            ],
            |k| k.store(p, v),
        );
        k.ret_void();
    })
}
