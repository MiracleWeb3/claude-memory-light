// model2vec static embeddings: tokenize, look up one row per token, mean-pool,
// L2-normalise. No neural network runs at inference — the distillation baked PCA and
// Zipf weighting into the stored matrix — so this is a table lookup and an average.
//
// The 29 MB matrix is mmap'd rather than read: the Rust build copied the whole file
// into RAM on every invocation, which is real money on a 3.6 GB laptop.
#pragma once

#include <cstddef>
#include <cstdint>
#include <string>
#include <string_view>
#include <vector>

#include "wordpiece.hpp"

namespace cml {

class StaticModel {
public:
    StaticModel() = default;
    ~StaticModel();
    StaticModel(const StaticModel&) = delete;
    StaticModel& operator=(const StaticModel&) = delete;
    StaticModel(StaticModel&&) noexcept;
    StaticModel& operator=(StaticModel&&) noexcept;

    // `id` is a HuggingFace repo id ("minishlab/potion-base-8M") or a local folder.
    // Returns a model for which ok() is false, with `error` set, on failure.
    static StaticModel load(const std::string& id, std::string& error);

    bool ok() const { return matrix_ != nullptr && tokenizer_.ok(); }
    std::size_t dim() const { return dim_; }

    std::vector<float> encode(std::string_view text) const;

private:
    WordPiece tokenizer_;
    const float* matrix_ = nullptr;  // [rows_ x dim_], owned by the mapping below
    void* map_ = nullptr;
    std::size_t map_len_ = 0;
    std::size_t rows_ = 0;
    std::size_t dim_ = 0;
    bool normalize_ = true;
};

// Default is tiny and fast; CML_EMBED_MODEL swaps in a bigger one, after which
// `cml embed --all` must rebuild the vectors.
std::string embed_model_id();

}  // namespace cml
