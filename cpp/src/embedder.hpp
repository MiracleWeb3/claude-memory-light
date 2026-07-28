// Which model turns text into a vector.
//
// Two backends with deliberately identical shapes: StaticModel (model2vec, one table
// lookup per token, microseconds, 51.32 MTEB) and Encoder (a real twelve-layer BERT
// forward pass, milliseconds, the model the static one was distilled FROM). The
// abstraction exists because there are genuinely two and a runtime choice between
// them — not on speculation that a third might appear.
#pragma once

#include <cstddef>
#include <memory>
#include <string>
#include <string_view>
#include <vector>

namespace cml {

class Embedder {
public:
    Embedder();
    ~Embedder();
    Embedder(Embedder&&) noexcept;
    Embedder& operator=(Embedder&&) noexcept;
    Embedder(const Embedder&) = delete;
    Embedder& operator=(const Embedder&) = delete;

    // CML_EMBED_BACKEND picks: "static" forces model2vec, "encoder" forces the
    // transformer and fails loudly if it cannot load. Unset means prefer the
    // transformer and fall back to static, so a machine that cannot fetch 128 MB
    // still has working semantic search rather than none.
    static Embedder load(std::string& error);

    bool ok() const;
    std::size_t dim() const;
    std::vector<float> encode(std::string_view text) const;
    const std::string& id() const;  // model id, for `embed` and `doctor` to report

private:
    struct Impl;
    std::unique_ptr<Impl> p_;
};

}  // namespace cml
