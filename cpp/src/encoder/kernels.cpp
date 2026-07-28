// The four pieces of arithmetic a BERT layer is made of. Everything else in the
// encoder is bookkeeping around these.
//
// AVX2 is chosen at run time rather than compiled in: the build sets no -march, so
// __AVX2__ is never defined here and an #ifdef would compile the fast path away
// entirely. The target attribute lets one translation unit hold both.

#include <algorithm>
#include <cmath>

#include "encoder/internals.hpp"

#if defined(__x86_64__) || defined(__i386__)
#include <immintrin.h>
#define CML_X86 1
#endif

namespace cml::enc {
namespace {

#ifdef CML_X86
__attribute__((target("avx2,fma"))) float hsum(__m256 v) {
    __m128 lo = _mm_add_ps(_mm256_castps256_ps128(v), _mm256_extractf128_ps(v, 1));
    __m128 hi = _mm_movehl_ps(lo, lo);
    lo = _mm_add_ps(lo, hi);
    hi = _mm_shuffle_ps(lo, lo, 0x1);
    return _mm_cvtss_f32(_mm_add_ss(lo, hi));
}

__attribute__((target("avx2,fma"))) float dot8(const float* a, const float* b, std::size_t k) {
    __m256 acc = _mm256_setzero_ps();
    std::size_t i = 0;
    for (; i + 8 <= k; i += 8) {
        acc = _mm256_fmadd_ps(_mm256_loadu_ps(a + i), _mm256_loadu_ps(b + i), acc);
    }
    float s = hsum(acc);
    for (; i < k; ++i) s += a[i] * b[i];
    return s;
}

// A 2x4 register tile: two rows of A against four columns of B, giving eight
// independent FMA chains. That number is the whole point — an FMA has 4-cycle
// latency and two can issue per cycle, so eight in flight is the minimum that keeps
// both ports busy. The obvious 1x4 version runs at half this machine's peak for that
// reason alone. Six loads feed those eight FMAs, comfortably inside two loads/cycle.
//
// The column loop stays outermost so the four weight rows (6 KB at hidden=384) stay
// in L1 across every row of A: the weights are the big operand and get streamed once.
__attribute__((target("avx2,fma"))) void linear_avx2(const float* A, const float* B,
                                                     const float* bias, float* C, std::size_t M,
                                                     std::size_t N, std::size_t K) {
    const std::size_t n4 = N & ~std::size_t(3);
    for (std::size_t n0 = 0; n0 < n4; n0 += 4) {
        const float* b0 = B + n0 * K;
        const float* b1 = b0 + K;
        const float* b2 = b1 + K;
        const float* b3 = b2 + K;
        std::size_t m = 0;
        for (; m + 2 <= M; m += 2) {
            const float* a0 = A + m * K;
            const float* a1 = a0 + K;
            __m256 c00 = _mm256_setzero_ps();
            __m256 c01 = c00, c02 = c00, c03 = c00;
            __m256 c10 = c00, c11 = c00, c12 = c00, c13 = c00;
            std::size_t k = 0;
            for (; k + 8 <= K; k += 8) {
                const __m256 x0 = _mm256_loadu_ps(a0 + k);
                const __m256 x1 = _mm256_loadu_ps(a1 + k);
                __m256 w = _mm256_loadu_ps(b0 + k);
                c00 = _mm256_fmadd_ps(x0, w, c00);
                c10 = _mm256_fmadd_ps(x1, w, c10);
                w = _mm256_loadu_ps(b1 + k);
                c01 = _mm256_fmadd_ps(x0, w, c01);
                c11 = _mm256_fmadd_ps(x1, w, c11);
                w = _mm256_loadu_ps(b2 + k);
                c02 = _mm256_fmadd_ps(x0, w, c02);
                c12 = _mm256_fmadd_ps(x1, w, c12);
                w = _mm256_loadu_ps(b3 + k);
                c03 = _mm256_fmadd_ps(x0, w, c03);
                c13 = _mm256_fmadd_ps(x1, w, c13);
            }
            float s0[4] = {hsum(c00), hsum(c01), hsum(c02), hsum(c03)};
            float s1[4] = {hsum(c10), hsum(c11), hsum(c12), hsum(c13)};
            for (std::size_t t = k; t < K; ++t) {
                const float* bp[4] = {b0, b1, b2, b3};
                for (std::size_t j = 0; j < 4; ++j) {
                    s0[j] += a0[t] * bp[j][t];
                    s1[j] += a1[t] * bp[j][t];
                }
            }
            float* c0 = C + m * N + n0;
            float* c1 = c0 + N;
            for (std::size_t j = 0; j < 4; ++j) {
                const float bj = bias ? bias[n0 + j] : 0.0F;
                c0[j] = s0[j] + bj;
                c1[j] = s1[j] + bj;
            }
        }
        for (; m < M; ++m) {  // odd last row, one of M — cost is noise
            float* c = C + m * N + n0;
            for (std::size_t j = 0; j < 4; ++j) {
                c[j] = dot8(A + m * K, B + (n0 + j) * K, K) + (bias ? bias[n0 + j] : 0.0F);
            }
        }
    }
    for (std::size_t n = n4; n < N; ++n) {  // N is a multiple of 4 in every BERT shape
        for (std::size_t m = 0; m < M; ++m) {
            C[m * N + n] = dot8(A + m * K, B + n * K, K) + (bias ? bias[n] : 0.0F);
        }
    }
}

// Four keys per pass. A head is 32 floats — four AVX2 vectors — so scoring one key at
// a time spends a horizontal sum on every four FMAs, and the hsum costs more than the
// arithmetic it finishes. Four accumulators amortise it four ways.
__attribute__((target("avx2,fma"))) void scores_avx2(const float* q, const float* keys,
                                                     std::size_t stride, std::size_t count,
                                                     std::size_t n, float scale, float* scores) {
    std::size_t j = 0;
    for (; j + 4 <= count; j += 4) {
        const float* k0 = keys + j * stride;
        const float* k1 = k0 + stride;
        const float* k2 = k1 + stride;
        const float* k3 = k2 + stride;
        __m256 a0 = _mm256_setzero_ps();
        __m256 a1 = a0, a2 = a0, a3 = a0;
        std::size_t e = 0;
        for (; e + 8 <= n; e += 8) {
            const __m256 qv = _mm256_loadu_ps(q + e);
            a0 = _mm256_fmadd_ps(qv, _mm256_loadu_ps(k0 + e), a0);
            a1 = _mm256_fmadd_ps(qv, _mm256_loadu_ps(k1 + e), a1);
            a2 = _mm256_fmadd_ps(qv, _mm256_loadu_ps(k2 + e), a2);
            a3 = _mm256_fmadd_ps(qv, _mm256_loadu_ps(k3 + e), a3);
        }
        float s[4] = {hsum(a0), hsum(a1), hsum(a2), hsum(a3)};
        for (; e < n; ++e) {
            s[0] += q[e] * k0[e];
            s[1] += q[e] * k1[e];
            s[2] += q[e] * k2[e];
            s[3] += q[e] * k3[e];
        }
        for (std::size_t t = 0; t < 4; ++t) scores[j + t] = s[t] * scale;
    }
    for (; j < count; ++j) scores[j] = dot8(q, keys + j * stride, n) * scale;
}

// Four value rows per pass, into two accumulators. The obvious one-row-at-a-time form
// reloads and rewrites `out` for every single token — two loads and a store to do one
// FMA — and chains every add on the last. This does one load and one store per four
// rows, and splits the adds across two chains so neither waits on the other. Measured
// the larger half of attention before the change.
__attribute__((target("avx2,fma"))) void mix_avx2(const float* probs, const float* v,
                                                  std::size_t stride, std::size_t count,
                                                  std::size_t n, float* out) {
    const std::size_t n8 = n & ~std::size_t(7);
    for (std::size_t e = 0; e < n; ++e) out[e] = 0.0F;
    std::size_t j = 0;
    for (; j + 4 <= count; j += 4) {
        const __m256 p0 = _mm256_set1_ps(probs[j]);
        const __m256 p1 = _mm256_set1_ps(probs[j + 1]);
        const __m256 p2 = _mm256_set1_ps(probs[j + 2]);
        const __m256 p3 = _mm256_set1_ps(probs[j + 3]);
        const float* r0 = v + j * stride;
        const float* r1 = r0 + stride;
        const float* r2 = r1 + stride;
        const float* r3 = r2 + stride;
        for (std::size_t e = 0; e < n8; e += 8) {
            __m256 a = _mm256_fmadd_ps(p0, _mm256_loadu_ps(r0 + e), _mm256_loadu_ps(out + e));
            __m256 b = _mm256_mul_ps(p1, _mm256_loadu_ps(r1 + e));
            a = _mm256_fmadd_ps(p2, _mm256_loadu_ps(r2 + e), a);
            b = _mm256_fmadd_ps(p3, _mm256_loadu_ps(r3 + e), b);
            _mm256_storeu_ps(out + e, _mm256_add_ps(a, b));
        }
        for (std::size_t e = n8; e < n; ++e) {
            out[e] += probs[j] * r0[e] + probs[j + 1] * r1[e] + probs[j + 2] * r2[e] +
                      probs[j + 3] * r3[e];
        }
    }
    for (; j < count; ++j) {
        const __m256 p = _mm256_set1_ps(probs[j]);
        const float* row = v + j * stride;
        for (std::size_t e = 0; e < n8; e += 8) {
            _mm256_storeu_ps(out + e, _mm256_fmadd_ps(p, _mm256_loadu_ps(row + e),
                                                      _mm256_loadu_ps(out + e)));
        }
        for (std::size_t e = n8; e < n; ++e) out[e] += probs[j] * row[e];
    }
}
#endif

}  // namespace

bool have_avx2() {
#ifdef CML_X86
    static const bool yes = __builtin_cpu_supports("avx2") && __builtin_cpu_supports("fma");
    return yes;
#else
    return false;
#endif
}

void linear_scalar(const float* A, const float* B, const float* bias, float* C, std::size_t M,
                   std::size_t N, std::size_t K) {
    for (std::size_t n = 0; n < N; ++n) {
        const float* b = B + n * K;
        for (std::size_t m = 0; m < M; ++m) {
            C[m * N + n] = dot(A + m * K, b, K) + (bias ? bias[n] : 0.0F);
        }
    }
}

void linear(const float* A, const float* B, const float* bias, float* C, std::size_t M,
            std::size_t N, std::size_t K) {
#ifdef CML_X86
    if (have_avx2()) {
        linear_avx2(A, B, bias, C, M, N, K);
        return;
    }
#endif
    linear_scalar(A, B, bias, C, M, N, K);
}

void attention_row(const float* q, const float* keys, std::size_t stride, std::size_t count,
                   std::size_t n, float scale, float* scores) {
#ifdef CML_X86
    if (have_avx2()) {
        scores_avx2(q, keys, stride, count, n, scale, scores);
        return;
    }
#endif
    for (std::size_t j = 0; j < count; ++j) scores[j] = dot(q, keys + j * stride, n) * scale;
}

void attention_mix(const float* probs, const float* values, std::size_t stride,
                   std::size_t count, std::size_t n, float* out) {
#ifdef CML_X86
    if (have_avx2()) {
        mix_avx2(probs, values, stride, count, n, out);
        return;
    }
#endif
    for (std::size_t e = 0; e < n; ++e) out[e] = 0.0F;
    for (std::size_t j = 0; j < count; ++j) {
        const float* row = values + j * stride;
        for (std::size_t e = 0; e < n; ++e) out[e] += probs[j] * row[e];
    }
}

void layernorm_row(float* x, std::size_t n, const float* gain, const float* bias, float eps) {
    if (n == 0) return;
    double mean = 0.0;
    for (std::size_t i = 0; i < n; ++i) mean += x[i];
    mean /= static_cast<double>(n);
    double var = 0.0;
    for (std::size_t i = 0; i < n; ++i) {
        const double d = x[i] - mean;
        var += d * d;
    }
    var /= static_cast<double>(n);  // biased, as torch.nn.LayerNorm computes it
    const float scale = static_cast<float>(1.0 / std::sqrt(var + eps));
    for (std::size_t i = 0; i < n; ++i) {
        const float norm = static_cast<float>(x[i] - mean) * scale;
        x[i] = gain ? norm * gain[i] + (bias ? bias[i] : 0.0F) : norm;
    }
}

}  // namespace cml::enc
