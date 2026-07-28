// Loading the model and running the forward pass:
//   ids -> word + position + token_type embeddings -> LayerNorm
//       -> 12 x block()  (attention, FFN, residuals, norms)
//       -> [CLS] row -> L2 normalise

#include "encoder/encoder.hpp"

#include <simdjson.h>

#include <algorithm>
#include <cmath>
#include <cstdlib>

#include "encoder/internals.hpp"
#include "utf8.hpp"
#include "wordpiece.hpp"

namespace cml {
namespace {

using enc::Config;

// Reads one tensor per call and latches the first failure, so the binder below can
// be written as a straight list instead of sixteen checked assignments.
struct Binder {
    const enc::Safetensors& st;
    std::string prefix;
    std::string& error;

    const float* operator()(const std::string& name) {
        if (!error.empty()) return nullptr;
        std::string e;
        const auto span = st.get(prefix + name, e);
        if (span.empty()) error = e;
        return span.data();
    }
};

bool read_config(const std::string& path, Config& cfg, std::string& error) {
    simdjson::dom::parser parser;
    simdjson::dom::element doc;
    if (parser.load(path).get(doc) != simdjson::SUCCESS) {
        error = "cannot read config.json at " + path;
        return false;
    }
    const auto num = [&doc](const char* key, std::size_t fallback) {
        std::int64_t v = 0;
        return doc[key].get(v) == simdjson::SUCCESS && v > 0 ? static_cast<std::size_t>(v)
                                                             : fallback;
    };
    cfg.hidden = num("hidden_size", 0);
    cfg.layers = num("num_hidden_layers", 0);
    cfg.heads = num("num_attention_heads", 0);
    cfg.intermediate = num("intermediate_size", 0);
    cfg.max_pos = num("max_position_embeddings", 0);
    cfg.vocab = num("vocab_size", 0);
    double eps = 1e-12;
    if (doc["layer_norm_eps"].get(eps) == simdjson::SUCCESS) cfg.eps = static_cast<float>(eps);

    if (cfg.hidden == 0 || cfg.layers == 0 || cfg.heads == 0 || cfg.intermediate == 0 ||
        cfg.vocab == 0 || cfg.max_pos < 4 || cfg.hidden % cfg.heads != 0) {
        error = "config.json does not describe a usable BERT encoder: " + path;
        return false;
    }
    // Anything but absolute positions would need different arithmetic below, and
    // reading the wrong kind silently is worse than refusing it.
    std::string_view kind;
    if (doc["position_embedding_type"].get(kind) == simdjson::SUCCESS && kind != "absolute") {
        error = "position_embedding_type is '" + std::string(kind) + "', only 'absolute' is run";
        return false;
    }
    return true;
}

}  // namespace

struct Encoder::Impl {
    enc::Safetensors st;
    WordPiece tok;
    Config cfg;
    enc::Weights w;
    std::uint32_t cls = 0;
    std::uint32_t sep = 0;
    bool ready = false;
};

Encoder::Encoder() : p_(std::make_unique<Impl>()) {}
Encoder::~Encoder() = default;
Encoder::Encoder(Encoder&&) noexcept = default;
Encoder& Encoder::operator=(Encoder&&) noexcept = default;

bool Encoder::ok() const { return p_ && p_->ready; }
std::size_t Encoder::dim() const { return p_ ? p_->cfg.hidden : 0; }

std::string encoder_model_id() {
    if (const char* m = std::getenv("CML_ENCODER_MODEL")) {
        if (*m) return m;
    }
    return "BAAI/bge-small-en-v1.5";
}

Encoder Encoder::load(const std::string& id, std::string& error) {
    Encoder m;
    const auto dir = enc::ensure_model(id, error);
    if (dir.empty()) return m;

    if (!read_config((dir / "config.json").string(), m.p_->cfg, error)) return m;

    const std::string tokenizer = (dir / "tokenizer.json").string();
    m.p_->tok = WordPiece::load(tokenizer, error);
    if (!m.p_->tok.ok()) return m;
    if (!enc::special_ids(tokenizer, m.p_->cls, m.p_->sep)) {
        error = "tokenizer.json has no [CLS]/[SEP] — not a BERT tokenizer";
        return m;
    }

    m.p_->st = enc::Safetensors::open((dir / "model.safetensors").string(), error);
    if (!m.p_->st.ok()) return m;

    // Checkpoints saved as a bare BertModel have no prefix; those saved from a
    // head model carry "bert.".
    Binder bind{m.p_->st, m.p_->st.has("embeddings.word_embeddings.weight") ? "" : "bert.",
                error};

    const Config& cfg = m.p_->cfg;
    enc::Weights& w = m.p_->w;
    w.word = bind("embeddings.word_embeddings.weight");
    w.pos = bind("embeddings.position_embeddings.weight");
    w.type = bind("embeddings.token_type_embeddings.weight");
    w.ln_g = bind("embeddings.LayerNorm.weight");
    w.ln_b = bind("embeddings.LayerNorm.bias");

    w.layers.resize(cfg.layers);
    for (std::size_t i = 0; i < cfg.layers; ++i) {
        const std::string p = "encoder.layer." + std::to_string(i) + ".";
        enc::LayerWeights& l = w.layers[i];
        l.q_w = bind(p + "attention.self.query.weight");
        l.q_b = bind(p + "attention.self.query.bias");
        l.k_w = bind(p + "attention.self.key.weight");
        l.k_b = bind(p + "attention.self.key.bias");
        l.v_w = bind(p + "attention.self.value.weight");
        l.v_b = bind(p + "attention.self.value.bias");
        l.o_w = bind(p + "attention.output.dense.weight");
        l.o_b = bind(p + "attention.output.dense.bias");
        l.ln1_g = bind(p + "attention.output.LayerNorm.weight");
        l.ln1_b = bind(p + "attention.output.LayerNorm.bias");
        l.in_w = bind(p + "intermediate.dense.weight");
        l.in_b = bind(p + "intermediate.dense.bias");
        l.out_w = bind(p + "output.dense.weight");
        l.out_b = bind(p + "output.dense.bias");
        l.ln2_g = bind(p + "output.LayerNorm.weight");
        l.ln2_b = bind(p + "output.LayerNorm.bias");
    }
    if (!error.empty()) return m;

    // config.json and the weights disagreeing is the one failure that would produce
    // plausible-looking nonsense rather than an error, so it is checked, not assumed.
    const auto shape = m.p_->st.shape(bind.prefix + "embeddings.word_embeddings.weight");
    if (shape.size() != 2 || static_cast<std::size_t>(shape[0]) != cfg.vocab ||
        static_cast<std::size_t>(shape[1]) != cfg.hidden) {
        error = "word_embeddings does not match config.json's vocab_size x hidden_size";
        return m;
    }

    m.p_->ready = true;
    return m;
}

std::vector<float> Encoder::encode(std::string_view text) const {
    if (!ok()) return {};
    const Config& cfg = p_->cfg;
    const enc::Weights& w = p_->w;

    // Cheap byte-level cut first, so a megabyte of transcript is not tokenised in
    // full only to keep its first 512 tokens.
    auto ids = p_->tok.encode(utf8_take(text, cfg.max_pos * p_->tok.median_token_bytes()));
    if (ids.size() > cfg.max_pos - 2) ids.resize(cfg.max_pos - 2);

    std::vector<std::uint32_t> seq;
    seq.reserve(ids.size() + 2);
    seq.push_back(p_->cls);
    for (const std::uint32_t id : ids) {
        seq.push_back(id < cfg.vocab ? id : p_->tok.unk_id());  // vocab/weights mismatch
    }
    seq.push_back(p_->sep);
    const std::size_t tokens = seq.size();

    std::vector<float> h(tokens * cfg.hidden);
    for (std::size_t i = 0; i < tokens; ++i) {
        const float* word = w.word + static_cast<std::size_t>(seq[i]) * cfg.hidden;
        const float* pos = w.pos + i * cfg.hidden;
        float* row = h.data() + i * cfg.hidden;
        // token_type is 0 throughout: one segment, no sentence-pair task.
        for (std::size_t c = 0; c < cfg.hidden; ++c) row[c] = word[c] + pos[c] + w.type[c];
        enc::layernorm_row(row, cfg.hidden, w.ln_g, w.ln_b, cfg.eps);
    }

    enc::Scratch s;
    s.fit(tokens, cfg);
    for (const auto& layer : w.layers) enc::block(h.data(), tokens, cfg, layer, s);

    std::vector<float> out(h.begin(), h.begin() + static_cast<std::ptrdiff_t>(cfg.hidden));
    double norm = 0.0;
    for (const float v : out) norm += static_cast<double>(v) * v;
    const float inv = static_cast<float>(1.0 / std::max(std::sqrt(norm), 1e-12));
    for (float& v : out) v *= inv;
    return out;
}

}  // namespace cml
