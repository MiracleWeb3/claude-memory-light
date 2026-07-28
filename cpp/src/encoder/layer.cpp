// One encoder layer: multi-head self-attention, then the feed-forward network,
// each followed by a residual add and a LayerNorm. Post-norm, which is what BERT
// does — the norm comes after the residual, not before it.

#include <cmath>

#include "encoder/internals.hpp"

namespace cml::enc {

void Scratch::fit(std::size_t tokens, const Config& cfg) {
    const std::size_t th = tokens * cfg.hidden;
    q.resize(th);
    k.resize(th);
    v.resize(th);
    ctx.resize(th);
    proj.resize(th);
    att.resize(tokens);  // one query's scores; queries and heads run in sequence
    ff.resize(tokens * cfg.intermediate);
}

void self_attention(float* h, std::size_t tokens, const Config& cfg, const LayerWeights& w,
                    Scratch& s) {
    const std::size_t hid = cfg.hidden;
    const std::size_t dim = cfg.head_dim();

    linear(h, w.q_w, w.q_b, s.q.data(), tokens, hid, hid);
    linear(h, w.k_w, w.k_b, s.k.data(), tokens, hid, hid);
    linear(h, w.v_w, w.v_b, s.v.data(), tokens, hid, hid);

    // No attention mask: encode() builds one sequence with no padding, so every
    // position is real and the mask would be identically 1.
    // ponytail: add the mask the day rows are batched, not before.
    const float scale = 1.0F / std::sqrt(static_cast<float>(dim));
    // Score, softmax and mix one query at a time, so the row of probabilities is
    // still in L1 when it is consumed and never has to be stored T-by-T.
    float* scores = s.att.data();
    for (std::size_t head = 0; head < cfg.heads; ++head) {
        const std::size_t off = head * dim;
        for (std::size_t i = 0; i < tokens; ++i) {
            attention_row(s.q.data() + i * hid + off, s.k.data() + off, hid, tokens, dim, scale,
                          scores);
            softmax_row(scores, tokens);
            attention_mix(scores, s.v.data() + off, hid, tokens, dim, s.ctx.data() + i * hid + off);
        }
    }

    linear(s.ctx.data(), w.o_w, w.o_b, s.proj.data(), tokens, hid, hid);
    for (std::size_t i = 0; i < tokens; ++i) {
        float* row = h + i * hid;
        const float* add = s.proj.data() + i * hid;
        for (std::size_t c = 0; c < hid; ++c) row[c] += add[c];
        layernorm_row(row, hid, w.ln1_g, w.ln1_b, cfg.eps);
    }
}

void block(float* h, std::size_t tokens, const Config& cfg, const LayerWeights& w, Scratch& s) {
    self_attention(h, tokens, cfg, w, s);

    linear(h, w.in_w, w.in_b, s.ff.data(), tokens, cfg.intermediate, cfg.hidden);
    gelu(s.ff.data(), tokens * cfg.intermediate);
    linear(s.ff.data(), w.out_w, w.out_b, s.proj.data(), tokens, cfg.hidden, cfg.intermediate);

    for (std::size_t i = 0; i < tokens; ++i) {
        float* row = h + i * cfg.hidden;
        const float* add = s.proj.data() + i * cfg.hidden;
        for (std::size_t c = 0; c < cfg.hidden; ++c) row[c] += add[c];
        layernorm_row(row, cfg.hidden, w.ln2_g, w.ln2_b, cfg.eps);
    }
}

}  // namespace cml::enc
