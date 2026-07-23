#include <cmath>
#include <cstdio>
#include <string>

#include "embed.hpp"
#include "harness.hpp"

namespace cml_test {
namespace {

void test_tokenizer_and_embedding() {
    // Skipped rather than failed when the model has not been fetched: the tokenizer
    // is data-dependent and a missing cache is not a code defect.
    std::string err;
    auto model = cml::StaticModel::load(cml::embed_model_id(), err);
    if (!model.ok()) {
        std::fprintf(stderr, "  (skipping embedding checks: %s)\n", err.c_str());
        return;
    }

    const auto v = model.encode("telegram channels flag");
    ok(v.size() == 256, "256-dim vector");
    double norm = 0;
    for (const float x : v) norm += double(x) * x;
    ok(std::abs(norm - 1.0) < 1e-3, "vector is L2-normalised");

    // Same text must give the same vector, or the index is not reproducible.
    const auto v2 = model.encode("telegram channels flag");
    ok(v == v2, "encoding is deterministic");

    // Unrelated text must not collide.
    const auto other = model.encode("sqlite full text search index");
    double dot = 0;
    for (std::size_t i = 0; i < v.size(); ++i) dot += double(v[i]) * other[i];
    ok(dot < 0.99, "distinct texts get distinct vectors");

    // A multi-byte character absent from the vocab segfaulted the greedy matcher:
    // `end` walked down to `start` and `--end` underflowed size_t to SIZE_MAX.
    for (const char* hard : {"⊞", "日本語", "", "   ", "a", "\xff\xfe"}) {
        const auto safe = model.encode(hard);
        ok(safe.size() == 256, "no crash on awkward input");
    }
}

}  // namespace

void suite_embedding() { test_tokenizer_and_embedding(); }

}  // namespace cml_test
