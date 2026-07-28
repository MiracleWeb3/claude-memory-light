// A BERT-class sentence encoder that runs in this process, on the CPU: text in,
// one L2-normalised vector out. No Python, no ONNX runtime, no inference library —
// the weights are mmap'd out of a safetensors file and the twelve layers are
// arithmetic in kernels.cpp.
//
// This is the model StaticModel's potion-base-8M was distilled *from*. Static
// embeddings are one table lookup per token with no attention, so "river bank" and
// "bank loan" land on the same vector; twelve layers of self-attention are what
// makes the difference, at the cost of milliseconds instead of microseconds.
#pragma once

#include <cstddef>
#include <memory>
#include <string>
#include <string_view>
#include <vector>

namespace cml {

class Encoder {
public:
    Encoder();
    ~Encoder();
    Encoder(const Encoder&) = delete;
    Encoder& operator=(const Encoder&) = delete;
    Encoder(Encoder&&) noexcept;
    Encoder& operator=(Encoder&&) noexcept;

    // `id` is a HuggingFace repo id ("BAAI/bge-small-en-v1.5") or a local folder.
    // Downloads the model on a cache miss. On failure returns an encoder for which
    // ok() is false, with `error` set.
    static Encoder load(const std::string& id, std::string& error);

    bool ok() const;
    std::size_t dim() const;  // 384 for bge-small-en-v1.5

    // [CLS] pooled and L2-normalised, so a dot product is the cosine similarity.
    // Empty when !ok(). Deterministic, and const: the working buffers are per call,
    // which costs one allocation and buys a shareable encoder.
    std::vector<float> encode(std::string_view text) const;

private:
    struct Impl;
    std::unique_ptr<Impl> p_;
};

// CML_ENCODER_MODEL, else BAAI/bge-small-en-v1.5. Changing it invalidates every
// stored vector — the dimensions need not even match.
std::string encoder_model_id();

}  // namespace cml
