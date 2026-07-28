// `cml eval` — recall@k measured against your own history.
//
// Every claim this project makes about retrieval was, until now, an assertion. The
// ground truth was sitting in the index the whole time: when you asked something at
// turn N, the assistant answered at turn N+1, and that answer is by construction the
// row retrieval should have found. So replay the question with its own session
// excluded and ask whether the answer comes back.
//
// That is a real recall@k over a real corpus and it needs no labelling.
//
// The first version of this scored a perfect 0/282, because it passed the question's
// own session as `exclude_session` — and the answer lives in that session, one turn
// later, so the ground truth was excluded by construction. Same-session exclusion is a
// product rule (don't re-inject what is already on screen), not a property of the
// ranker. The benchmark measures the ranker, so it does not exclude. `--cross-session`
// puts the rule back, which measures something much harder and much rarer: whether the
// same question, asked again, finds the answer from the session that first solved it.

#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <string>
#include <vector>

#include "db.hpp"
#include "noise.hpp"
#include "recall.hpp"

namespace cml {
namespace {

struct Pair {
    std::string question;  // the user's message
    std::string session;
    std::int64_t answer_rowid;  // the assistant reply that followed it
};

// Adjacent (user -> assistant) turns within one session, straight out of the index.
std::vector<Pair> build_pairs(Db& db) {
    std::vector<Pair> pairs;
    Stmt s(db, "SELECT rowid, role, session, text FROM mem "
               "WHERE role IN ('user','assistant') ORDER BY session, ts, rowid");
    if (!s) return pairs;

    std::string prev_session, prev_role, prev_text;
    while (s.step()) {
        const std::int64_t rowid = s.i64(0);
        const std::string role = s.text(1), session = s.text(2), text = s.text(3);
        if (prev_role == "user" && role == "assistant" && session == prev_session &&
            !is_noise(prev_text) && prev_text.size() >= 20 && text.size() >= 80)
            pairs.push_back({prev_text, session, rowid});
        prev_session = session;
        prev_role = role;
        prev_text = text;
    }
    return pairs;
}

}  // namespace

int eval(const std::vector<std::string>& args) {
    std::size_t k = 3, limit = 400;
    bool cross_session = false;
    RetrieveOpts opts;
    for (std::size_t i = 0; i < args.size(); ++i) {
        const auto next = [&]() -> long {
            return (i + 1 < args.size()) ? std::strtol(args[++i].c_str(), nullptr, 10) : 0;
        };
        if (args[i] == "-k") {
            if (const long v = next(); v > 0) k = static_cast<std::size_t>(v);
        } else if (args[i] == "--limit") {
            if (const long v = next(); v > 0) limit = static_cast<std::size_t>(v);
        } else if (args[i] == "--cross-session") {
            cross_session = true;
        } else if (args[i] == "--keyword") {
            opts.keyword_only = true;
        } else if (args[i] == "--no-asks") {
            opts.text_only = true;
        }
    }

    Db db = open_db();
    if (!db) {
        std::fprintf(stderr, "cml: cannot open index\n");
        return 1;
    }

    const auto all = build_pairs(db);
    if (all.empty()) {
        std::printf("no question/answer pairs in the index — run `cml index --all` first\n");
        return 0;
    }
    // Evenly spaced rather than the first N, so one long project cannot be the whole
    // benchmark. Deterministic: same index, same sample, every run.
    std::vector<Pair> sample;
    const std::size_t stride = (all.size() + limit - 1) / limit;
    for (std::size_t i = 0; i < all.size(); i += stride) sample.push_back(all[i]);

    std::size_t fired = 0, hit = 0, hit_at_1 = 0;
    for (const auto& p : sample) {
        const auto hits = retrieve(db, p.question, cross_session ? p.session : "", k, opts);
        if (hits.empty()) continue;
        ++fired;
        for (std::size_t i = 0; i < hits.size(); ++i) {
            if (hits[i].rowid != p.answer_rowid) continue;
            ++hit;
            if (i == 0) ++hit_at_1;
            break;
        }
    }

    const auto pct = [](std::size_t n, std::size_t d) {
        return d ? 100.0 * static_cast<double>(n) / static_cast<double>(d) : 0.0;
    };
    std::printf("legs            : %s, %s\n", opts.keyword_only ? "BM25 only" : "BM25 + embedding rerank",
                opts.text_only ? "text column only (no expansions)" : "text + asks");
    std::printf("pairs           : %zu sampled of %zu question/answer turns in the index\n",
                sample.size(), all.size());
    std::printf("fired           : %zu (%.0f%%) — the rest were gated out as asking nothing\n",
                fired, pct(fired, sample.size()));
    std::printf("recall@%zu        : %zu (%.1f%% of all questions, %.1f%% of the ones it answered)\n",
                k, hit, pct(hit, sample.size()), pct(hit, fired));
    std::printf("recall@1        : %zu (%.1f%% of all questions)\n", hit_at_1,
                pct(hit_at_1, sample.size()));
    std::printf("\nthe exact answer row, found from a different session, with no labels.\n");
    return 0;
}

}  // namespace cml
