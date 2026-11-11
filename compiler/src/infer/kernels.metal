// The GPU kernels a program runs a model's forward step with, on Metal.
//
// One kernel per ggml op the step uses. The ops that read a whole row for each
// output, `mul_mat`, `rms_norm` and `flash_attn_ext`, give each output one SIMD
// group of 32 threads, which split the row and add up their sums; every other
// op runs one thread per output value. Beyond that nothing is tuned: the CUDA
// and Linux kernels get the tuning first. `src/infer/README.md` says why.
//
// Every kernel takes the same arguments: up to four source tensors and the
// destination as buffers 0 to 4, each bound at its tensor's byte offset in its
// region, and one `Args` record, buffer 5, describing every tensor's shape,
// strides and element type, plus the op's parameters. Shapes follow ggml: `ne`
// counts elements per dimension, fastest first, and `nb` gives each
// dimension's stride in bytes. A quantized tensor's `nb[0]` is its block size in
// bytes, and its rows are whole blocks.
//
// `i[7]` is how many outputs the op computes, and `i[6]` how many KV cache
// cells hold tokens, both set by the step. A kernel that gives each output a
// SIMD group runs 32 threads per output; the threadgroups are 64 wide, so the
// 32 threads of one output are always one SIMD group.

#include <metal_stdlib>
using namespace metal;

// ggml's element types, by the numbers GGUF files use.
constant int T_F32 = 0;
constant int T_F16 = 1;
constant int T_Q4_0 = 2;
constant int T_Q8_0 = 8;
constant int T_Q4_K = 12;
constant int T_Q5_K = 13;
constant int T_Q6_K = 14;
constant int T_I32 = 26;
constant int T_I64 = 27;
constant int T_BF16 = 30;

struct Tensor {
    long ne[4];
    long nb[4];
    long type;
    long pad[3];
};

struct Args {
    Tensor src[4];
    Tensor dst;
    // Floats the op takes: eps, a scale, rope's frequencies.
    float f[8];
    // Ints the op takes: rope's dimensions and mode; the cache cells in use;
    // the number of outputs.
    long i[8];
};

// The threads that share one output in the kernels that read a whole row: one
// SIMD group on Apple GPUs.
constant uint GROUP = 32;

// ── reading one element ─────────────────────────────────────────────────────

// Purpose: This function reads a half from two bytes.
static inline float half_at(device const uchar * p) {
    return float(*(device const half *)p);
}

// Purpose: This function returns one element of a block-quantized row as a
// float.
//
// Inputs:
// - `row`: the row's first byte.
// - `k`: the element's index in the row.
// - `type`: the element type.
static float quant_at(device const uchar * row, long k, int type) {
    if (type == T_Q8_0) {
        device const uchar * b = row + (k / 32) * 34;
        return half_at(b) * float(((device const char *)(b + 2))[k % 32]);
    }
    if (type == T_Q4_0) {
        device const uchar * b = row + (k / 32) * 18;
        int j = k % 32;
        uchar q = b[2 + (j % 16)];
        int v = j < 16 ? (q & 0xF) : (q >> 4);
        return half_at(b) * float(v - 8);
    }
    long blk = k / 256;
    int j = int(k % 256);
    if (type == T_Q4_K || type == T_Q5_K) {
        int size = type == T_Q4_K ? 144 : 176;
        device const uchar * b = row + blk * size;
        float d = half_at(b);
        float dmin = half_at(b + 2);
        device const uchar * sc = b + 4;
        // Each 64 values share a pair of 6-bit scales and mins.
        int group = j / 64;
        int sub = (j % 64) / 32;
        int is = group * 2 + sub;
        int s, m;
        if (is < 4) {
            s = sc[is] & 63;
            m = sc[is + 4] & 63;
        } else {
            s = (sc[is + 4] & 0xF) | ((sc[is - 4] >> 6) << 4);
            m = (sc[is + 4] >> 4) | ((sc[is] >> 6) << 4);
        }
        int l = j % 32;
        int q;
        if (type == T_Q4_K) {
            uchar byte = b[16 + group * 32 + l];
            q = sub == 0 ? (byte & 0xF) : (byte >> 4);
        } else {
            uchar byte = b[48 + group * 32 + l];
            int low = sub == 0 ? (byte & 0xF) : (byte >> 4);
            uchar hb = b[16 + l];
            int bit = (hb >> (group * 2 + sub)) & 1;
            q = low + (bit ? 16 : 0);
        }
        return d * float(s) * float(q) - dmin * float(m);
    }
    if (type == T_Q6_K) {
        device const uchar * b = row + blk * 210;
        device const uchar * ql = b;
        device const uchar * qh = b + 128;
        device const char * scales = (device const char *)(b + 192);
        float d = half_at(b + 208);
        int half_n = j / 128;
        int r = j % 128;
        int quarter = r / 32;
        int l = r % 32;
        ql += half_n * 64;
        qh += half_n * 32;
        int s_base = half_n * 8 + l / 16;
        int q;
        if (quarter == 0) {
            q = (ql[l] & 0xF) | (((qh[l] >> 0) & 3) << 4);
        } else if (quarter == 1) {
            q = (ql[l + 32] & 0xF) | (((qh[l] >> 2) & 3) << 4);
        } else if (quarter == 2) {
            q = (ql[l] >> 4) | (((qh[l] >> 4) & 3) << 4);
        } else {
            q = (ql[l + 32] >> 4) | (((qh[l] >> 6) & 3) << 4);
        }
        return d * float(scales[s_base + quarter * 2]) * float(q - 32);
    }
    return 0.0f;
}

// Purpose: This function returns one element of a tensor as a float.
//
// Inputs:
// - `base`: the tensor's first byte.
// - `t`: its shape and type.
// - `i0`..`i3`: the element's index in each dimension.
static float load(device const uchar * base, constant Tensor & t, long i0, long i1, long i2, long i3) {
    int type = int(t.type);
    device const uchar * row = base + i1 * t.nb[1] + i2 * t.nb[2] + i3 * t.nb[3];
    if (type == T_F32) {
        return *(device const float *)(row + i0 * t.nb[0]);
    }
    if (type == T_F16) {
        return half_at(row + i0 * t.nb[0]);
    }
    if (type == T_BF16) {
        uint bits = uint(*(device const ushort *)(row + i0 * t.nb[0])) << 16;
        return as_type<float>(bits);
    }
    if (type == T_I32) {
        return float(*(device const int *)(row + i0 * t.nb[0]));
    }
    return quant_at(row, i0, type);
}

// Purpose: This function writes one float into an f32, f16 or bf16 tensor.
static void store(device uchar * base, constant Tensor & t, long i0, long i1, long i2, long i3, float v) {
    device uchar * p = base + i0 * t.nb[0] + i1 * t.nb[1] + i2 * t.nb[2] + i3 * t.nb[3];
    int type = int(t.type);
    if (type == T_F16) {
        *(device half *)p = half(v);
    } else if (type == T_BF16) {
        *(device ushort *)p = ushort(as_type<uint>(v) >> 16);
    } else {
        *(device float *)p = v;
    }
}

// Purpose: This function reads an integer index, i32 or i64.
static long index_at(device const uchar * base, constant Tensor & t, long i0, long i1) {
    device const uchar * p = base + i0 * t.nb[0] + i1 * t.nb[1];
    if (int(t.type) == T_I64) {
        return *(device const long *)p;
    }
    return long(*(device const int *)p);
}

// Purpose: This function splits a flat thread index into four dimensions.
static void unflatten(long g, constant long * ne, thread long & i0, thread long & i1, thread long & i2, thread long & i3) {
    i0 = g % ne[0];
    g /= ne[0];
    i1 = g % ne[1];
    g /= ne[1];
    i2 = g % ne[2];
    i3 = g / ne[2];
}

// ── the ops ─────────────────────────────────────────────────────────────────

// get_rows(src0, rows) → dst: dst[:, r] = src0[:, rows[r]].
kernel void k_get_rows(device const uchar * s0 [[buffer(0)]], device const uchar * s1 [[buffer(1)]],
                       device uchar * d [[buffer(4)]], constant Args & a [[buffer(5)]],
                       uint gid [[thread_position_in_grid]]) {
    if (long(gid) >= a.i[7]) return;
    long i0, i1, i2, i3;
    unflatten(gid, a.dst.ne, i0, i1, i2, i3);
    long r = index_at(s1, a.src[1], i1, i2);
    store(d, a.dst, i0, i1, i2, i3, load(s0, a.src[0], i0, r, i2, i3));
}

// rms_norm(src0, eps) → dst, one SIMD group per row: each thread takes every
// 32nd value.
kernel void k_rms_norm(device const uchar * s0 [[buffer(0)]], device uchar * d [[buffer(4)]],
                       constant Args & a [[buffer(5)]], uint gid [[thread_position_in_grid]],
                       uint lane [[thread_index_in_simdgroup]]) {
    long row = gid / GROUP;
    if (row >= a.i[7]) return;
    long rows = a.dst.ne[1];
    long i1 = row % rows;
    long rest = row / rows;
    long i2 = rest % a.dst.ne[2];
    long i3 = rest / a.dst.ne[2];
    long n = a.src[0].ne[0];
    float sum = 0.0f;
    for (long k = lane; k < n; k += GROUP) {
        float x = load(s0, a.src[0], k, i1, i2, i3);
        sum += x * x;
    }
    sum = simd_sum(sum);
    float scale = 1.0f / sqrt(sum / float(n) + a.f[0]);
    for (long k = lane; k < n; k += GROUP) {
        store(d, a.dst, k, i1, i2, i3, load(s0, a.src[0], k, i1, i2, i3) * scale);
    }
}

// Purpose: This function reads src1 broadcast over src0's shape, as ggml's
// binary ops do: each of src1's dimensions divides src0's.
static float broadcast(device const uchar * s1, constant Tensor & t, long i0, long i1, long i2, long i3) {
    return load(s1, t, i0 % t.ne[0], i1 % t.ne[1], i2 % t.ne[2], i3 % t.ne[3]);
}

// mul(src0, src1) → dst, src1 broadcast.
kernel void k_mul(device const uchar * s0 [[buffer(0)]], device const uchar * s1 [[buffer(1)]],
                  device uchar * d [[buffer(4)]], constant Args & a [[buffer(5)]],
                  uint gid [[thread_position_in_grid]]) {
    if (long(gid) >= a.i[7]) return;
    long i0, i1, i2, i3;
    unflatten(gid, a.dst.ne, i0, i1, i2, i3);
    float x = load(s0, a.src[0], i0, i1, i2, i3) * broadcast(s1, a.src[1], i0, i1, i2, i3);
    store(d, a.dst, i0, i1, i2, i3, x);
}

// add(src0, src1) → dst, src1 broadcast.
kernel void k_add(device const uchar * s0 [[buffer(0)]], device const uchar * s1 [[buffer(1)]],
                  device uchar * d [[buffer(4)]], constant Args & a [[buffer(5)]],
                  uint gid [[thread_position_in_grid]]) {
    if (long(gid) >= a.i[7]) return;
    long i0, i1, i2, i3;
    unflatten(gid, a.dst.ne, i0, i1, i2, i3);
    float x = load(s0, a.src[0], i0, i1, i2, i3) + broadcast(s1, a.src[1], i0, i1, i2, i3);
    store(d, a.dst, i0, i1, i2, i3, x);
}

// Purpose: This function reads value `k` of an f32, f16 or bf16 row.
//
// Inputs:
// - `row`: the row's first byte.
// - `nb0`: the stride between values, in bytes.
// - `type`: the element type.
// - `k`: the value's index in the row.
static inline float float_at(device const uchar * row, long nb0, int type, long k) {
    device const uchar * p = row + k * nb0;
    if (type == T_F16) {
        return half_at(p);
    }
    if (type == T_BF16) {
        return as_type<float>(uint(*(device const ushort *)p) << 16);
    }
    return *(device const float *)p;
}

// Purpose: This function returns the dot product of 32 values of a weight row
// with the same 32 values of a float row, decoding the weights' scales once
// for all 32.
//
// Inputs:
// - `w`: the weight row's first byte.
// - `wt`: the weights' element type.
// - `wnb`: the stride between the weights, in bytes, for f32, f16 and bf16.
// - `x`: the float row's first byte.
// - `xnb`: the stride between the float row's values, in bytes.
// - `xt`: the float row's element type: f32, f16 or bf16.
// - `k0`: the first of the 32 values, a multiple of 32.
// - `len`: how many of the 32 to take; fewer only at the end of an f32, f16
//   or bf16 weight row whose length is not a multiple of 32.
static float dot32(device const uchar * w, int wt, long wnb, device const uchar * x, long xnb, int xt,
                   long k0, int len) {
    float sum = 0.0f;
    if (wt == T_F32 || wt == T_F16 || wt == T_BF16) {
        for (int l = 0; l < len; l++) {
            sum += float_at(w, wnb, wt, k0 + l) * float_at(x, xnb, xt, k0 + l);
        }
        return sum;
    }
    if (wt == T_Q8_0) {
        device const uchar * b = w + (k0 / 32) * 34;
        device const char * q = (device const char *)(b + 2);
        for (int l = 0; l < 32; l++) {
            sum += float(q[l]) * float_at(x, xnb, xt, k0 + l);
        }
        return half_at(b) * sum;
    }
    if (wt == T_Q4_0) {
        device const uchar * b = w + (k0 / 32) * 18;
        for (int l = 0; l < 16; l++) {
            uchar q = b[2 + l];
            sum += float(int(q & 0xF) - 8) * float_at(x, xnb, xt, k0 + l);
            sum += float(int(q >> 4) - 8) * float_at(x, xnb, xt, k0 + l + 16);
        }
        return half_at(b) * sum;
    }
    // The K types: 256 values a block, and 32 values share a scale.
    long blk = k0 / 256;
    int is = int((k0 % 256) / 32);
    if (wt == T_Q4_K || wt == T_Q5_K) {
        int size = wt == T_Q4_K ? 144 : 176;
        device const uchar * b = w + blk * size;
        device const uchar * sc = b + 4;
        int s, m;
        if (is < 4) {
            s = sc[is] & 63;
            m = sc[is + 4] & 63;
        } else {
            s = (sc[is + 4] & 0xF) | ((sc[is - 4] >> 6) << 4);
            m = (sc[is + 4] >> 4) | ((sc[is] >> 6) << 4);
        }
        // Each value is d * s * q - dmin * m, so the 32 share both terms.
        int group = is / 2;
        int shift = (is % 2) * 4;
        float qx = 0.0f;
        float xs = 0.0f;
        if (wt == T_Q4_K) {
            device const uchar * qs = b + 16 + group * 32;
            for (int l = 0; l < 32; l++) {
                float xv = float_at(x, xnb, xt, k0 + l);
                qx += float((qs[l] >> shift) & 0xF) * xv;
                xs += xv;
            }
        } else {
            device const uchar * qs = b + 48 + group * 32;
            device const uchar * qh = b + 16;
            for (int l = 0; l < 32; l++) {
                float xv = float_at(x, xnb, xt, k0 + l);
                int q = ((qs[l] >> shift) & 0xF) | (((qh[l] >> is) & 1) << 4);
                qx += float(q) * xv;
                xs += xv;
            }
        }
        return half_at(b) * float(s) * qx - half_at(b + 2) * float(m) * xs;
    }
    if (wt == T_Q6_K) {
        device const uchar * b = w + blk * 210;
        int half_n = is / 4;
        int quarter = is % 4;
        device const uchar * ql = b + half_n * 64 + (quarter % 2) * 32;
        device const uchar * qh = b + 128 + half_n * 32;
        device const char * scales = (device const char *)(b + 192) + half_n * 8 + quarter * 2;
        int lo_shift = (quarter / 2) * 4;
        int hi_shift = quarter * 2;
        // The first 16 values have one scale and the last 16 the next.
        float first = 0.0f;
        float second = 0.0f;
        for (int l = 0; l < 32; l++) {
            int q = ((ql[l] >> lo_shift) & 0xF) | (((qh[l] >> hi_shift) & 3) << 4);
            float v = float(q - 32) * float_at(x, xnb, xt, k0 + l);
            if (l < 16) {
                first += v;
            } else {
                second += v;
            }
        }
        return half_at(b + 208) * (float(scales[0]) * first + float(scales[1]) * second);
    }
    return 0.0f;
}

// mul_mat(src0 [K, M], src1 [K, N, ...]) → dst [M, N, ...]: each output is
// the dot product of a row of src0 and a column of src1. src0's batch
// dimensions broadcast over src1's. One SIMD group per output: thread `lane`
// takes the row's 32-value chunks `lane`, `lane + 32` and so on, so the group
// reads the row front to back together.
kernel void k_mul_mat(device const uchar * s0 [[buffer(0)]], device const uchar * s1 [[buffer(1)]],
                      device uchar * d [[buffer(4)]], constant Args & a [[buffer(5)]],
                      uint gid [[thread_position_in_grid]], uint lane [[thread_index_in_simdgroup]]) {
    long out = gid / GROUP;
    if (out >= a.i[7]) return;
    long i0, i1, i2, i3;
    unflatten(out, a.dst.ne, i0, i1, i2, i3);
    long k_n = a.src[0].ne[0];
    long b2 = i2 / (a.dst.ne[2] / a.src[0].ne[2]);
    long b3 = i3 / (a.dst.ne[3] / a.src[0].ne[3]);
    device const uchar * w = s0 + i0 * a.src[0].nb[1] + b2 * a.src[0].nb[2] + b3 * a.src[0].nb[3];
    device const uchar * x = s1 + i1 * a.src[1].nb[1] + i2 * a.src[1].nb[2] + i3 * a.src[1].nb[3];
    int wt = int(a.src[0].type);
    int xt = int(a.src[1].type);
    long wnb = a.src[0].nb[0];
    long xnb = a.src[1].nb[0];
    float sum = 0.0f;
    for (long k0 = long(lane) * 32; k0 < k_n; k0 += GROUP * 32) {
        int len = k_n - k0 < 32 ? int(k_n - k0) : 32;
        sum += dot32(w, wt, wnb, x, xnb, xt, k0, len);
    }
    sum = simd_sum(sum);
    if (lane == 0) {
        store(d, a.dst, i0, i1, i2, i3, sum);
    }
}

// rope(src0, pos) → dst. f[0] freq_base, f[1] freq_scale, f[2] attn_factor;
// i[0] n_dims, i[1] mode: 0 normal, 2 neox. The position of token i2 is
// pos[i2]. One thread per pair of rotated values, and one per value past
// n_dims, which is copied.
kernel void k_rope(device const uchar * s0 [[buffer(0)]], device const uchar * s1 [[buffer(1)]],
                   device uchar * d [[buffer(4)]], constant Args & a [[buffer(5)]],
                   uint gid [[thread_position_in_grid]]) {
    if (long(gid) >= a.i[7]) return;
    long i0, i1, i2, i3;
    unflatten(gid, a.dst.ne, i0, i1, i2, i3);
    long n_dims = a.i[0];
    bool neox = a.i[1] == 2;
    if (i0 >= n_dims) {
        store(d, a.dst, i0, i1, i2, i3, load(s0, a.src[0], i0, i1, i2, i3));
        return;
    }
    // Each thread handles the first value of its pair.
    long half_dims = n_dims / 2;
    long pair;
    long first, second;
    if (neox) {
        if (i0 >= half_dims) return;
        pair = i0;
        first = i0;
        second = i0 + half_dims;
    } else {
        if (i0 % 2 == 1) return;
        pair = i0 / 2;
        first = i0;
        second = i0 + 1;
    }
    float pos = float(index_at(s1, a.src[1], i2, 0));
    float theta = a.f[1] * pos * pow(a.f[0], -2.0f * float(pair) / float(n_dims));
    float c = cos(theta) * a.f[2];
    float s = sin(theta) * a.f[2];
    float x0 = load(s0, a.src[0], first, i1, i2, i3);
    float x1 = load(s0, a.src[0], second, i1, i2, i3);
    store(d, a.dst, first, i1, i2, i3, x0 * c - x1 * s);
    store(d, a.dst, second, i1, i2, i3, x0 * s + x1 * c);
}

// cpy and cont: dst = src0, converting the type, both walked in element order.
kernel void k_cpy(device const uchar * s0 [[buffer(0)]], device uchar * d [[buffer(4)]],
                  constant Args & a [[buffer(5)]], uint gid [[thread_position_in_grid]]) {
    if (long(gid) >= a.i[7]) return;
    long i0, i1, i2, i3;
    unflatten(gid, a.dst.ne, i0, i1, i2, i3);
    long s0n, s1n, s2n, s3n;
    unflatten(gid, a.src[0].ne, s0n, s1n, s2n, s3n);
    store(d, a.dst, i0, i1, i2, i3, load(s0, a.src[0], s0n, s1n, s2n, s3n));
}

// set_rows(dst, src1, rows): dst[:, rows[r]] = src1[:, r]. src0 is the
// destination itself, bound as buffer 4 too.
kernel void k_set_rows(device const uchar * s1 [[buffer(1)]], device const uchar * s2 [[buffer(2)]],
                       device uchar * d [[buffer(4)]], constant Args & a [[buffer(5)]],
                       uint gid [[thread_position_in_grid]]) {
    if (long(gid) >= a.i[7]) return;
    long i0, i1, i2, i3;
    unflatten(gid, a.src[1].ne, i0, i1, i2, i3);
    long r = index_at(s2, a.src[2], i1, i2);
    store(d, a.dst, i0, r, i2, i3, load(s1, a.src[1], i0, i1, i2, i3));
}

// swiglu(gate, up) → dst = silu(gate) * up.
kernel void k_swiglu(device const uchar * s0 [[buffer(0)]], device const uchar * s1 [[buffer(1)]],
                     device uchar * d [[buffer(4)]], constant Args & a [[buffer(5)]],
                     uint gid [[thread_position_in_grid]]) {
    if (long(gid) >= a.i[7]) return;
    long i0, i1, i2, i3;
    unflatten(gid, a.dst.ne, i0, i1, i2, i3);
    float g = load(s0, a.src[0], i0, i1, i2, i3);
    float u = load(s1, a.src[1], i0, i1, i2, i3);
    store(d, a.dst, i0, i1, i2, i3, g / (1.0f + exp(-g)) * u);
}

// flash_attn_ext(q [D, T, H, 1], k [D, KV, Hkv, 1], v [Dv, KV, Hkv, 1],
// mask [KV, T]) → dst [Dv, H, T, 1], with f[0] the scale. One SIMD group per
// query head and token computes the whole softmax over the cache, in one pass.
// Thread `lane` holds values `lane`, `lane + 32` and so on of the query and of
// the output, up to eight each, since a head is at most 256 wide. Only the
// first `i[6]` cells hold tokens; the mask hides every cell past them.
kernel void k_flash_attn(device const uchar * q [[buffer(0)]], device const uchar * k [[buffer(1)]],
                         device const uchar * v [[buffer(2)]], device const uchar * mask [[buffer(3)]],
                         device uchar * d [[buffer(4)]], constant Args & a [[buffer(5)]],
                         uint gid [[thread_position_in_grid]], uint lane [[thread_index_in_simdgroup]]) {
    long item = gid / GROUP;
    if (item >= a.i[7]) return;
    long tokens = a.src[0].ne[1];
    long t = item % tokens;
    long h = item / tokens;
    long dk = a.src[0].ne[0];
    long dv = a.src[2].ne[0];
    long n_kv = a.src[1].ne[1] < a.i[6] ? a.src[1].ne[1] : a.i[6];
    long hkv = h / (a.src[0].ne[2] / a.src[1].ne[2]);
    float scale = a.f[0];
    float qv[8];
    float acc[8];
    for (int m = 0; m < 8; m++) {
        long e = long(lane) + m * GROUP;
        qv[m] = e < dk ? load(q, a.src[0], e, t, h, 0) : 0.0f;
        acc[m] = 0.0f;
    }
    float running_max = -INFINITY;
    float denom = 0.0f;
    for (long j = 0; j < n_kv; j++) {
        float mk = load(mask, a.src[3], j, t, 0, 0);
        if (mk == -INFINITY) continue;
        float s = 0.0f;
        for (int m = 0; m < 8; m++) {
            long e = long(lane) + m * GROUP;
            if (e < dk) {
                s += qv[m] * load(k, a.src[1], e, j, hkv, 0);
            }
        }
        s = simd_sum(s) * scale + mk;
        float next_max = max(running_max, s);
        float old_scale = exp(running_max - next_max);
        float w = exp(s - next_max);
        denom = denom * old_scale + w;
        for (int m = 0; m < 8; m++) {
            long e = long(lane) + m * GROUP;
            if (e < dv) {
                acc[m] = acc[m] * old_scale + w * load(v, a.src[2], e, j, hkv, 0);
            }
        }
        running_max = next_max;
    }
    float inv = denom > 0.0f ? 1.0f / denom : 0.0f;
    for (int m = 0; m < 8; m++) {
        long e = long(lane) + m * GROUP;
        if (e < dv) {
            store(d, a.dst, e, h, t, 0, acc[m] * inv);
        }
    }
}

// argmax over the last row of logits, the token a greedy step picks. One
// thread; the vocabulary is read once per generated token.
kernel void k_argmax(device const uchar * s0 [[buffer(0)]], device uchar * d [[buffer(4)]],
                     constant Args & a [[buffer(5)]], uint gid [[thread_position_in_grid]]) {
    if (gid != 0) return;
    long n = a.src[0].ne[0];
    long row = a.i[0];
    float best = -INFINITY;
    long at = 0;
    for (long i = 0; i < n; i++) {
        float x = load(s0, a.src[0], i, row, 0, 0);
        if (x > best) {
            best = x;
            at = i;
        }
    }
    *(device long *)d = at;
}
