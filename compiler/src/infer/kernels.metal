// The GPU kernels a program runs a model's forward step with, on Metal.
//
// One kernel per ggml op the step uses, written plainly: one thread computes one
// output value, or one row where a row has to be read whole. Nothing here is
// tuned. The CUDA and Linux kernels get the tuning first; these exist so a
// program on a Mac runs its prompts at all. `src/infer/README.md` says why.
//
// Every kernel takes the same arguments: up to four source tensors and the
// destination as buffers 0 to 4, each bound at its tensor's byte offset in its
// region, and one `Args` record, buffer 5, describing every tensor's shape,
// strides and element type, plus the op's parameters. Shapes follow ggml: `ne`
// counts elements per dimension, fastest first, and `nb` gives each
// dimension's stride in bytes. A quantized tensor's `nb[0]` is its block size in
// bytes, and its rows are whole blocks.

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
    // Ints the op takes: rope's dimensions and mode, the number of threads.
    long i[8];
};

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

// rms_norm(src0, eps) → dst, one thread per row.
kernel void k_rms_norm(device const uchar * s0 [[buffer(0)]], device uchar * d [[buffer(4)]],
                       constant Args & a [[buffer(5)]], uint gid [[thread_position_in_grid]]) {
    if (long(gid) >= a.i[7]) return;
    long rows = a.dst.ne[1];
    long i1 = gid % rows;
    long rest = gid / rows;
    long i2 = rest % a.dst.ne[2];
    long i3 = rest / a.dst.ne[2];
    long n = a.src[0].ne[0];
    float sum = 0.0f;
    for (long k = 0; k < n; k++) {
        float x = load(s0, a.src[0], k, i1, i2, i3);
        sum += x * x;
    }
    float scale = 1.0f / sqrt(sum / float(n) + a.f[0]);
    for (long k = 0; k < n; k++) {
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

// mul_mat(src0 [K, M], src1 [K, N, ...]) → dst [M, N, ...]: each output is
// the dot product of a row of src0 and a column of src1. src0's batch
// dimensions broadcast over src1's.
kernel void k_mul_mat(device const uchar * s0 [[buffer(0)]], device const uchar * s1 [[buffer(1)]],
                      device uchar * d [[buffer(4)]], constant Args & a [[buffer(5)]],
                      uint gid [[thread_position_in_grid]]) {
    if (long(gid) >= a.i[7]) return;
    long i0, i1, i2, i3;
    unflatten(gid, a.dst.ne, i0, i1, i2, i3);
    long k_n = a.src[0].ne[0];
    long b2 = i2 / (a.dst.ne[2] / a.src[0].ne[2]);
    long b3 = i3 / (a.dst.ne[3] / a.src[0].ne[3]);
    float sum = 0.0f;
    for (long k = 0; k < k_n; k++) {
        sum += load(s0, a.src[0], k, i0, b2, b3) * load(s1, a.src[1], k, i1, i2, i3);
    }
    store(d, a.dst, i0, i1, i2, i3, sum);
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
// mask [KV, T]) → dst [Dv, H, T, 1], with f[0] the scale. One thread per
// query head and token computes the whole softmax over the cache, in one pass.
kernel void k_flash_attn(device const uchar * q [[buffer(0)]], device const uchar * k [[buffer(1)]],
                         device const uchar * v [[buffer(2)]], device const uchar * mask [[buffer(3)]],
                         device uchar * d [[buffer(4)]], constant Args & a [[buffer(5)]],
                         uint gid [[thread_position_in_grid]]) {
    if (long(gid) >= a.i[7]) return;
    long tokens = a.src[0].ne[1];
    long t = gid % tokens;
    long h = gid / tokens;
    long dk = a.src[0].ne[0];
    long dv = a.src[2].ne[0];
    long n_kv = a.src[1].ne[1];
    long hkv = h / (a.src[0].ne[2] / a.src[1].ne[2]);
    float scale = a.f[0];
    float running_max = -INFINITY;
    float denom = 0.0f;
    float acc[256];
    for (long e = 0; e < dv; e++) acc[e] = 0.0f;
    for (long j = 0; j < n_kv; j++) {
        float m = load(mask, a.src[3], j, t, 0, 0);
        if (m == -INFINITY) continue;
        float s = 0.0f;
        for (long e = 0; e < dk; e++) {
            s += load(q, a.src[0], e, t, h, 0) * load(k, a.src[1], e, j, hkv, 0);
        }
        s = s * scale + m;
        float next_max = max(running_max, s);
        float old_scale = exp(running_max - next_max);
        float w = exp(s - next_max);
        denom = denom * old_scale + w;
        for (long e = 0; e < dv; e++) {
            acc[e] = acc[e] * old_scale + w * load(v, a.src[2], e, j, hkv, 0);
        }
        running_max = next_max;
    }
    float inv = denom > 0.0f ? 1.0f / denom : 0.0f;
    for (long e = 0; e < dv; e++) {
        store(d, a.dst, e, h, t, 0, acc[e] * inv);
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
