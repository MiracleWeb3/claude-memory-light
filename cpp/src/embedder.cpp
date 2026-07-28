#include "embedder.hpp"

#include <cstdlib>
#include <optional>
#include <utility>

#include "embed.hpp"
#include "encoder/encoder.hpp"

namespace cml {

struct Embedder::Impl {
    std::optional<Encoder> enc;
    std::optional<StaticModel> stat;
    std::string id;
};

Embedder::Embedder() : p_(std::make_unique<Impl>()) {}
Embedder::~Embedder() = default;
Embedder::Embedder(Embedder&&) noexcept = default;
Embedder& Embedder::operator=(Embedder&&) noexcept = default;

Embedder Embedder::load(std::string& error) {
    const char* want = std::getenv("CML_EMBED_BACKEND");
    const std::string backend = want ? want : "";

    // Static is the DEFAULT, and stays that way until the transformer is fast enough to
    // re-embed a corpus in minutes rather than an hour and a half. Preferring the
    // encoder whenever its weights happened to be cached was worse than slow: the two
    // backends have different widths, the stored 256-dim vectors stopped matching the
    // 384-dim query, and the width guard then refused them — so semantic search silently
    // became keyword-only for anyone who had ever run the encoder once.
    Embedder e;
    if (backend == "encoder") {
        std::string enc_err;
        auto enc = Encoder::load(encoder_model_id(), enc_err);
        if (enc.ok()) {
            e.p_->id = encoder_model_id();
            e.p_->enc.emplace(std::move(enc));
            return e;
        }
        // Asked for the transformer by name: say why it did not load rather than
        // quietly returning worse vectors under the name of better ones.
        error = enc_err;
        return e;
    }

    std::string stat_err;
    auto sm = StaticModel::load(embed_model_id(), stat_err);
    if (!sm.ok()) {
        if (!stat_err.empty()) error = stat_err;
        return e;
    }
    error.clear();
    e.p_->id = embed_model_id();
    e.p_->stat.emplace(std::move(sm));
    return e;
}

bool Embedder::ok() const {
    return (p_->enc && p_->enc->ok()) || (p_->stat && p_->stat->ok());
}

std::size_t Embedder::dim() const {
    if (p_->enc) return p_->enc->dim();
    if (p_->stat) return p_->stat->dim();
    return 0;
}

std::vector<float> Embedder::encode(std::string_view text) const {
    if (p_->enc) return p_->enc->encode(text);
    if (p_->stat) return p_->stat->encode(text);
    return {};
}

const std::string& Embedder::id() const { return p_->id; }

std::string embedder_id() {
    const char* want = std::getenv("CML_EMBED_BACKEND");
    if (want && std::string(want) == "encoder") return encoder_model_id();
    return embed_model_id();
}

}  // namespace cml
