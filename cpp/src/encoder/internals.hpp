// Everything inside the encoder: the weight file, the math kernels, and the shape
// of a BERT block. Only encoder.hpp is meant for the rest of the binary; this
// header exists so the self-test can reach the kernels directly, which is the one
// place a silent numerical error is catchable.
#pragma once

#include <cstddef>
#include <cstdint>
#include <filesystem>
#include <span>
#include <string>
#include <string_view>
#include <unordered_map>
#include <vector>

namespace cml::enc {

// A memory-mapped safetensors file indexed by tensor name. The spans it hands out
// point into the mapping, so they live exactly as long as this object.
class Safetensors {
public:
    Safetensors() = default;
    ~Safetensors();
    Safetensors(const Safetensors&) = delete;
    Safetensors& operator=(const Safetensors&) = delete;
    Safetensors(Safetensors&&) noexcept;
    Safetensors& operator=(Safetensors&&) noexcept;

    static Safetensors open(const std::string& path, std::string& error);
    bool ok() const { return base_ != nullptr; }

    bool has(std::string_view name) const { return index_.count(std::string(name)) != 0; }

    // Empty with `error` set when the name is absent or the tensor is not F32 —
    // reading a bf16 checkpoint as floats would produce plausible garbage.
    std::span<const float> get(std::string_view name, std::string& error) const;
    std::vector<std::int64_t> shape(std::string_view name) const;

private:
    struct Entry {
        std::size_t offset = 0;  // bytes from the start of the mapping
        std::size_t count = 0;   // elements
        std::string dtype;
        std::vector<std::int64_t> dims;
    };
    std::unordered_map<std::string, Entry> index_;
    void* map_ = nullptr;
    std::size_t map_len_ = 0;
    const unsigned char* base_ = nullptr;
};

inline float dot(const float* a, const float* b, std::size_t n) {
    float s = 0.0F;
    for (std::size_t i = 0; i < n; ++i) s += a[i] * b[i];
    return s;
}

// C[M,N] = A[M,K] . B[N,K]^T + bias[N], with `bias` optional. That transposed shape
// is HuggingFace's own linear layout ([out_features, in_features]), so both operands
// are walked contiguously along K and no transpose is ever materialised.
void linear(const float* A, const float* B, const float* bias, float* C, std::size_t M,
            std::size_t N, std::size_t K);

// The portable reference the vectorised path is held to. Same contract as linear().
void linear_scalar(const float* A, const float* B, const float* bias, float* C, std::size_t M,
                   std::size_t N, std::size_t K);

// Whether linear() will take the AVX2+FMA path on this CPU. Decided at run time:
// the build carries no -mavx2, so a compile-time #ifdef would be dead code here.
bool have_avx2();

// One query's attention scores against `count` key rows spaced `stride` apart, each
// a dot over `n` (one head's slice), pre-scaled. A whole row per call, because at
// head_dim=32 a per-dot dispatch costs more than the arithmetic it dispatches.
void attention_row(const float* q, const float* keys, std::size_t stride, std::size_t count,
                   std::size_t n, float scale, float* scores);

// out[0..n) = sum over j of probs[j] * values[j * stride + 0..n). Overwrites `out`.
void attention_mix(const float* probs, const float* values, std::size_t stride,
                   std::size_t count, std::size_t n, float* out);

void softmax_row(float* x, std::size_t n);
void layernorm_row(float* x, std::size_t n, const float* gain, const float* bias, float eps);
void gelu(float* x, std::size_t n);

struct Config {
    std::size_t hidden = 0;
    std::size_t layers = 0;
    std::size_t heads = 0;
    std::size_t intermediate = 0;
    std::size_t max_pos = 0;
    std::size_t vocab = 0;
    float eps = 1e-12F;
    std::size_t head_dim() const { return heads ? hidden / heads : 0; }
};

// Views into the mapping, one set per encoder layer.
struct LayerWeights {
    const float* q_w = nullptr;
    const float* q_b = nullptr;
    const float* k_w = nullptr;
    const float* k_b = nullptr;
    const float* v_w = nullptr;
    const float* v_b = nullptr;
    const float* o_w = nullptr;
    const float* o_b = nullptr;
    const float* ln1_g = nullptr;
    const float* ln1_b = nullptr;
    const float* in_w = nullptr;
    const float* in_b = nullptr;
    const float* out_w = nullptr;
    const float* out_b = nullptr;
    const float* ln2_g = nullptr;
    const float* ln2_b = nullptr;
};

struct Weights {
    const float* word = nullptr;
    const float* pos = nullptr;
    const float* type = nullptr;
    const float* ln_g = nullptr;
    const float* ln_b = nullptr;
    std::vector<LayerWeights> layers;
};

// Working memory for one forward pass, allocated once and reused across the layers.
struct Scratch {
    std::vector<float> q, k, v, ctx, att, proj, ff;
    void fit(std::size_t tokens, const Config& cfg);
};

// Both operate in place on `h`, a tokens x hidden row-major activation block.
void self_attention(float* h, std::size_t tokens, const Config& cfg, const LayerWeights& w,
                    Scratch& s);
void block(float* h, std::size_t tokens, const Config& cfg, const LayerWeights& w, Scratch& s);

// The HuggingFace snapshot dir for a repo id, fetching model.safetensors, config.json
// and tokenizer.json with curl on a miss. Empty with `error` set when neither works.
std::filesystem::path ensure_model(const std::string& id, std::string& error);

// [CLS] and [SEP] out of tokenizer.json's added_tokens. Their ids are per-vocabulary
// (potion-base-8M puts [CLS] at 2, bert-base-uncased at 101), so they are read, not
// assumed. False when either is missing.
bool special_ids(const std::string& tokenizer_json, std::uint32_t& cls, std::uint32_t& sep);

}  // namespace cml::enc
