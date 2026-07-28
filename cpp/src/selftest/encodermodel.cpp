// The encoder as a whole, against the real bge-small weights: frozen output vectors
// and the semantic structure they have to carry. Split from encoder.cpp, which
// checks the kernels and needs no download — everything here skips cleanly when the
// weights are not cached.

#include <cmath>
#include <cstdio>
#include <string>
#include <vector>

#include "encoder/encoder.hpp"
#include "harness.hpp"

namespace cml_test {
namespace {

double cosine(const std::vector<float>& a, const std::vector<float>& b) {
    double d = 0.0;
    for (std::size_t i = 0; i < a.size(); ++i) d += static_cast<double>(a[i]) * b[i];
    return d;
}

// Frozen output, captured before the kernels were optimised. Any change that moves
// these is a broken encoder, however fast it is. Regenerate deliberately, and only
// when the model itself changes.
struct Golden {
    const char* text;
    float head[4];
    float tail;
};

const Golden kFrozen[] = {
    {"cat", {-0.003818F, -0.035857F, 0.023853F, 0.039767F}, 0.072590F},
    {"the quick brown fox jumps over the lazy dog",
     {-0.104754F, -0.022422F, -0.012649F, 0.102616F},
     0.155629F},
    {"sqlite full text search index rebuilt by a stop hook",
     {-0.084266F, 0.032847F, -0.001673F, 0.044775F},
     -0.000674F},
};

}  // namespace

// Skipped, not failed, when the weights are not cached: a missing 130 MB download is
// not a code defect, and this suite must stay runnable offline.
void suite_encoder_model() {
    std::string err;
    const auto model = cml::Encoder::load(cml::encoder_model_id(), err);
    if (!model.ok()) {
        std::fprintf(stderr, "  (skipping encoder semantics: %s)\n", err.c_str());
        return;
    }

    for (const auto& g : kFrozen) {
        const auto v = model.encode(g.text);
        for (std::size_t i = 0; i < 4; ++i) {
            ok(std::abs(v[i] - g.head[i]) < 1e-4F, "frozen vector still reproduced");
        }
        ok(std::abs(v[383] - g.tail) < 1e-4F, "frozen vector, last component");
    }

    const auto cat = model.encode("cat");
    ok(cat.size() == model.dim() && model.dim() == 384, "384-dim vector");
    ok(std::abs(cosine(cat, cat) - 1.0) < 1e-5, "vectors are L2-normalised");
    ok(cat == model.encode("cat"), "encoding is deterministic");

    const auto kitten = model.encode("kitten");
    const auto car = model.encode("automobile");
    ok(cosine(cat, kitten) > cosine(cat, car), "cat is nearer kitten than automobile");

    // Absolute bounds, not just an ordering. A forward pass with a residual dropped or
    // a LayerNorm misapplied still ranks these correctly while collapsing the spread,
    // and only a measured floor catches that. Observed: 0.876 and 0.651.
    ok(cosine(cat, kitten) > 0.80 && cosine(cat, car) < 0.72, "the spread is bge-sized");

    // The whole reason for running twelve layers instead of a lookup table: the same
    // word in two senses must not land on the same vector. Observed: 0.635 vs 0.524.
    const auto river = model.encode("he sat on the river bank watching the water");
    const auto money = model.encode("she went to the bank to deposit her paycheck");
    const auto loan = model.encode("the bank approved his mortgage application");
    ok(cosine(money, loan) > cosine(money, river), "context separates the two banks");

    // Attention and position embeddings, proven rather than assumed: these two share
    // every token, so any bag-of-tokens model returns exactly 1. Observed: 0.977.
    ok(cosine(model.encode("dog bites man"), model.encode("man bites dog")) < 0.995,
       "word order changes the vector");

    const auto ask = model.encode("how do I reset my password");
    ok(cosine(ask, model.encode("I forgot my login credentials")) > 0.70,
       "a paraphrase with no shared content word still matches");
    ok(cosine(ask, model.encode("the capital of France is Paris")) < 0.55,
       "an unrelated sentence does not");

    for (const char* hard : {"", "   ", "\xff\xfe", "日本語", "a"}) {
        ok(model.encode(hard).size() == 384, "no crash on awkward input");
    }
    // 512 positions is the hard limit; more text than that must truncate, not read
    // past the end of the position embedding table.
    std::string huge;
    while (huge.size() < 40000) huge += "the quick brown fox jumps over the lazy dog. ";
    ok(model.encode(huge).size() == 384, "input past max_position_embeddings truncates");
}

}  // namespace cml_test
