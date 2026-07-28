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

    Embedder e;
    if (backend != "static") {
        std::string enc_err;
        auto enc = Encoder::load(encoder_model_id(), enc_err);
        if (enc.ok()) {
            e.p_->id = encoder_model_id();
            e.p_->enc.emplace(std::move(enc));
            return e;
        }
        // Asked for the transformer by name: say why it did not load rather than
        // quietly returning worse vectors under the name of better ones.
        if (backend == "encoder") {
            error = enc_err;
            return e;
        }
        error = enc_err;  // kept as context if the static path also fails
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

}  // namespace cml
