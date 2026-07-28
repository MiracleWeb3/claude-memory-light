// The two kernels that are dominated by a transcendental rather than by multiplying:
// softmax (one expf per attention score) and GELU (one erff per FFN element). At
// T=221 that is 7 million expf and 4 million erff calls per row, which measured at
// 39 ms of a 255 ms forward pass — more than the Q/K/V projections put together.
//
// Split from kernels.cpp so the vector exp inlines into both callers; a cross-TU
// call every eight elements would eat most of what the vectorisation buys.

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

// Exact-erf GELU, the activation BERT's "gelu" actually names — not the tanh
// approximation, which differs by ~1e-3 around x=2.
float gelu_one(float x) { return 0.5F * x * (1.0F + std::erf(x * kInvSqrt2)); }

#ifdef CML_X86

// exp for eight floats: range-reduce x = n*ln2 + r, evaluate a degree-5 minimax
// polynomial on r in [-ln2/2, ln2/2], then scale by 2^n built directly into the
// exponent field. Relative error stays under ~1e-7, far inside the 1e-4 the frozen
// output vectors are held to.
__attribute__((target("avx2,fma"))) __m256 exp8(__m256 x) {
    const __m256 hi = _mm256_set1_ps(88.3762626F);
    const __m256 lo = _mm256_set1_ps(-88.3762626F);
    x = _mm256_min_ps(_mm256_max_ps(x, lo), hi);

    const __m256 n = _mm256_round_ps(_mm256_mul_ps(x, _mm256_set1_ps(1.44269504088896341F)),
                                     _MM_FROUND_TO_NEAREST_INT | _MM_FROUND_NO_EXC);
    // ln2 split high/low so n*ln2 is subtracted without losing bits.
    __m256 r = _mm256_fnmadd_ps(n, _mm256_set1_ps(0.693359375F), x);
    r = _mm256_fnmadd_ps(n, _mm256_set1_ps(-2.12194440e-4F), r);

    __m256 p = _mm256_set1_ps(1.9875691500e-4F);
    p = _mm256_fmadd_ps(p, r, _mm256_set1_ps(1.3981999507e-3F));
    p = _mm256_fmadd_ps(p, r, _mm256_set1_ps(8.3334519073e-3F));
    p = _mm256_fmadd_ps(p, r, _mm256_set1_ps(4.1665795894e-2F));
    p = _mm256_fmadd_ps(p, r, _mm256_set1_ps(1.6666665459e-1F));
    p = _mm256_fmadd_ps(p, r, _mm256_set1_ps(5.0000001201e-1F));
    p = _mm256_fmadd_ps(_mm256_mul_ps(p, r), r, _mm256_add_ps(r, _mm256_set1_ps(1.0F)));

    // 2^n by placing n+127 in the exponent field.
    const __m256i e = _mm256_slli_epi32(
        _mm256_add_epi32(_mm256_cvtps_epi32(n), _mm256_set1_epi32(127)), 23);
    return _mm256_mul_ps(p, _mm256_castsi256_ps(e));
}

// erf via Abramowitz & Stegun 7.1.26 (max absolute error 1.5e-7), which needs one
// exp(-x^2) — hence living next to exp8 rather than in kernels.cpp.
__attribute__((target("avx2,fma"))) __m256 erf8(__m256 x) {
    const __m256 sign = _mm256_and_ps(x, _mm256_castsi256_ps(_mm256_set1_epi32(0x80000000)));
    const __m256 ax = _mm256_andnot_ps(_mm256_castsi256_ps(_mm256_set1_epi32(0x80000000)), x);

    const __m256 t = _mm256_div_ps(
        _mm256_set1_ps(1.0F),
        _mm256_fmadd_ps(_mm256_set1_ps(0.3275911F), ax, _mm256_set1_ps(1.0F)));
    __m256 y = _mm256_set1_ps(1.061405429F);
    y = _mm256_fmadd_ps(y, t, _mm256_set1_ps(-1.453152027F));
    y = _mm256_fmadd_ps(y, t, _mm256_set1_ps(1.421413741F));
    y = _mm256_fmadd_ps(y, t, _mm256_set1_ps(-0.284496736F));
    y = _mm256_fmadd_ps(y, t, _mm256_set1_ps(0.254829592F));
    y = _mm256_mul_ps(y, t);

    const __m256 decay = exp8(_mm256_mul_ps(_mm256_sub_ps(_mm256_setzero_ps(), ax), ax));
    const __m256 mag = _mm256_fnmadd_ps(y, decay, _mm256_set1_ps(1.0F));
    return _mm256_or_ps(mag, sign);  // erf is odd
}

__attribute__((target("avx2,fma"))) void exp_into(float* x, std::size_t n, float shift) {
    const __m256 s = _mm256_set1_ps(shift);
    std::size_t i = 0;
    for (; i + 8 <= n; i += 8) {
        _mm256_storeu_ps(x + i, exp8(_mm256_sub_ps(_mm256_loadu_ps(x + i), s)));
    }
    for (; i < n; ++i) x[i] = std::exp(x[i] - shift);
}

__attribute__((target("avx2,fma"))) void gelu_avx2(float* x, std::size_t n) {
    const __m256 half = _mm256_set1_ps(0.5F);
    const __m256 one = _mm256_set1_ps(1.0F);
    const __m256 c = _mm256_set1_ps(kInvSqrt2);
    std::size_t i = 0;
    for (; i + 8 <= n; i += 8) {
        const __m256 v = _mm256_loadu_ps(x + i);
        const __m256 e = _mm256_add_ps(one, erf8(_mm256_mul_ps(v, c)));
        _mm256_storeu_ps(x + i, _mm256_mul_ps(_mm256_mul_ps(half, v), e));
    }
    for (; i < n; ++i) x[i] = gelu_one(x[i]);
}

#endif  // CML_X86

}  // namespace

void softmax_row(float* x, std::size_t n) {
    if (n == 0) return;
    const float peak = *std::max_element(x, x + n);  // shift for range, not for meaning
#ifdef CML_X86
    if (have_avx2()) {
        exp_into(x, n, peak);
    } else
#endif
    {
        for (std::size_t i = 0; i < n; ++i) x[i] = std::exp(x[i] - peak);
    }
    // Summed separately, in double: folding this into the vector pass would leave the
    // row summing to 1 only to about 2e-5, and the check here holds it to 1e-5.
    double sum = 0.0;
    for (std::size_t i = 0; i < n; ++i) sum += x[i];
    const float inv = static_cast<float>(1.0 / (sum > 0.0 ? sum : 1.0));
    for (std::size_t i = 0; i < n; ++i) x[i] *= inv;
}

void gelu(float* x, std::size_t n) {
#ifdef CML_X86
    if (have_avx2()) {
        gelu_avx2(x, n);
        return;
    }
#endif
    for (std::size_t i = 0; i < n; ++i) x[i] = gelu_one(x[i]);
}

}  // namespace cml::enc
