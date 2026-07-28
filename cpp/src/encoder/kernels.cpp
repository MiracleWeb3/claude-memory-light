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

constexpr float kInvSqrt2 = 0.70710678118654752440F;

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

// Four output columns at a time so each loaded activation vector feeds four FMAs,
// with the four weight rows (6 KB at hidden=384) resident in L1 across every row of
// A. The column loop is outermost for exactly that reason: the weights are the big
// operand and must be streamed once, not once per token.
__attribute__((target("avx2,fma"))) void linear_avx2(const float* A, const float* B,
                                                     const float* bias, float* C, std::size_t M,
                                                     std::size_t N, std::size_t K) {
    const std::size_t n4 = N & ~std::size_t(3);
    for (std::size_t n0 = 0; n0 < n4; n0 += 4) {
        const float* b0 = B + n0 * K;
        const float* b1 = b0 + K;
        const float* b2 = b1 + K;
        const float* b3 = b2 + K;
        for (std::size_t m = 0; m < M; ++m) {
            const float* a = A + m * K;
            __m256 a0 = _mm256_setzero_ps();
            __m256 a1 = a0, a2 = a0, a3 = a0;
            std::size_t k = 0;
            for (; k + 8 <= K; k += 8) {
                const __m256 av = _mm256_loadu_ps(a + k);
                a0 = _mm256_fmadd_ps(av, _mm256_loadu_ps(b0 + k), a0);
                a1 = _mm256_fmadd_ps(av, _mm256_loadu_ps(b1 + k), a1);
                a2 = _mm256_fmadd_ps(av, _mm256_loadu_ps(b2 + k), a2);
                a3 = _mm256_fmadd_ps(av, _mm256_loadu_ps(b3 + k), a3);
            }
            float s[4] = {hsum(a0), hsum(a1), hsum(a2), hsum(a3)};
            for (std::size_t t = k; t < K; ++t) {
                const float av = a[t];
                s[0] += av * b0[t];
                s[1] += av * b1[t];
                s[2] += av * b2[t];
                s[3] += av * b3[t];
            }
            float* c = C + m * N + n0;
            for (std::size_t j = 0; j < 4; ++j) c[j] = s[j] + (bias ? bias[n0 + j] : 0.0F);
        }
    }
    for (std::size_t n = n4; n < N; ++n) {  // N is a multiple of 4 in every BERT shape
        for (std::size_t m = 0; m < M; ++m) {
            C[m * N + n] = dot8(A + m * K, B + n * K, K) + (bias ? bias[n] : 0.0F);
        }
    }
}

__attribute__((target("avx2,fma"))) void mix_avx2(const float* probs, const float* v,
                                                  std::size_t stride, std::size_t count,
                                                  std::size_t n, float* out) {
    const std::size_t n8 = n & ~std::size_t(7);
    for (std::size_t e = 0; e < n; ++e) out[e] = 0.0F;
    for (std::size_t j = 0; j < count; ++j) {
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
        for (std::size_t j = 0; j < count; ++j) scores[j] = dot8(q, keys + j * stride, n) * scale;
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

void softmax_row(float* x, std::size_t n) {
    if (n == 0) return;
    const float peak = *std::max_element(x, x + n);  // shift for range, not for meaning
    double sum = 0.0;
    for (std::size_t i = 0; i < n; ++i) {
        x[i] = std::exp(x[i] - peak);
        sum += x[i];
    }
    const float inv = static_cast<float>(1.0 / (sum > 0.0 ? sum : 1.0));
    for (std::size_t i = 0; i < n; ++i) x[i] *= inv;
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

void gelu(float* x, std::size_t n) {
    // The exact erf form, which is what BERT's "gelu" activation means in
    // transformers — not the tanh approximation.
    for (std::size_t i = 0; i < n; ++i) {
        x[i] = 0.5F * x[i] * (1.0F + std::erf(x[i] * kInvSqrt2));
    }
}

}  // namespace cml::enc
