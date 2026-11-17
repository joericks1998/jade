//! `k_flash_attn`: attention over the KV cache, the softmax computed in one
//! pass as `flash_attn_ext` does.
//!
//! `q [D, T, H, 1]`, `k [D, KV, Hkv, 1]`, `v [Dv, KV, Hkv, 1]` and
//! `mask [KV, T]` give `dst [Dv, H, T, 1]`, with float parameter 0 the scale.
//! One warp per query head and token: thread `lane` holds values `lane`,
//! `lane + 32` and so on of the query and of the output, up to eight each,
//! since a head is at most 256 wide. Each cell's score is a warp sum, and the
//! running maximum rescales what came before, so the cache is read once. Only
//! the first cells, integer parameter 6, hold tokens; the mask hides every
//! cell past them, and the loop stops there. The same as `k_flash_attn` in
//! `apple/kernels.metal`.

use inkwell::FloatPredicate;
use inkwell::values::{FloatValue, PointerValue};

use super::builder::{Gpu, WARP};
use super::values::DST;

/// The most values of a head each thread holds: 256 values over 32 threads.
const PER_LANE: i64 = 8;

/// Purpose: This function writes `k_flash_attn`.
pub(super) fn flash_attn(g: &Gpu<'_>) {
    g.kernel("k_flash_attn", |k| {
        let item = k.div(k.gid(), k.i64(WARP));
        k.exit_if(k.sge(item, k.iparam(7)));
        let lane = k.lane();
        let (q, kc, vc, mask, d) =
            (k.tensor(0), k.tensor(1), k.tensor(2), k.tensor(3), k.tensor(DST));
        let tokens = q.ne[1];
        let t = k.rem(item, tokens);
        let h = k.div(item, tokens);
        let (dk, dv) = (q.ne[0], vc.ne[0]);
        let n_kv = k.min(kc.ne[1], k.iparam(6));
        let hkv = k.div(h, k.div(q.ne[2], kc.ne[2]));
        let scale = k.fparam(0);
        let zero = k.f32(0.0);

        // This thread's value `m` of a head is `lane + 32 m`. A group of 32
        // values the head lacks entirely is skipped by the whole warp; inside
        // the last group, a value past the head reads its last value instead
        // and counts as zero, so nothing past the head is read.
        let elem = |m: i64| k.add(lane, k.i64(m * WARP));
        let qv: Vec<FloatValue> = (0..PER_LANE)
            .map(|m| {
                let slot = k.var(zero);
                k.if_(k.slt(k.i64(m * WARP), dk), |k| {
                    let e = elem(m);
                    let at = k.min(e, k.sub(dk, k.i64(1)));
                    let v = k.read(k.tensor_ptr(0), &q, [at, t, h, k.i64(0)]);
                    k.store(slot, k.select_f(k.slt(e, dk), v, zero));
                });
                k.get_f32(slot)
            })
            .collect();
        let acc: Vec<PointerValue> = (0..PER_LANE).map(|_| k.var(zero)).collect();

        // This head's cache and this token's mask row, found once; each cell
        // is then one stride further on.
        let k_head = k.at(k.tensor_ptr(1), k.mul(hkv, kc.nb[2]));
        let v_head = k.at(k.tensor_ptr(2), k.mul(hkv, vc.nb[2]));
        let m_row = k.at(k.tensor_ptr(3), k.mul(t, mask.nb[1]));
        let running = k.var(k.f32(f32::NEG_INFINITY));
        let denom = k.var(zero);
        k.for_step(k.i64(0), n_kv, k.i64(1), |k, j| {
            let mk = k.float_at(m_row, mask.nb[0], mask.ty, j);
            let seen = k.fcmp(FloatPredicate::UNE, mk, k.f32(f32::NEG_INFINITY));
            k.if_(seen, |k| {
                let k_cell = k.at(k_head, k.mul(j, kc.nb[1]));
                let dot = k.var(zero);
                for m in 0..PER_LANE {
                    k.if_(k.slt(k.i64(m * WARP), dk), |k| {
                        let e = elem(m);
                        let at = k.min(e, k.sub(dk, k.i64(1)));
                        let kv = k.float_at(k_cell, kc.nb[0], kc.ty, at);
                        let term = k.select_f(k.slt(e, dk), k.fmul(qv[m as usize], kv), zero);
                        k.store(dot, k.fadd(k.get_f32(dot), term));
                    });
                }
                let s = k.fadd(k.fmul(k.warp_sum(k.get_f32(dot)), scale), mk);
                let prev = k.get_f32(running);
                let next = k.fmax(prev, s);
                let old_scale = k.exp(k.fsub(prev, next));
                let weight = k.exp(k.fsub(s, next));
                k.store(denom, k.fadd(k.fmul(k.get_f32(denom), old_scale), weight));
                let v_cell = k.at(v_head, k.mul(j, vc.nb[1]));
                for m in 0..PER_LANE {
                    let slot = acc[m as usize];
                    k.if_(k.slt(k.i64(m * WARP), dv), |k| {
                        let at = k.min(elem(m), k.sub(dv, k.i64(1)));
                        let vv = k.float_at(v_cell, vc.nb[0], vc.ty, at);
                        let kept = k.fmul(k.get_f32(slot), old_scale);
                        k.store(slot, k.fadd(kept, k.fmul(weight, vv)));
                    });
                }
                k.store(running, next);
            });
        });

        let den = k.get_f32(denom);
        let inv = k.select_f(k.fcmp(FloatPredicate::OGT, den, zero), k.fdiv(k.f32(1.0), den), zero);
        for m in 0..PER_LANE {
            let e = elem(m);
            let v = k.fmul(k.get_f32(acc[m as usize]), inv);
            k.if_(k.slt(e, dv), |k| k.write(k.tensor_ptr(4), &d, [e, h, t, k.i64(0)], v));
        }
    });
}
