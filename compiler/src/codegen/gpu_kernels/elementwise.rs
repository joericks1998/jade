//! The kernels that compute each value of their output on its own, one thread
//! per value: `get_rows`, `mul`, `add`, `rope`, `cpy`, `set_rows` and
//! `swiglu`. Each is the kernel of the same name in `apple/kernels.metal`,
//! written through the builder.
//!
//! Every kernel starts the same way: its thread's index in the grid, and an
//! early end for a thread past the op's count, the argument record's integer
//! parameter 7, since the last block is rounded up to whole blocks.

use super::builder::{Gpu, K};
use super::values::{DST, Tensor};
use inkwell::values::IntValue;

/// Purpose: This function gives the thread's index in the grid, and ends a
/// thread with no output to compute.
fn start<'ctx>(k: &K<'_, 'ctx>) -> IntValue<'ctx> {
    let gid = k.gid();
    k.exit_if(k.sge(gid, k.iparam(7)));
    gid
}

/// Purpose: This function writes `k_get_rows`: `dst[:, r] = src0[:, rows[r]]`,
/// which looks up each token's embedding.
pub(super) fn get_rows(g: &Gpu<'_>) {
    g.kernel("k_get_rows", |k| {
        let gid = start(k);
        let (s0, s1, d) = (k.tensor(0), k.tensor(1), k.tensor(DST));
        let i = k.unflatten(gid, &d.ne);
        let r = k.index_at(k.tensor_ptr(1), &s1, i[1], i[2]);
        let v = k.read(k.tensor_ptr(0), &s0, [i[0], r, i[2], i[3]]);
        k.write(k.tensor_ptr(4), &d, i, v);
    });
}

/// Purpose: This function reads `src1` broadcast over the destination's
/// shape, as ggml's binary ops do: each of its dimensions divides the
/// destination's.
fn broadcast<'ctx>(
    k: &K<'_, 'ctx>,
    t: &Tensor<'ctx>,
    i: [IntValue<'ctx>; 4],
) -> inkwell::values::FloatValue<'ctx> {
    let j =
        [k.rem(i[0], t.ne[0]), k.rem(i[1], t.ne[1]), k.rem(i[2], t.ne[2]), k.rem(i[3], t.ne[3])];
    k.read(k.tensor_ptr(1), t, j)
}

/// Purpose: This function writes `k_mul`: `dst = src0 * src1`, `src1`
/// broadcast.
pub(super) fn mul(g: &Gpu<'_>) {
    g.kernel("k_mul", |k| {
        let gid = start(k);
        let (s0, s1, d) = (k.tensor(0), k.tensor(1), k.tensor(DST));
        let i = k.unflatten(gid, &d.ne);
        let v = k.fmul(k.read(k.tensor_ptr(0), &s0, i), broadcast(k, &s1, i));
        k.write(k.tensor_ptr(4), &d, i, v);
    });
}

/// Purpose: This function writes `k_add`: `dst = src0 + src1`, `src1`
/// broadcast.
pub(super) fn add(g: &Gpu<'_>) {
    g.kernel("k_add", |k| {
        let gid = start(k);
        let (s0, s1, d) = (k.tensor(0), k.tensor(1), k.tensor(DST));
        let i = k.unflatten(gid, &d.ne);
        let v = k.fadd(k.read(k.tensor_ptr(0), &s0, i), broadcast(k, &s1, i));
        k.write(k.tensor_ptr(4), &d, i, v);
    });
}

/// Purpose: This function writes `k_rope`: rotary position embedding.
///
/// Float parameters: 0 the frequency base, 1 the frequency scale, 2 the
/// attention factor, 3 the base to the power `-2 / n_dims`. Integer
/// parameters: 0 the dimensions rotated; 1 the mode, 0 normal or 2 neox; 2 is
/// 1 when `src2` holds Llama 3.1's frequency factors, one per pair, which
/// divide each pair's angle. Token `i2` is at position `pos[i2]`. One thread
/// per value: the first of each rotated pair rotates both, the second does
/// nothing, and a value past the rotated dimensions is copied.
pub(super) fn rope(g: &Gpu<'_>) {
    g.kernel("k_rope", |k| {
        let gid = start(k);
        let (s0, s1, d) = (k.tensor(0), k.tensor(1), k.tensor(DST));
        let src = k.tensor_ptr(0);
        let dst = k.tensor_ptr(4);
        let i = k.unflatten(gid, &d.ne);
        let n_dims = k.iparam(0);
        let neox = k.eq(k.iparam(1), k.i64(2));
        k.if_(k.sge(i[0], n_dims), |k| {
            k.write(dst, &d, i, k.read(src, &s0, i));
            k.ret_void();
        });
        let half_dims = k.div(n_dims, k.i64(2));
        // Normal mode pairs neighbours, neox pairs a value with the one half
        // the rotated dimensions after it.
        let second_of_pair =
            k.select_int(neox, k.sge(i[0], half_dims), k.eq(k.and(i[0], k.i64(1)), k.i64(1)));
        k.exit_if(second_of_pair);
        let pair = k.select_int(neox, i[0], k.div(i[0], k.i64(2)));
        let other = k.select_int(neox, k.add(i[0], half_dims), k.add(i[0], k.i64(1)));
        let pos = k.itof(k.index_at(k.tensor_ptr(1), &s1, i[2], k.i64(0)));
        // The pair's frequency is base^(-2 pair / n_dims): the plan's
        // base^(-2 / n_dims), worked out once on the CPU, raised to `pair` by
        // squaring. A fast pow on the GPU would put the angle off by about
        // 0.005 radians at position 3000.
        let mut power = k.f32(1.0);
        let mut square = k.fparam(3);
        for bit in 0..16 {
            let set = k.ne(k.and(pair, k.i64(1 << bit)), k.i64(0));
            power = k.select_f(set, k.fmul(power, square), power);
            square = k.fmul(square, square);
        }
        let theta = k.var(k.fmul(k.fmul(k.fparam(1), pos), power));
        k.if_(k.ne(k.iparam(2), k.i64(0)), |k| {
            let zero = k.i64(0);
            let factor = k.read(k.tensor_ptr(2), &k.tensor(2), [pair, zero, zero, zero]);
            k.store(theta, k.fdiv(k.get_f32(theta), factor));
        });
        let theta = k.get_f32(theta);
        let (sin, cos) = k.sin_cos(theta);
        let c = k.fmul(cos, k.fparam(2));
        let s = k.fmul(sin, k.fparam(2));
        let j = [other, i[1], i[2], i[3]];
        let x0 = k.read(src, &s0, i);
        let x1 = k.read(src, &s0, j);
        k.write(dst, &d, i, k.fsub(k.fmul(x0, c), k.fmul(x1, s)));
        k.write(dst, &d, j, k.fadd(k.fmul(x0, s), k.fmul(x1, c)));
    });
}

/// Purpose: This function writes `k_cpy`, for `cpy` and `cont`: `dst = src0`,
/// converting the type, both walked in element order.
pub(super) fn cpy(g: &Gpu<'_>) {
    g.kernel("k_cpy", |k| {
        let gid = start(k);
        let (s0, d) = (k.tensor(0), k.tensor(DST));
        let i = k.unflatten(gid, &d.ne);
        let j = k.unflatten(gid, &s0.ne);
        k.write(k.tensor_ptr(4), &d, i, k.read(k.tensor_ptr(0), &s0, j));
    });
}

/// Purpose: This function writes `k_set_rows`: `dst[:, rows[r]] = src1[:, r]`,
/// which writes each token's keys and values into the cache. `src0` is the
/// destination itself, so the kernel reads `src1` and `src2` alone. One
/// thread per value of `src1`.
pub(super) fn set_rows(g: &Gpu<'_>) {
    g.kernel("k_set_rows", |k| {
        let gid = start(k);
        let (s1, s2, d) = (k.tensor(1), k.tensor(2), k.tensor(DST));
        let i = k.unflatten(gid, &s1.ne);
        let r = k.index_at(k.tensor_ptr(2), &s2, i[1], i[2]);
        let v = k.read(k.tensor_ptr(1), &s1, i);
        k.write(k.tensor_ptr(4), &d, [i[0], r, i[2], i[3]], v);
    });
}

/// Purpose: This function writes `k_swiglu`: `dst = silu(gate) * up`, where
/// `silu(x) = x / (1 + e^-x)`.
pub(super) fn swiglu(g: &Gpu<'_>) {
    g.kernel("k_swiglu", |k| {
        let gid = start(k);
        let (s0, s1, d) = (k.tensor(0), k.tensor(1), k.tensor(DST));
        let i = k.unflatten(gid, &d.ne);
        let gate = k.read(k.tensor_ptr(0), &s0, i);
        let up = k.read(k.tensor_ptr(1), &s1, i);
        let silu = k.fdiv(gate, k.fadd(k.f32(1.0), k.exp(k.fneg(gate))));
        k.write(k.tensor_ptr(4), &d, i, k.fmul(silu, up));
    });
}
