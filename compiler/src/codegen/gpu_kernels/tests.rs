//! The kernels' tests: every kernel run over small tensors of each type it
//! reads, and its output compared with the same op worked out plainly, in
//! f64, from ggml's definitions.
//!
//! The cases are shared by two runners: `emulate.rs`, which runs the kernels
//! on the CPU wherever the tests run, and `nvidia/tests.rs`, which runs them
//! on an NVIDIA GPU when the machine has one. A case is an op record built the
//! way `crate::infer::plan` builds one, and [`launch`] turns it into an
//! argument record and a grid the way the step in `codegen/infer/metal.rs`
//! does, so a case checks the contract between the plan, the step and the
//! kernel, not the kernel alone.
//!
//! The block types are decoded here from ggml's reference code,
//! `dequantize_row_*` in `ggml-quants.c`, written out again rather than
//! shared with the kernels, so a mistake in one shows up as a difference.

use super::ARGS_BYTES;
use super::apple;
use super::builder::BLOCK;
use super::emulate::Emulator;
use super::values::ty;
use crate::infer::plan::{KERNELS, MUL_MM, OP_WORDS, block, rope_theta_scale, threads};

/// Purpose: This trait runs a kernel, on the CPU or on a GPU.
pub(super) trait Runner {
    /// Purpose: This function names where the kernels run, for messages.
    fn name(&self) -> &str;

    /// Purpose: This function runs one kernel over a grid.
    ///
    /// Inputs:
    /// - `kernel`: its name.
    /// - `bufs`: the four source tensors and the destination, empty for none;
    ///   the destination is read back into `bufs[4]`.
    /// - `args`: the argument record.
    /// - `blocks`: how many blocks of `BLOCK` threads.
    ///
    /// Outputs: `Ok(())`, or why it could not run.
    fn run(
        &mut self,
        kernel: &str,
        bufs: &mut [Vec<u8>; 5],
        args: &[u8],
        blocks: u64,
    ) -> Result<(), String>;
}

// ── tensors ─────────────────────────────────────────────────────────────────

/// Purpose: This struct holds one tensor of a case: its bytes, shape,
/// strides and ggml type.
#[derive(Clone)]
pub(super) struct T {
    bytes: Vec<u8>,
    ne: [i64; 4],
    nb: [i64; 4],
    ty: i64,
}

/// Purpose: This function gives a contiguous tensor's strides, as the plan
/// does.
fn strides(t: i64, ne: [i64; 4]) -> [i64; 4] {
    let (blk, size) = block(t).expect("a type the kernels read");
    let row = ne[0] / blk * size;
    [size, row, row * ne[1], row * ne[1] * ne[2]]
}

/// Purpose: This function reads an IEEE half, exactly.
fn f16(bits: u16) -> f64 {
    let sign = if bits >> 15 == 1 { -1.0 } else { 1.0 };
    let e = i32::from((bits >> 10) & 31);
    let m = f64::from(bits & 1023);
    match e {
        0 => sign * m * 2f64.powi(-24),
        31 if m == 0.0 => sign * f64::INFINITY,
        31 => f64::NAN,
        _ => sign * (1.0 + m / 1024.0) * 2f64.powi(e - 15),
    }
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

/// Purpose: This function gives the 6-bit scale and min `j` of a q4_K or
/// q5_K block: ggml's `get_scale_min_k4`.
fn scale_min_k4(j: usize, q: &[u8]) -> (f64, f64) {
    if j < 4 {
        (f64::from(q[j] & 63), f64::from(q[j + 4] & 63))
    } else {
        (
            f64::from((q[j + 4] & 0xF) | ((q[j - 4] >> 6) << 4)),
            f64::from((q[j + 4] >> 4) | ((q[j] >> 6) << 4)),
        )
    }
}

/// Purpose: This function decodes a row of a block type, as ggml's
/// `dequantize_row_*` does.
///
/// Inputs:
/// - `t`: the type.
/// - `row`: the row's bytes.
/// - `n`: its values.
fn dequant(t: i64, row: &[u8], n: usize) -> Vec<f64> {
    let mut y = Vec::with_capacity(n);
    match t {
        ty::Q8_0 => {
            for b in row.chunks(34).take(n / 32) {
                let d = f16(u16_at(b, 0));
                y.extend((0..32).map(|j| d * f64::from(b[2 + j] as i8)));
            }
        }
        ty::Q4_0 => {
            for b in row.chunks(18).take(n / 32) {
                let d = f16(u16_at(b, 0));
                y.extend((0..16).map(|j| d * f64::from(i32::from(b[2 + j] & 0xF) - 8)));
                y.extend((0..16).map(|j| d * f64::from(i32::from(b[2 + j] >> 4) - 8)));
            }
        }
        ty::Q4_K | ty::Q5_K => {
            let five = t == ty::Q5_K;
            let size = if five { 176 } else { 144 };
            for b in row.chunks(size).take(n / 256) {
                let (d, min) = (f16(u16_at(b, 0)), f16(u16_at(b, 2)));
                let scales = &b[4..16];
                let qh = &b[16..48];
                let ql = if five { &b[48..176] } else { &b[16..144] };
                let (mut is, mut q, mut u1, mut u2) = (0, 0, 1u8, 2u8);
                for _ in 0..4 {
                    let (sc, m) = scale_min_k4(is, scales);
                    let (d1, m1) = (d * sc, min * m);
                    let (sc, m) = scale_min_k4(is + 1, scales);
                    let (d2, m2) = (d * sc, min * m);
                    for l in 0..32 {
                        let hi = if five && qh[l] & u1 != 0 { 16.0 } else { 0.0 };
                        y.push(d1 * (f64::from(ql[q + l] & 0xF) + hi) - m1);
                    }
                    for l in 0..32 {
                        let hi = if five && qh[l] & u2 != 0 { 16.0 } else { 0.0 };
                        y.push(d2 * (f64::from(ql[q + l] >> 4) + hi) - m2);
                    }
                    q += 32;
                    is += 2;
                    u1 <<= 2;
                    u2 <<= 2;
                }
            }
        }
        ty::Q6_K => {
            for b in row.chunks(210).take(n / 256) {
                let d = f16(u16_at(b, 208));
                let mut out = [0.0; 256];
                for half in 0..2 {
                    let ql = &b[half * 64..];
                    let qh = &b[128 + half * 32..];
                    let sc = &b[192 + half * 8..];
                    for l in 0..32 {
                        let is = l / 16;
                        let q1 = i32::from((ql[l] & 0xF) | ((qh[l] & 3) << 4)) - 32;
                        let q2 = i32::from((ql[l + 32] & 0xF) | (((qh[l] >> 2) & 3) << 4)) - 32;
                        let q3 = i32::from((ql[l] >> 4) | (((qh[l] >> 4) & 3) << 4)) - 32;
                        let q4 = i32::from((ql[l + 32] >> 4) | (((qh[l] >> 6) & 3) << 4)) - 32;
                        let s = |k: usize| f64::from(sc[is + k] as i8);
                        out[half * 128 + l] = d * s(0) * f64::from(q1);
                        out[half * 128 + l + 32] = d * s(2) * f64::from(q2);
                        out[half * 128 + l + 64] = d * s(4) * f64::from(q3);
                        out[half * 128 + l + 96] = d * s(6) * f64::from(q4);
                    }
                }
                y.extend(out);
            }
        }
        _ => panic!("type {t} is not a block type"),
    }
    y
}

impl T {
    /// Purpose: This function makes a contiguous tensor of zeros.
    fn zeros(t: i64, ne: [i64; 4]) -> T {
        let nb = strides(t, ne);
        T { bytes: vec![0; (nb[3] * ne[3]) as usize], ne, nb, ty: t }
    }

    /// Purpose: This function makes a tensor of indices, i32 or i64.
    fn indices(t: i64, values: &[i64]) -> T {
        let mut out = T::zeros(t, [values.len() as i64, 1, 1, 1]);
        for (i, v) in values.iter().enumerate() {
            if t == ty::I64 {
                out.bytes[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
            } else {
                out.bytes[i * 4..i * 4 + 4].copy_from_slice(&(*v as i32).to_le_bytes());
            }
        }
        out
    }

    /// Purpose: This function decodes one row of the tensor.
    fn row(&self, i1: i64, i2: i64, i3: i64) -> Vec<f64> {
        let at = (i1 * self.nb[1] + i2 * self.nb[2] + i3 * self.nb[3]) as usize;
        let n = self.ne[0] as usize;
        let b = &self.bytes;
        let (blk, size) = block(self.ty).expect("a known type");
        if blk > 1 {
            return dequant(self.ty, &b[at..at + n / blk as usize * size as usize], n);
        }
        (0..n)
            .map(|i0| {
                let p = at + i0 * self.nb[0] as usize;
                match self.ty {
                    ty::F32 => f64::from(f32::from_le_bytes([b[p], b[p + 1], b[p + 2], b[p + 3]])),
                    ty::F16 => f16(u16_at(b, p)),
                    ty::BF16 => f64::from(f32::from_bits(u32::from(u16_at(b, p)) << 16)),
                    ty::I32 => f64::from(i32::from_le_bytes([b[p], b[p + 1], b[p + 2], b[p + 3]])),
                    _ => panic!("type {} has no float values", self.ty),
                }
            })
            .collect()
    }

    /// Purpose: This function decodes every value, the first dimension
    /// fastest.
    fn values(&self) -> Vec<f64> {
        let mut out = Vec::new();
        for i3 in 0..self.ne[3] {
            for i2 in 0..self.ne[2] {
                for i1 in 0..self.ne[1] {
                    out.extend(self.row(i1, i2, i3));
                }
            }
        }
        out
    }
}

/// Purpose: This struct makes the same random numbers on every run.
struct Rng(u64);

impl Rng {
    /// Purpose: This function gives the next 64 random bits: splitmix64.
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Purpose: This function gives a number from -1 to 1.
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }

    /// Purpose: This function gives a half's bits with an exponent field in
    /// `lo..=hi`, so its size is about `2^(lo - 15)` to `2^(hi - 14)`.
    fn half(&mut self, lo: u64, hi: u64, signed: bool) -> u16 {
        let sign = if signed { (self.next() & 1) << 15 } else { 0 };
        let e = lo + self.next() % (hi - lo + 1);
        (sign | (e << 10) | (self.next() & 1023)) as u16
    }
}

/// Purpose: This function makes a tensor of random values of a type, sized
/// so no sum in a case overflows or loses its meaning.
fn random(rng: &mut Rng, t: i64, ne: [i64; 4]) -> T {
    let mut out = T::zeros(t, ne);
    let b = &mut out.bytes;
    match t {
        ty::F32 => {
            for c in b.chunks_mut(4) {
                c.copy_from_slice(&(rng.unit() as f32).to_le_bytes());
            }
        }
        ty::F16 => {
            for c in b.chunks_mut(2) {
                c.copy_from_slice(&rng.half(12, 15, true).to_le_bytes());
            }
        }
        ty::BF16 => {
            for c in b.chunks_mut(2) {
                let bits =
                    ((rng.next() & 1) << 15) | ((124 + rng.next() % 4) << 7) | (rng.next() & 127);
                c.copy_from_slice(&(bits as u16).to_le_bytes());
            }
        }
        _ => {
            for x in b.iter_mut() {
                *x = rng.next() as u8;
            }
            // Each block's scales: a small positive half, so the values stay
            // near 1.
            let (size, at, lo, hi) = match t {
                ty::Q8_0 => (34, vec![0], 6, 8),
                ty::Q4_0 => (18, vec![0], 10, 12),
                ty::Q4_K => (144, vec![0, 2], 4, 6),
                ty::Q5_K => (176, vec![0, 2], 4, 6),
                ty::Q6_K => (210, vec![208], 2, 4),
                _ => panic!("type {t} is not one a case makes"),
            };
            for blk in b.chunks_mut(size) {
                for &a in &at {
                    blk[a..a + 2].copy_from_slice(&rng.half(lo, hi, false).to_le_bytes());
                }
            }
        }
    }
    out
}

/// Every weight type, with its name.
const WEIGHTS: [(i64, &str); 8] = [
    (ty::F32, "f32"),
    (ty::F16, "f16"),
    (ty::BF16, "bf16"),
    (ty::Q8_0, "q8_0"),
    (ty::Q4_0, "q4_0"),
    (ty::Q4_K, "q4_K"),
    (ty::Q5_K, "q5_K"),
    (ty::Q6_K, "q6_K"),
];

// ── ops ─────────────────────────────────────────────────────────────────────

/// Purpose: This struct holds one op of a case, as the plan would describe it.
struct Op {
    kernel: i64,
    rule: i64,
    width: i64,
    srcs: Vec<Option<T>>,
    dst: T,
    f: [f32; 8],
    ints: [i64; 8],
    /// The step's tokens and the position of its first, which give the cache
    /// cells that hold tokens.
    n: i64,
    pos: i64,
}

impl Op {
    fn new(kernel: i64, rule: i64, width: i64, srcs: Vec<Option<T>>, dst: T) -> Op {
        Op { kernel, rule, width, srcs, dst, f: [0.0; 8], ints: [0; 8], n: 1, pos: 0 }
    }
}

/// Purpose: This function writes an op record as `crate::infer::plan::read`
/// does.
fn record(op: &Op) -> [i64; OP_WORDS] {
    let mut rec = [0i64; OP_WORDS];
    rec[0] = op.kernel;
    let put = |rec: &mut [i64; OP_WORDS], slot: usize, t: &T| {
        let b = 1 + slot * 12;
        rec[b] = 0;
        rec[b + 1] = 0;
        rec[b + 2..b + 6].copy_from_slice(&t.ne);
        rec[b + 6..b + 10].copy_from_slice(&t.nb);
        rec[b + 10] = t.ty;
        rec[b + 11] = -1;
    };
    for slot in 0..5 {
        rec[1 + slot * 12] = -1;
    }
    for (slot, t) in op.srcs.iter().enumerate() {
        if let Some(t) = t {
            put(&mut rec, slot, t);
        }
    }
    put(&mut rec, 4, &op.dst);
    for k in 0..4 {
        let lo = op.f[2 * k].to_bits() as i64;
        let hi = op.f[2 * k + 1].to_bits() as i64;
        rec[61 + k] = lo | (hi << 32);
    }
    rec[65..73].copy_from_slice(&op.ints);
    rec[73] = op.rule;
    rec[74] = op.width;
    rec
}

/// Purpose: This struct holds one launch: the kernel, its argument record,
/// and its blocks.
pub(super) struct Launch {
    pub kernel: &'static str,
    pub args: Vec<u8>,
    pub blocks: u64,
}

fn put_word(args: &mut [u8], at: usize, v: i64) {
    args[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

/// Purpose: This function turns an op record into a launch, the arithmetic
/// the step in `codegen/infer/metal.rs` does for each op.
///
/// Inputs:
/// - `rec`: the record.
/// - `n`: the step's tokens.
/// - `pos`: the position of its first token.
pub(super) fn launch(rec: &[i64; OP_WORDS], n: i64, pos: i64) -> Launch {
    let mut args = vec![0u8; ARGS_BYTES as usize];
    let mut ne = [[0i64; 4]; 5];
    for (slot, shape) in ne.iter_mut().enumerate() {
        let b = 1 + slot * 12;
        let axis = rec[b + 11];
        for (d, size) in shape.iter_mut().enumerate() {
            *size = if axis == d as i64 { n } else { rec[b + 2 + d] };
            put_word(&mut args, slot * 96 + d * 8, *size);
            put_word(&mut args, slot * 96 + 32 + d * 8, rec[b + 6 + d]);
        }
        put_word(&mut args, slot * 96 + 64, rec[b + 10]);
    }
    for k in 0..4 {
        put_word(&mut args, 480 + k * 8, rec[61 + k]);
    }
    for k in 0..8 {
        put_word(&mut args, 512 + k * 8, rec[65 + k]);
    }
    put_word(&mut args, 512 + 6 * 8, pos + n);
    let all = |s: usize| ne[s].iter().product::<i64>();
    let rule = rec[73];
    let tiled = rule == threads::MAT && ne[4][1] > 1;
    let count = match rule {
        _ if tiled => {
            let t = threads::TILE;
            ((ne[4][0] + t - 1) / t) * ((ne[4][1] + t - 1) / t) * ne[4][2] * ne[4][3]
        }
        threads::DST_ROWS => ne[4][1] * ne[4][2] * ne[4][3],
        threads::SRC1 => all(1),
        threads::ATTENTION => ne[0][1] * ne[0][2],
        _ => all(4),
    };
    put_word(&mut args, 512 + 7 * 8, count);
    let per = if tiled { threads::TILE_THREADS } else { rec[74] };
    let kernel = if tiled { MUL_MM } else { rec[0] };
    Launch {
        kernel: KERNELS[kernel as usize],
        args,
        blocks: ((count * per + BLOCK - 1) / BLOCK) as u64,
    }
}

// ── cases ───────────────────────────────────────────────────────────────────

/// Purpose: This struct holds one case: an op, and what its destination
/// should hold, within `tol` of its largest value.
pub(super) struct Case {
    pub name: String,
    op: Op,
    expect: Vec<f64>,
    tol: f64,
}

/// Purpose: This function runs one case.
///
/// Outputs: the largest error, as a fraction of the largest value, or a
/// message naming the first value that is wrong.
fn run_case(r: &mut dyn Runner, c: &Case) -> Result<f64, String> {
    let l = launch(&record(&c.op), c.op.n, c.op.pos);
    let mut bufs: [Vec<u8>; 5] = std::array::from_fn(|i| match i {
        4 => c.op.dst.bytes.clone(),
        _ => c.op.srcs.get(i).and_then(|s| s.as_ref()).map(|t| t.bytes.clone()).unwrap_or_default(),
    });
    r.run(l.kernel, &mut bufs, &l.args, l.blocks)?;
    let out = T { bytes: std::mem::take(&mut bufs[4]), ..c.op.dst.clone() };
    let got = out.values();
    let scale = c.expect.iter().fold(1.0f64, |m, v| m.max(v.abs()));
    let mut worst = (0.0f64, 0usize);
    for (i, (g, w)) in got.iter().zip(&c.expect).enumerate() {
        let d = (g - w).abs();
        if !d.is_finite() {
            return Err(format!("value {i} is {g}, where {w} was expected"));
        }
        if d > worst.0 {
            worst = (d, i);
        }
    }
    let (d, i) = worst;
    if d > c.tol * scale {
        return Err(format!(
            "value {i} is {}, where {} was expected: off by {:.2e} of a largest value {:.2e}",
            got[i], c.expect[i], d, scale
        ));
    }
    Ok(d / scale)
}

/// Purpose: This function runs every case.
///
/// Outputs: a line per case that failed, naming it.
pub(super) fn run_all(r: &mut dyn Runner) -> Vec<String> {
    let mut failures = Vec::new();
    for c in cases() {
        match run_case(r, &c) {
            Ok(err) => println!("{}: {} matches, worst error {err:.1e}", r.name(), c.name),
            Err(e) => failures.push(format!("{}: {e}", c.name)),
        }
    }
    failures
}

/// Purpose: This function gives the dot product of two lists.
fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Purpose: This function makes every case.
pub(super) fn cases() -> Vec<Case> {
    let mut rng = Rng(0x6a61_6465);
    let mut out = Vec::new();
    get_rows(&mut rng, &mut out);
    rms_norm(&mut rng, &mut out);
    binary(&mut rng, &mut out);
    mul_mat(&mut rng, &mut out);
    rope(&mut rng, &mut out);
    cpy(&mut rng, &mut out);
    set_rows(&mut rng, &mut out);
    swiglu(&mut rng, &mut out);
    attention(&mut rng, &mut out);
    out
}

/// Purpose: This function adds `get_rows` from a table of each weight type.
fn get_rows(rng: &mut Rng, out: &mut Vec<Case>) {
    let picks = [7, 0, 9];
    for (t, name) in WEIGHTS {
        let table = random(rng, t, [256, 10, 1, 1]);
        let rows = T::indices(ty::I32, &picks);
        let dst = T::zeros(ty::F32, [256, 3, 1, 1]);
        let expect = picks.iter().flat_map(|&r| table.row(r, 0, 0)).collect();
        let op = Op::new(0, threads::DST, 1, vec![Some(table), Some(rows)], dst);
        out.push(Case { name: format!("get_rows from {name}"), op, expect, tol: 1e-6 });
    }
}

/// Purpose: This function adds `rms_norm` over rows of two lengths, one not a
/// multiple of 32.
fn rms_norm(rng: &mut Rng, out: &mut Vec<Case>) {
    for (n, rows) in [(1000, 3), (4096, 2)] {
        let src = random(rng, ty::F32, [n, rows, 1, 1]);
        let dst = T::zeros(ty::F32, [n, rows, 1, 1]);
        let eps = 1e-6f32;
        let mut expect = Vec::new();
        for r in 0..rows {
            let x = src.row(r, 0, 0);
            let mean = x.iter().map(|v| v * v).sum::<f64>() / n as f64;
            let scale = 1.0 / (mean + f64::from(eps)).sqrt();
            expect.extend(x.iter().map(|v| v * scale));
        }
        let mut op = Op::new(1, threads::DST_ROWS, threads::GROUP, vec![Some(src)], dst);
        op.f[0] = eps;
        out.push(Case { name: format!("rms_norm over rows of {n}"), op, expect, tol: 1e-5 });
    }
}

/// Purpose: This function adds `mul` with `src1` broadcast and `add`.
fn binary(rng: &mut Rng, out: &mut Vec<Case>) {
    let a = random(rng, ty::F32, [64, 5, 1, 1]);
    let b = random(rng, ty::F32, [64, 1, 1, 1]);
    let (av, bv) = (a.values(), b.values());
    let expect = (0..320).map(|i| av[i] * bv[i % 64]).collect();
    let op = Op::new(2, threads::DST, 1, vec![Some(a), Some(b)], T::zeros(ty::F32, [64, 5, 1, 1]));
    out.push(Case { name: "mul, broadcast".into(), op, expect, tol: 1e-6 });

    let a = random(rng, ty::F32, [64, 5, 1, 1]);
    let b = random(rng, ty::F32, [64, 5, 1, 1]);
    let expect = a.values().iter().zip(b.values()).map(|(x, y)| x + y).collect();
    let op = Op::new(3, threads::DST, 1, vec![Some(a), Some(b)], T::zeros(ty::F32, [64, 5, 1, 1]));
    out.push(Case { name: "add".into(), op, expect, tol: 1e-6 });
}

/// Purpose: This function makes one `mul_mat` case, with `src0`'s batch
/// dimension broadcast over `src1`'s.
fn mat_case(name: String, w: T, x: T) -> Case {
    let (m, n, batch) = (w.ne[1], x.ne[1], x.ne[2]);
    let per = batch / w.ne[2];
    let mut expect = Vec::new();
    for h in 0..batch {
        for j in 0..n {
            let xr = x.row(j, h, 0);
            for r in 0..m {
                expect.push(dot(&w.row(r, h / per, 0), &xr));
            }
        }
    }
    let dst = T::zeros(ty::F32, [m, n, batch, 1]);
    let op = Op::new(4, threads::MAT, threads::GROUP, vec![Some(w), Some(x)], dst);
    Case { name, op, expect, tol: 1e-4 }
}

/// Purpose: This function adds `mul_mat` for one token, `k_mul_mat`, and for
/// many, `k_mul_mm`, over every weight type, with tiles cut short at the
/// edges, a row length that is not a multiple of 32, f16 and bf16 on the
/// token side, and a broadcast batch.
fn mul_mat(rng: &mut Rng, out: &mut Vec<Case>) {
    for (t, name) in WEIGHTS {
        for n in [1, 37] {
            let w = random(rng, t, [512, 40, 1, 1]);
            let x = random(rng, ty::F32, [512, n, 1, 1]);
            out.push(mat_case(format!("mul_mat of {name} by {n} tokens"), w, x));
        }
    }
    for n in [1, 5] {
        let w = random(rng, ty::F32, [100, 7, 1, 1]);
        let x = random(rng, ty::F32, [100, n, 1, 1]);
        out.push(mat_case(format!("mul_mat of rows of 100 by {n} tokens"), w, x));
        let w = random(rng, ty::F16, [64, 8, 2, 1]);
        let x = random(rng, ty::F32, [64, n, 4, 1]);
        out.push(mat_case(format!("mul_mat broadcast over heads, {n} tokens"), w, x));
    }
    for n in [1, 37] {
        let w = random(rng, ty::F16, [512, 40, 1, 1]);
        let x = random(rng, ty::F16, [512, n, 1, 1]);
        out.push(mat_case(format!("mul_mat of f16 by f16, {n} tokens"), w, x));
        let w = random(rng, ty::Q4_K, [512, 40, 1, 1]);
        let x = random(rng, ty::BF16, [512, n, 1, 1]);
        out.push(mat_case(format!("mul_mat of q4_K by bf16, {n} tokens"), w, x));
    }
}

/// Purpose: This function adds `rope`, normal and neox, over all of a head
/// and half of it, at positions up to 3001, where the angles reach thousands
/// of radians, and with Llama 3.1's frequency factors.
fn rope(rng: &mut Rng, out: &mut Vec<Case>) {
    let positions = [0i64, 17, 3001];
    let shapes = [
        (0i64, 64i64, false),
        (2, 64, false),
        (0, 32, false),
        (2, 32, false),
        (0, 64, true),
        (2, 64, true),
    ];
    for (mode, n_dims, with_factors) in shapes {
        let src = random(rng, ty::F32, [64, 4, 3, 1]);
        let pos = T::indices(ty::I32, &positions);
        let (base, freq_scale, factor) = (10000f32, 1f32, 1f32);
        let theta_scale = rope_theta_scale(f64::from(base), n_dims);
        // Factors from 1 to 32, as Llama 3.2's file holds.
        let mut factors = T::zeros(ty::F32, [n_dims / 2, 1, 1, 1]);
        for c in factors.bytes.chunks_mut(4) {
            c.copy_from_slice(&(1.0 + 31.0 * rng.unit().abs() as f32).to_le_bytes());
        }
        let fv = factors.values();
        let x = src.values();
        let at = |i0: i64, i1: i64, i2: i64| x[(i0 + 64 * (i1 + 4 * i2)) as usize];
        let mut expect = x.clone();
        for i2 in 0..3 {
            for i1 in 0..4 {
                for pair in 0..n_dims / 2 {
                    let (a, b) = if mode == 2 {
                        (pair, pair + n_dims / 2)
                    } else {
                        (2 * pair, 2 * pair + 1)
                    };
                    let mut theta = f64::from(freq_scale)
                        * positions[i2 as usize] as f64
                        * f64::from(theta_scale).powi(pair as i32);
                    if with_factors {
                        theta /= fv[pair as usize];
                    }
                    let (s, c) = theta.sin_cos();
                    let (s, c) = (s * f64::from(factor), c * f64::from(factor));
                    let (x0, x1) = (at(a, i1, i2), at(b, i1, i2));
                    expect[(a + 64 * (i1 + 4 * i2)) as usize] = x0 * c - x1 * s;
                    expect[(b + 64 * (i1 + 4 * i2)) as usize] = x0 * s + x1 * c;
                }
            }
        }
        let dst = T::zeros(ty::F32, [64, 4, 3, 1]);
        let mut srcs = vec![Some(src), Some(pos)];
        if with_factors {
            srcs.push(Some(factors));
        }
        let mut op = Op::new(5, threads::DST, 1, srcs, dst);
        op.f[..4].copy_from_slice(&[base, freq_scale, factor, theta_scale]);
        op.ints[0] = n_dims;
        op.ints[1] = mode;
        op.ints[2] = i64::from(with_factors);
        let kind = if mode == 2 { "neox" } else { "normal" };
        let extra = if with_factors { ", with frequency factors" } else { "" };
        out.push(Case {
            name: format!("rope, {kind}, {n_dims} of 64 dimensions{extra}"),
            op,
            expect,
            tol: 2e-3,
        });
    }
}

/// Purpose: This function adds `cpy` from a transposed f32 view into f16, and
/// from f16 into f32 of another shape.
fn cpy(rng: &mut Rng, out: &mut Vec<Case>) {
    let base = random(rng, ty::F32, [8, 4, 3, 1]);
    let view =
        T { bytes: base.bytes.clone(), ne: [4, 8, 3, 1], nb: [32, 4, 128, 384], ty: ty::F32 };
    // `view[a, b, c]` is `base[b, a, c]`.
    let mut expect = Vec::new();
    for c in 0..3 {
        for b in 0..8 {
            for a in 0..4 {
                expect.push(base.row(a, c, 0)[b as usize]);
            }
        }
    }
    let op = Op::new(6, threads::DST, 1, vec![Some(view)], T::zeros(ty::F16, [4, 8, 3, 1]));
    out.push(Case { name: "cpy of a transposed view into f16".into(), op, expect, tol: 1e-3 });

    let src = random(rng, ty::F16, [64, 3, 1, 1]);
    let expect = src.values();
    let op = Op::new(6, threads::DST, 1, vec![Some(src)], T::zeros(ty::F32, [192, 1, 1, 1]));
    out.push(Case { name: "cpy of f16 into f32 of another shape".into(), op, expect, tol: 1e-7 });
}

/// Purpose: This function adds `set_rows`: three rows written into an f16
/// cache at i64 indices, the other rows kept.
fn set_rows(rng: &mut Rng, out: &mut Vec<Case>) {
    let cache = random(rng, ty::F16, [64, 10, 1, 1]);
    let rows = random(rng, ty::F32, [64, 3, 1, 1]);
    let at = [2i64, 9, 5];
    let mut expect = cache.values();
    for (r, &to) in at.iter().enumerate() {
        let v = rows.row(r as i64, 0, 0);
        expect[(to * 64) as usize..(to * 64 + 64) as usize].copy_from_slice(&v);
    }
    let idx = T::indices(ty::I64, &at);
    let op = Op::new(7, threads::SRC1, 1, vec![Some(cache.clone()), Some(rows), Some(idx)], cache);
    out.push(Case { name: "set_rows into an f16 cache".into(), op, expect, tol: 1e-3 });
}

/// Purpose: This function adds `swiglu`.
fn swiglu(rng: &mut Rng, out: &mut Vec<Case>) {
    let gate = random(rng, ty::F32, [100, 3, 1, 1]);
    let up = random(rng, ty::F32, [100, 3, 1, 1]);
    let expect =
        gate.values().iter().zip(up.values()).map(|(g, u)| g / (1.0 + (-g).exp()) * u).collect();
    let op =
        Op::new(8, threads::DST, 1, vec![Some(gate), Some(up)], T::zeros(ty::F32, [100, 3, 1, 1]));
    out.push(Case { name: "swiglu".into(), op, expect, tol: 1e-5 });
}

/// Purpose: This function adds `flash_attn_ext` over an f16 cache, with two
/// query heads per cache head, a causal mask, and heads of 64, 80 and 128
/// values; 80 is not a multiple of 32.
fn attention(rng: &mut Rng, out: &mut Vec<Case>) {
    let (tokens, heads, kv_heads, n_ctx, pos) = (3i64, 4i64, 2i64, 48i64, 27i64);
    let cells = pos + tokens;
    for dk in [64i64, 80, 128] {
        let q = random(rng, ty::F32, [dk, tokens, heads, 1]);
        let k = random(rng, ty::F16, [dk, n_ctx, kv_heads, 1]);
        let v = random(rng, ty::F16, [dk, n_ctx, kv_heads, 1]);
        let mut mask = T::zeros(ty::F16, [n_ctx, tokens, 1, 1]);
        for t in 0..tokens {
            for j in 0..n_ctx {
                let bits: u16 = if j <= pos + t { 0 } else { 0xFC00 };
                let at = ((t * n_ctx + j) * 2) as usize;
                mask.bytes[at..at + 2].copy_from_slice(&bits.to_le_bytes());
            }
        }
        let scale = 1.0 / (dk as f32).sqrt();
        let mut expect = vec![0.0; (dk * heads * tokens) as usize];
        for t in 0..tokens {
            for h in 0..heads {
                let hk = h / (heads / kv_heads);
                let qr = q.row(t, h, 0);
                let seen: Vec<i64> = (0..cells.min(n_ctx)).filter(|&j| j <= pos + t).collect();
                let scores: Vec<f64> =
                    seen.iter().map(|&j| dot(&qr, &k.row(j, hk, 0)) * f64::from(scale)).collect();
                let top = scores.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                let weights: Vec<f64> = scores.iter().map(|s| (s - top).exp()).collect();
                let total: f64 = weights.iter().sum();
                for e in 0..dk {
                    let mut acc = 0.0;
                    for (w, &j) in weights.iter().zip(&seen) {
                        acc += w * v.row(j, hk, 0)[e as usize];
                    }
                    expect[(e + dk * (h + heads * t)) as usize] = acc / total;
                }
            }
        }
        let dst = T::zeros(ty::F32, [dk, heads, tokens, 1]);
        let mut op = Op::new(
            9,
            threads::ATTENTION,
            threads::GROUP,
            vec![Some(q), Some(k), Some(v), Some(mask)],
            dst,
        );
        op.f[0] = scale;
        op.n = tokens;
        op.pos = pos;
        out.push(Case { name: format!("flash_attn over heads of {dk}"), op, expect, tol: 1e-4 });
    }
}

// ── the tests ───────────────────────────────────────────────────────────────

/// Every kernel gives what plain math gives, run on this machine's CPU with
/// a thread per GPU thread.
#[test]
fn every_kernel_matches_plain_math_on_the_cpu() {
    let mut cpu = Emulator::new().expect("the kernels compile for this machine");
    let failures = run_all(&mut cpu);
    assert!(
        failures.is_empty(),
        "{} cases failed on the CPU:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The Metal source has a kernel of every name an op record can give.
#[test]
fn the_metal_kernels_have_every_name() {
    for name in KERNELS {
        assert!(
            apple::SOURCE.contains(&format!("kernel void {name}(")),
            "no {name} in kernels.metal"
        );
    }
}

/// The argument record is the size `kernels.metal`'s `Args` is: five tensors
/// of twelve words, eight floats and eight ints.
#[test]
fn the_argument_record_is_metals() {
    assert_eq!(ARGS_BYTES, 5 * 12 * 8 + 8 * 4 + 8 * 8);
}
