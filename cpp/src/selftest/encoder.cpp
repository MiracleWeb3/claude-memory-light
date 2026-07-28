// The transformer encoder. There is no PyTorch here to diff against, so the
// arithmetic is pinned structurally instead: each kernel is checked against the
// property that defines it, the vectorised path against the scalar one, and the
// safetensors reader against a file built byte by byte in this test.

#include <algorithm>
#include <array>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <string>
#include <vector>

#include "encoder/internals.hpp"
#include "harness.hpp"

namespace cml_test {

// Defined in encodermodel.cpp: everything that needs the downloaded weights.
void suite_encoder_model();

namespace {

using namespace cml::enc;

// Deterministic pseudo-random floats in [-1, 1): the AVX2/scalar comparison must
// fail the same way on every machine, which rules out <random>'s distributions.
struct Rng {
    std::uint32_t s = 0x9E3779B9U;
    float next() {
        s = s * 1664525U + 1013904223U;
        return static_cast<float>(s >> 8) / 8388608.0F - 1.0F;
    }
};

void test_softmax() {
    std::vector<float> x = {1.0F, 2.0F, 3.0F, -4.0F, 0.5F};
    softmax_row(x.data(), x.size());
    double sum = 0.0;
    for (const float v : x) {
        sum += v;
        ok(v > 0.0F && v < 1.0F, "softmax outputs are probabilities");
    }
    ok(std::abs(sum - 1.0) < 1e-5, "softmax row sums to 1");
    ok(x[2] > x[1] && x[1] > x[0] && x[3] < x[0], "softmax preserves order");

    // Shift invariance is the property the max-subtraction relies on; if it did not
    // hold, the overflow guard would be changing the answer.
    std::vector<float> y = {1.0F + 40.0F, 2.0F + 40.0F, 3.0F + 40.0F, -4.0F + 40.0F, 0.5F + 40.0F};
    softmax_row(y.data(), y.size());
    for (std::size_t i = 0; i < x.size(); ++i) {
        ok(std::abs(x[i] - y[i]) < 1e-6F, "softmax is shift-invariant");
    }

    std::vector<float> flat(8, 7.0F);
    softmax_row(flat.data(), flat.size());
    ok(std::abs(flat[0] - 0.125F) < 1e-6F, "equal logits give a uniform distribution");
}

void test_layernorm() {
    Rng rng;
    std::vector<float> x(384);
    for (float& v : x) v = rng.next() * 5.0F + 2.0F;

    std::vector<float> plain = x;
    layernorm_row(plain.data(), plain.size(), nullptr, nullptr, 1e-12F);
    double mean = 0.0;
    for (const float v : plain) mean += v;
    mean /= static_cast<double>(plain.size());
    double var = 0.0;
    for (const float v : plain) var += (v - mean) * (v - mean);
    var /= static_cast<double>(plain.size());
    ok(std::abs(mean) < 1e-5, "layernorm centres the row");
    ok(std::abs(var - 1.0) < 1e-4, "layernorm scales the row to unit variance");

    // gain and bias must land the row on N(bias, gain^2), or the learned scales are
    // being applied in the wrong order.
    const std::vector<float> gain(384, 2.0F), bias(384, 3.0F);
    std::vector<float> scaled = x;
    layernorm_row(scaled.data(), scaled.size(), gain.data(), bias.data(), 1e-12F);
    double m2 = 0.0;
    for (const float v : scaled) m2 += v;
    m2 /= static_cast<double>(scaled.size());
    ok(std::abs(m2 - 3.0) < 1e-4, "gain and bias shift the mean to the bias");
    ok(std::abs(scaled[0] - (plain[0] * 2.0F + 3.0F)) < 1e-4F, "gain scales, bias offsets");
}

void test_gelu() {
    std::vector<float> x = {0.0F, 1.0F, -1.0F, 3.0F, -3.0F};
    gelu(x.data(), x.size());
    ok(std::abs(x[0]) < 1e-6F, "gelu(0) = 0");
    ok(std::abs(x[1] - 0.8413447F) < 1e-5F, "gelu(1) = 0.8413");
    ok(std::abs(x[2] + 0.1586553F) < 1e-5F, "gelu(-1) = -0.1587");
    ok(std::abs(x[3] - 2.9959507F) < 1e-5F, "gelu is nearly the identity at 3");
    ok(std::abs(x[4] + 0.0040493F) < 1e-5F, "gelu(-3) is small and negative");
    // The tanh approximation differs from the erf form by ~1e-3 around x=2, which is
    // the size of error these bounds are here to catch.
}

void test_linear() {
    // A[2,3] . B[2,3]^T + bias, worked out by hand.
    const float a[6] = {1, 2, 3, 4, 5, 6};
    const float b[6] = {1, 0, 1, 2, 1, 0};
    const float bias[2] = {10, 20};
    float c[4] = {0, 0, 0, 0};
    linear(a, b, bias, c, 2, 2, 3);
    ok(c[0] == 14.0F && c[1] == 24.0F, "first row: 4+10, 4+20");
    ok(c[2] == 20.0F && c[3] == 33.0F, "second row: 10+10, 13+20");

    linear(a, b, nullptr, c, 2, 2, 3);
    ok(c[0] == 4.0F && c[3] == 13.0F, "a null bias adds nothing");

    if (!have_avx2()) {
        std::fprintf(stderr, "  (no AVX2 on this CPU: linear() is the scalar path)\n");
    }
    // Shapes chosen to leave remainders in both loops: N=13 is not a multiple of 4,
    // K=37 is not a multiple of 8. Those tails are where a hand-written kernel breaks.
    Rng rng;
    for (const auto [m, n, k] : {std::array<std::size_t, 3>{5, 12, 40},
                                 std::array<std::size_t, 3>{9, 13, 37},
                                 std::array<std::size_t, 3>{64, 384, 384}}) {
        std::vector<float> A(m * k), B(n * k), bs(n), fast(m * n), slow(m * n);
        for (float& v : A) v = rng.next();
        for (float& v : B) v = rng.next();
        for (float& v : bs) v = rng.next();
        linear(A.data(), B.data(), bs.data(), fast.data(), m, n, k);
        linear_scalar(A.data(), B.data(), bs.data(), slow.data(), m, n, k);
        float worst = 0.0F;
        for (std::size_t i = 0; i < fast.size(); ++i) {
            worst = std::max(worst, std::abs(fast[i] - slow[i]));
        }
        ok(worst < 1e-4F, "the vectorised gemm matches the scalar one");
    }
}

// attention_row and attention_mix against a reference written out longhand here.
// Whichever path the CPU picks is the one measured, so the scalar branches are
// covered on the machines that take them rather than only on x86.
void test_attention_kernels() {
    constexpr std::size_t kTokens = 5, kStride = 96, kHead = 32, kOff = 32;
    Rng rng;
    std::vector<float> q(kHead), kv(kTokens * kStride), vv(kTokens * kStride);
    for (float& x : q) x = rng.next();
    for (float& x : kv) x = rng.next();
    for (float& x : vv) x = rng.next();

    const float scale = 0.176776695F;  // 1/sqrt(32)
    std::vector<float> scores(kTokens);
    attention_row(q.data(), kv.data() + kOff, kStride, kTokens, kHead, scale, scores.data());
    for (std::size_t j = 0; j < kTokens; ++j) {
        float want = 0.0F;
        for (std::size_t e = 0; e < kHead; ++e) want += q[e] * kv[j * kStride + kOff + e];
        ok(std::abs(scores[j] - want * scale) < 1e-5F, "attention scores are scaled dots");
    }

    std::vector<float> probs = {0.1F, 0.2F, 0.3F, 0.15F, 0.25F};
    std::vector<float> out(kHead, 99.0F);  // must be overwritten, not accumulated into
    attention_mix(probs.data(), vv.data() + kOff, kStride, kTokens, kHead, out.data());
    for (std::size_t e = 0; e < kHead; ++e) {
        float want = 0.0F;
        for (std::size_t j = 0; j < kTokens; ++j) want += probs[j] * vv[j * kStride + kOff + e];
        ok(std::abs(out[e] - want) < 1e-5F, "the value mix is a weighted sum over tokens");
    }
}

// A three-tensor safetensors file written here, so the reader is checked against
// bytes this test controls rather than against the model it is supposed to load.
void test_safetensors() {
    std::string head =
        R"({"__metadata__":{"format":"pt"},)"
        R"("w":{"dtype":"F32","shape":[2,2],"data_offsets":[0,16]},)"
        R"("i":{"dtype":"I64","shape":[1],"data_offsets":[16,24]}})";
    while (head.size() % 8 != 0) head += ' ';  // the spec's padding, which keeps the
                                               // data section aligned for float views
    const auto path = std::filesystem::temp_directory_path() / "cml-selftest.safetensors";
    {
        std::ofstream f(path, std::ios::binary);
        const std::uint64_t n = head.size();
        f.write(reinterpret_cast<const char*>(&n), 8);
        f.write(head.data(), static_cast<std::streamsize>(n));
        const float w[4] = {1.5F, -2.5F, 3.0F, 4.0F};
        const std::int64_t i = 7;
        f.write(reinterpret_cast<const char*>(w), 16);
        f.write(reinterpret_cast<const char*>(&i), 8);
    }

    std::string err;
    const auto st = Safetensors::open(path.string(), err);
    ok(st.ok(), "safetensors file opens");
    ok(st.has("w") && !st.has("__metadata__"), "tensors indexed, metadata skipped");
    ok(st.shape("w") == std::vector<std::int64_t>({2, 2}), "shape survives the header");

    const auto w = st.get("w", err);
    ok(w.size() == 4 && w[0] == 1.5F && w[1] == -2.5F && w[3] == 4.0F, "floats read back");

    err.clear();
    ok(st.get("i", err).empty() && err.find("F32") != std::string::npos,
       "an I64 tensor is refused, not reinterpreted as floats");
    err.clear();
    ok(st.get("absent", err).empty() && !err.empty(), "a missing tensor is an error");

    std::error_code ec;
    std::filesystem::remove(path, ec);
}

}  // namespace

void suite_encoder() {
    test_softmax();
    test_layernorm();
    test_gelu();
    test_linear();
    test_attention_kernels();
    test_safetensors();
    suite_encoder_model();
}

}  // namespace cml_test
