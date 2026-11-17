//! `k_rms_norm`: each row divided by the root of its mean square.
//!
//! A row is up to a few thousand values, so one thread reading it alone would
//! leave the GPU waiting on memory. Each row gets one warp instead: thread
//! `lane` takes values `lane`, `lane + 32` and so on, and the warp adds up the
//! 32 sums. The same as `k_rms_norm` in `apple/kernels.metal`.

use super::builder::{Gpu, WARP};
use super::values::DST;

/// Purpose: This function writes `k_rms_norm`: `dst = src0 / sqrt(mean(src0^2)
/// + eps)` per row, with `eps` float parameter 0.
pub(super) fn rms_norm(g: &Gpu<'_>) {
    g.kernel("k_rms_norm", |k| {
        let row = k.div(k.gid(), k.i64(WARP));
        k.exit_if(k.sge(row, k.iparam(7)));
        let lane = k.lane();
        let (s0, d) = (k.tensor(0), k.tensor(DST));
        let (src, dst) = (k.tensor_ptr(0), k.tensor_ptr(4));
        let i1 = k.rem(row, d.ne[1]);
        let rest = k.div(row, d.ne[1]);
        let i2 = k.rem(rest, d.ne[2]);
        let i3 = k.div(rest, d.ne[2]);
        let n = s0.ne[0];
        let sum = k.var(k.f32(0.0));
        k.for_step(lane, n, k.i64(WARP), |k, c| {
            let x = k.read(src, &s0, [c, i1, i2, i3]);
            k.store(sum, k.fadd(k.get_f32(sum), k.fmul(x, x)));
        });
        let total = k.warp_sum(k.get_f32(sum));
        let mean = k.fadd(k.fdiv(total, k.itof(n)), k.fparam(0));
        let scale = k.fdiv(k.f32(1.0), k.sqrt(mean));
        k.for_step(lane, n, k.i64(WARP), |k, c| {
            let x = k.read(src, &s0, [c, i1, i2, i3]);
            k.write(dst, &d, [c, i1, i2, i3], k.fmul(x, scale));
        });
    });
}
