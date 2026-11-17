//! Matrix multiplication, where a model's step spends nearly all its time:
//! `mul_mat(src0 [K, M], src1 [K, N, ...]) → dst [M, N, ...]`, each output the
//! dot product of a row of weights and a column of `src1`. `src0`'s batch
//! dimensions broadcast over `src1`'s.
//!
//! *`k_mul_mat`*, for a step of one token, gives each output one warp. Thread
//! `lane` takes the row's 32-value chunks `lane`, `lane + 32` and so on, so the
//! warp reads the row front to back together, decoding each chunk's scale
//! once. The weight type is checked once per output, not once per chunk: the
//! loop is written out for each type.
//!
//! *`k_mul_mm`*, for a step of many tokens, gives each block of 64 threads a
//! tile of 32 rows by 32 tokens. For every 32 values of K, the first warp
//! decodes the tile's 32 rows of weights into shared memory and the second
//! copies its 32 columns of `src1` beside them; then each thread multiplies a
//! 4 by 4 piece of the tile. Each weight is decoded once per tile rather than
//! once per token. NVIDIA's tensor cores come with the tuning, later.
//!
//! Both add up a sum in a different order, so a token's values can differ in
//! their last bits between a step of one token and a step of many, as they do
//! in llama.cpp and on Metal.

use inkwell::values::{FloatValue, IntValue, PointerValue};

use super::builder::{Case, Gpu, K, WARP, case};
use super::values::{DST, Q6, Wt};

/// Purpose: This function adds up a list of floats in order.
fn sum<'ctx>(
    k: &K<'_, 'ctx>,
    terms: impl IntoIterator<Item = FloatValue<'ctx>>,
) -> FloatValue<'ctx> {
    terms.into_iter().fold(k.f32(0.0), |a, t| k.fadd(a, t))
}

/// Purpose: This function gives the dot product of 32 values of a weight row
/// with 32 floats, decoding the weights' scales once for all 32.
///
/// Inputs:
/// - `w`: the weight row's first byte.
/// - `wt`: its type.
/// - `wnb`: the stride between weights, in bytes, for f32, f16 and bf16.
/// - `xs`: the 32 floats.
/// - `k0`: the first of the 32 values, a multiple of 32.
/// - `len`: how many of the 32 to take, or `None` for all; fewer only at the
///   end of a float row whose length is not a multiple of 32.
fn dot32<'ctx>(
    k: &K<'_, 'ctx>,
    w: PointerValue<'ctx>,
    wt: Wt,
    wnb: IntValue<'ctx>,
    xs: &[FloatValue<'ctx>],
    k0: IntValue<'ctx>,
    len: Option<IntValue<'ctx>>,
) -> FloatValue<'ctx> {
    let i32t = k.g.ctx.i32_type();
    match wt {
        Wt::F32 | Wt::F16 | Wt::Bf16 => {
            let ws = k.floats32(w, wnb, wt, k0, len);
            sum(k, ws.iter().zip(xs).map(|(a, b)| k.fmul(*a, *b)))
        }
        Wt::Q8_0 => {
            let b = k.at(w, k.mul(k.div(k0, k.i64(32)), k.i64(34)));
            let s =
                sum(k, (0..32).map(|l| k.fmul(k.itof(k.i8_at(k.at_c(b, 2 + l))), xs[l as usize])));
            k.fmul(k.half(b), s)
        }
        Wt::Q4_0 => {
            let b = k.at(w, k.mul(k.div(k0, k.i64(32)), k.i64(18)));
            let s = sum(
                k,
                (0..16usize).flat_map(|l| {
                    let q = k.u8_at(k.at_c(b, 2 + l as i64));
                    let lo = k.sub(k.and(q, k.i32(15)), k.i32(8));
                    let hi = k.sub(k.lshr(q, k.i32(4)), k.i32(8));
                    [k.fmul(k.itof(lo), xs[l]), k.fmul(k.itof(hi), xs[l + 16])]
                }),
            );
            k.fmul(k.half(b), s)
        }
        Wt::Q4K | Wt::Q5K => {
            let size = if wt == Wt::Q4K { 144 } else { 176 };
            let blk = k.div(k0, k.i64(256));
            let is = k.div(k.rem(k0, k.i64(256)), k.i64(32));
            let b = k.at(w, k.mul(blk, k.i64(size)));
            let (s, m) = k.scale_min_k4(k.at_c(b, 4), is);
            // Each value is d * s * q - dmin * m, so the 32 share both terms.
            let group = k.div(is, k.i64(2));
            let shift = k.trunc(k.mul(k.rem(is, k.i64(2)), k.i64(4)), i32t);
            let isb = k.trunc(is, i32t);
            let first = if wt == Wt::Q4K { 16 } else { 48 };
            let qs = k.at(b, k.add(k.i64(first), k.mul(group, k.i64(32))));
            let qx = sum(
                k,
                (0..32).map(|l| {
                    let mut q = k.and(k.lshr(k.u8_at(k.at_c(qs, l)), shift), k.i32(15));
                    if wt == Wt::Q5K {
                        let hb = k.u8_at(k.at_c(b, 16 + l));
                        q = k.or(q, k.shl(k.and(k.lshr(hb, isb), k.i32(1)), k.i32(4)));
                    }
                    k.fmul(k.itof(q), xs[l as usize])
                }),
            );
            let x_sum = sum(k, xs.iter().copied());
            let d = k.fmul(k.half(b), k.itof(s));
            let dmin = k.fmul(k.half(k.at_c(b, 2)), k.itof(m));
            k.fsub(k.fmul(d, qx), k.fmul(dmin, x_sum))
        }
        Wt::Q6K => {
            let q6 = Q6::new(k, w, k0);
            let term = |l: i64| k.fmul(k.itof(k.sub(q6.q(k, l), k.i32(32))), xs[l as usize]);
            // The first 16 values have one scale and the last 16 the next.
            let first = sum(k, (0..16).map(term));
            let second = sum(k, (16..32).map(term));
            let s0 = k.itof(k.i8_at(q6.scales));
            let s1 = k.itof(k.i8_at(k.at_c(q6.scales, 1)));
            let d = k.half(k.at_c(q6.b, 208));
            k.fmul(d, k.fadd(k.fmul(s0, first), k.fmul(s1, second)))
        }
    }
}

/// Purpose: This function writes `k_mul_mat`: one warp per output, for a step
/// of one token.
pub(super) fn mul_mat(g: &Gpu<'_>) {
    g.kernel("k_mul_mat", |k| {
        let out = k.div(k.gid(), k.i64(WARP));
        k.exit_if(k.sge(out, k.iparam(7)));
        let lane = k.lane();
        let (s0, s1, d) = (k.tensor(0), k.tensor(1), k.tensor(DST));
        let i = k.unflatten(out, &d.ne);
        let k_n = s0.ne[0];
        let b2 = k.div(i[2], k.div(d.ne[2], s0.ne[2]));
        let b3 = k.div(i[3], k.div(d.ne[3], s0.ne[3]));
        let w_off = k.add(k.mul(i[0], s0.nb[1]), k.add(k.mul(b2, s0.nb[2]), k.mul(b3, s0.nb[3])));
        let w = k.at(k.tensor_ptr(0), w_off);
        let x_off =
            k.add(k.mul(i[1], s1.nb[1]), k.add(k.mul(i[2], s1.nb[2]), k.mul(i[3], s1.nb[3])));
        let x = k.at(k.tensor_ptr(1), x_off);
        let acc = k.var(k.f32(0.0));
        // Chunks that start before this are whole.
        let whole = k.sub(k_n, k.i64(31));
        let first = k.mul(lane, k.i64(32));
        let step = k.i64(WARP * 32);
        let cases: Vec<Case> = Wt::ALL
            .iter()
            .map(|&wt| {
                case(wt.code(), move |k| {
                    k.for_step(first, whole, step, |k, k0| {
                        let xs = k.floats32_any(x, s1.nb[0], s1.ty, k0, None);
                        let part = dot32(k, w, wt, s0.nb[0], &xs, k0, None);
                        k.store(acc, k.fadd(k.get_f32(acc), part));
                    });
                })
            })
            .collect();
        k.switch(s0.ty, cases, |_| {});
        // A float row whose length is not a multiple of 32 ends in a part
        // chunk, taken by the thread whose turn it is.
        let tail = k.rem(k_n, k.i64(32));
        let k0 = k.sub(k_n, tail);
        let turn = k.rem(k.div(k_n, k.i64(32)), k.i64(WARP));
        k.if_(k.and(k.ne(tail, k.i64(0)), k.eq(lane, turn)), |k| {
            let cases: Vec<Case> = [Wt::F32, Wt::F16, Wt::Bf16]
                .iter()
                .map(|&wt| {
                    case(wt.code(), move |k| {
                        let xs = k.floats32_any(x, s1.nb[0], s1.ty, k0, Some(tail));
                        let part = dot32(k, w, wt, s0.nb[0], &xs, k0, Some(tail));
                        k.store(acc, k.fadd(k.get_f32(acc), part));
                    })
                })
                .collect();
            k.switch(s0.ty, cases, |_| {});
        });
        let total = k.warp_sum(k.get_f32(acc));
        k.if_(k.eq(lane, k.i64(0)), |k| k.write(k.tensor_ptr(4), &d, i, total));
    });
}

/// The rows of weights, the tokens and the values of K in one `k_mul_mm`
/// tile: `threads::TILE` in `crate::infer::plan`.
const TILE: i64 = 32;

/// The floats between one row of a tile in shared memory and the next: one
/// more than a row, so the 32 threads of a warp reading a column touch 32
/// different banks.
const STRIDE: i64 = TILE + 1;

/// Purpose: This function writes 32 floats into a row of a tile in shared
/// memory.
fn put_row<'ctx>(k: &K<'_, 'ctx>, row: PointerValue<'ctx>, vals: Vec<FloatValue<'ctx>>) {
    for (l, v) in vals.into_iter().enumerate() {
        k.store(k.at_c(row, l as i64 * 4), v);
    }
}

/// Purpose: This function writes `k_mul_mm`: one block of 64 threads per tile
/// of 32 rows by 32 tokens, for a step of many tokens.
pub(super) fn mul_mm(g: &Gpu<'_>) {
    g.kernel("k_mul_mm", |k| {
        let item = k.block_x();
        k.exit_if(k.sge(item, k.iparam(7)));
        let tid = k.thread_x();
        let lane = k.lane();
        let weights = k.slt(tid, k.i64(WARP));
        let (s0, s1, d) = (k.tensor(0), k.tensor(1), k.tensor(DST));
        let (rows, n) = (d.ne[0], d.ne[1]);
        let tiles = |len| k.div(k.add(len, k.i64(TILE - 1)), k.i64(TILE));
        let (row_tiles, col_tiles) = (tiles(rows), tiles(n));
        let r0 = k.mul(k.rem(item, row_tiles), k.i64(TILE));
        let rest = k.div(item, row_tiles);
        let c0 = k.mul(k.rem(rest, col_tiles), k.i64(TILE));
        let rest = k.div(rest, col_tiles);
        let i2 = k.rem(rest, d.ne[2]);
        let i3 = k.div(rest, d.ne[2]);
        let b2 = k.div(i2, k.div(d.ne[2], s0.ne[2]));
        let b3 = k.div(i3, k.div(d.ne[3], s0.ne[3]));
        let k_n = s0.ne[0];

        // The tile's weights as [row][k], then its columns as [column][k].
        let sa = k.shared((TILE * STRIDE) as u32);
        let sb = k.shared((TILE * STRIDE) as u32);
        // The first warp's thread `lane` loads row `r0 + lane` of weights, the
        // second warp's loads column `c0 + lane`, when the tile has it.
        let mine = k.add(k.select_int(weights, r0, c0), lane);
        let have = k.slt(mine, k.select_int(weights, rows, n));
        let w_off = k.add(k.mul(mine, s0.nb[1]), k.add(k.mul(b2, s0.nb[2]), k.mul(b3, s0.nb[3])));
        let w = k.at(k.tensor_ptr(0), w_off);
        let x_off = k.add(k.mul(mine, s1.nb[1]), k.add(k.mul(i2, s1.nb[2]), k.mul(i3, s1.nb[3])));
        let x = k.at(k.tensor_ptr(1), x_off);
        let row = k.at(k.select_ptr(weights, sa, sb), k.mul(lane, k.i64(STRIDE * 4)));
        // This thread's 4 by 4 piece of the tile.
        let rr = k.mul(k.rem(tid, k.i64(8)), k.i64(4));
        let cc = k.mul(k.div(tid, k.i64(8)), k.i64(4));
        let acc: Vec<PointerValue> = (0..16).map(|_| k.var(k.f32(0.0))).collect();

        k.for_step(k.i64(0), k_n, k.i64(TILE), |k, k0| {
            let len = k.min(k.sub(k_n, k0), k.i64(TILE));
            k.if_else(
                k.not(have),
                |k| put_row(k, row, vec![k.f32(0.0); 32]),
                |k| {
                    k.if_else(
                        weights,
                        |k| {
                            let cases: Vec<Case> = Wt::ALL
                                .iter()
                                .map(|&wt| {
                                    case(wt.code(), move |k| {
                                        let part = if wt.is_float() { Some(len) } else { None };
                                        put_row(k, row, k.decode32(w, wt, s0.nb[0], k0, part));
                                    })
                                })
                                .collect();
                            k.switch(s0.ty, cases, |_| {});
                        },
                        |k| put_row(k, row, k.floats32_any(x, s1.nb[0], s1.ty, k0, Some(len))),
                    );
                },
            );
            k.barrier();
            k.for_step(k.i64(0), k.i64(TILE), k.i64(1), |k, kk| {
                let read = |base, at| {
                    k.f32_at(k.at(base, k.mul(k.add(k.mul(at, k.i64(STRIDE)), kk), k.i64(4))))
                };
                let a: Vec<FloatValue> = (0..4).map(|i| read(sa, k.add(rr, k.i64(i)))).collect();
                let b: Vec<FloatValue> = (0..4).map(|j| read(sb, k.add(cc, k.i64(j)))).collect();
                for i in 0..4 {
                    for j in 0..4 {
                        let slot = acc[i * 4 + j];
                        k.store(slot, k.fadd(k.get_f32(slot), k.fmul(a[i], b[j])));
                    }
                }
            });
            k.barrier();
        });

        for i in 0..4 {
            for j in 0..4 {
                let r = k.add(k.add(r0, rr), k.i64(i));
                let c = k.add(k.add(c0, cc), k.i64(j));
                let inside = k.and(k.slt(r, rows), k.slt(c, n));
                let v = k.get_f32(acc[(i * 4 + j) as usize]);
                k.if_(inside, |k| k.write(k.tensor_ptr(4), &d, [r, c, i2, i3], v));
            }
        }
    });
}
