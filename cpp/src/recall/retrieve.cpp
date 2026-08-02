// The retrieval core: every gate except per-session dedupe, shared by the hook and by
// `cml eval`. Why it is unconditional, and why it ranks the way it does:
//
// Measured across 564 transcripts before this existed: the write side, which is hooked,
// ran 625 times (index 321, capture 304). The read side, which was not, ran 20 times —
// `cml search` in 12 sessions, 2%. Storage was never the gap. Retrieval being a decision
// the model had to remember to make was the gap, and a decision made 2% of the time is
// the same as no feature at all.
//
// So the hook fires on every prompt and the model is not consulted. What it must NOT do
// is become wallpaper: three gates keep it quiet — a prompt with fewer than two content
// words asks nothing, a row sharing fewer than two of them is a coincidence (both below),
// and a row already injected this session is not injected twice (that one is the hook's).
//
// BM25 only, deliberately. `cml eval` measured the embedding leg making this path worse
// in every configuration: -5 and -9 rows with potion-base-8M, -2 and -3 with a real
// bge-small transformer, and unchanged when ungated — because the two-shared-word floor
// below is itself a lexical requirement, so a purely semantic match cannot survive the
// pipeline however the fusion is arranged. Vectors still serve `cml search --semantic`,
// where they do something BM25 cannot: "trackpad dragging" finds touchpad rows.

#include "recall.hpp"

#include <algorithm>
#include <string>

#include "db.hpp"
#include "noise.hpp"
#include "recall/internal.hpp"
#include "search.hpp"

namespace cml {
namespace {

constexpr std::size_t kSnippet = 160;    // ~3 lines of context total
constexpr std::size_t kMinTerms = 2;     // "yes" and "do it" recall nothing
constexpr std::size_t kMinOverlap = 2;   // one shared word is a coincidence, two is a topic
constexpr int kCandidates = 40;

// Rarity gate. Measured before it existed: 76% of real prompts fired, most of them on
// words like "code", "problem" or "same" — content words by any stopword list, and
// worthless in an index where every row is about code. BM25 discounts them when it
// ranks; the overlap gate does not, so two such words were enough to drag in a row
// from an unrelated project. A term in more than kDfPercent of the index is not
// evidence, and a prompt with fewer than two terms left after this asks nothing.
constexpr std::int64_t kMinCorpus = 200;   // below this, rarity cannot be judged at all
constexpr std::int64_t kDfPercent = 6;
constexpr std::int64_t kDfFloor = 5;       // ...but never call a 5-row term common
constexpr std::size_t kEchoMin = 25;       // shorter than this, containment means nothing

// FTS5 OR query. A prompt is not a conjunction: requiring every word (which is what
// `cml search` does, correctly, for a hand-typed query) matches nothing when the query
// is a whole sentence. BM25 then ranks by how many rare terms a row actually hit.
std::string or_query(const std::vector<std::string>& terms) {
    std::string q;
    for (const auto& t : terms) {
        std::string escaped;
        for (const char c : t) {
            escaped.push_back(c);
            if (c == '"') escaped.push_back('"');  // FTS5 doubles the quote
        }
        if (!q.empty()) q += " OR ";
        q += '"' + escaped + '"';
    }
    return q;
}

// Keep only terms rare enough to mean something. A term matching nothing is dropped
// with them: it cannot rank, and it cannot count toward the overlap floor either.
std::vector<std::string> discriminative(Db& db, const std::vector<std::string>& terms,
                                        Lane lane) {
    // Counted over `mem` alone, which is conversation. The work lives in `work` and is
    // deliberately not part of this ratio: a stack trace repeats an identifier hundreds
    // of times, and when tool rows shared this table the ceiling moved so far that the
    // gate fired on 99% of prompts instead of 50% — the wallpaper it exists to prevent.
    // Each table judges rarity by its own statistics. Using conversation counts for the
    // work lane silently deleted the only terms that lane exists for: "reply.cpp" occurs
    // zero times in anything anyone SAID, so it was dropped as matching nothing, and
    // "what was that undefined reference in reply.cpp" came back empty with 29,703 tool
    // rows sitting there holding the answer.
    const bool tools = lane == Lane::Tools;
    const std::int64_t total =
        db.scalar(tools ? "SELECT count(*) FROM work" : "SELECT count(*) FROM mem");
    if (total < kMinCorpus) return terms;  // a young index: let everything through
    const std::int64_t ceiling = std::max(kDfFloor, total * kDfPercent / 100);

    Stmt df(db, tools ? "SELECT count(*) FROM work WHERE work MATCH ?1"
                      : "SELECT count(*) FROM mem WHERE mem MATCH ?1");
    if (!df) return terms;
    std::vector<std::string> out;
    for (const auto& t : terms) {
        df.reset();
        df.bind(1, '"' + t + '"');
        if (!df.step()) continue;
        const std::int64_t n = df.i64(0);
        if (n > 0 && n <= ceiling) out.push_back(t);
    }
    return out;
}

// A row that is the prompt itself, asked once before, tells the model nothing that is
// not already on screen. The answer to it might — the echo never does.
bool echoes(std::string_view row, const std::string& prompt_flat) {
    if (prompt_flat.size() < kEchoMin) return false;
    const std::string a = squeeze(lower(row), 400);
    return a.find(prompt_flat) != std::string::npos || prompt_flat.find(a) != std::string::npos;
}

}  // namespace

std::vector<Hit> retrieve(Db& db, std::string_view prompt, std::string_view exclude_session,
                          std::size_t limit, RetrieveOpts opts) {
    std::vector<Hit> out;
    // A slash-command envelope or a hook injection is not a question to the index.
    if (is_noise(prompt)) return out;

    const auto words = content_terms(prompt);
    if (words.size() < kMinTerms) return out;
    const auto terms =
        discriminative(db, words, opts.tools ? Lane::Tools : Lane::Conversation);
    if (terms.size() < kMinTerms) return out;

    const std::string prompt_flat = squeeze(lower(prompt), 400);
    std::string joined;
    for (const auto& t : terms) {
        if (!joined.empty()) joined += ' ';
        joined += t;
    }
    // {text} : (...) restricts the match to one column. FTS5 searches every indexed
    // column by default, which is exactly what makes doc2query work — and what has to
    // be switched off to measure whether it works.
    std::string fts = or_query(terms);
    if (opts.text_only) fts = "{text} : (" + fts + ")";
    const auto ranked =
        rank_rowids(db, fts, opts.with_vectors ? joined : std::string{}, kCandidates,
                    nullptr, opts.ungated, opts.tools ? Lane::Tools : Lane::Conversation);
    if (ranked.empty()) return out;

    const auto gists = gist_lookup(db);
    // Fetch from the SAME table the ranking came from. This read was pinned to `mem`
    // while the work lane ranked against `work`, so every tool rowid was looked up in
    // the wrong table and silently produced nothing — the lane was wired end to end and
    // returned empty for every query it existed to answer.
    Stmt fetch(db, opts.tools
                       ? "SELECT ts, role, project, session, substr(text,1,400), "
                         "substr(text,1,64) FROM work WHERE rowid=?1"
                       : "SELECT ts, role, project, session, substr(text,1,400), "
                         "substr(text,1,64) FROM mem WHERE rowid=?1");
    if (!fetch) return out;

    for (const std::int64_t rowid : ranked) {
        if (out.size() >= limit) break;
        fetch.reset();
        fetch.bind(1, rowid);
        if (!fetch.step()) continue;

        const std::string ts = fetch.text(0), role = fetch.text(1), proj = fetch.text(2);
        const std::string sess = fetch.text(3), text = fetch.text(4), head = fetch.text(5);

        if (!exclude_session.empty() && sess == exclude_session) continue;
        if (term_overlap(text, terms) < kMinOverlap) continue;
        if (echoes(text, prompt_flat)) continue;

        const std::string key = stable_key(sess, ts, role, head);
        const auto it = gists.find(key);
        const std::string date = (ts.size() >= 10) ? ts.substr(0, 10) : "no-date";
        out.push_back({rowid, date + " " + role + (proj.empty() ? "" : "/" + proj) + ": " +
                                  squeeze((it != gists.end()) ? it->second : text, kSnippet),
                       key});
    }
    return out;
}

}  // namespace cml
