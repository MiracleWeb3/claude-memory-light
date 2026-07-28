// UserPromptSubmit hook: the read half of the memory loop.
//
// Measured across 564 transcripts before this existed: the write side, which is hooked,
// ran 625 times (index 321, capture 304). The read side, which was not, ran 20 times —
// `cml search` in 12 sessions, 2%. Storage was never the gap. Retrieval being a decision
// the model had to remember to make was the gap, and a decision made 2% of the time is
// the same as no feature at all.
//
// So this fires on every prompt and the model is not consulted. What it must NOT do is
// become wallpaper: three gates keep it quiet — a prompt with fewer than two content
// words asks nothing, a row sharing fewer than two of them is a coincidence, and a row
// already injected this session is not injected twice.
//
// BM25 only, deliberately. `cml eval` measured the embedding leg making this path worse
// in every configuration: -5 and -9 rows with potion-base-8M, -2 and -3 with a real
// bge-small transformer, and unchanged when ungated — because the two-shared-word floor
// below is itself a lexical requirement, so a purely semantic match cannot survive the
// pipeline however the fusion is arranged. Vectors still serve `cml search --semantic`,
// where they do something BM25 cannot: "trackpad dragging" finds touchpad rows.

#include "recall.hpp"

#include <simdjson.h>

#include <algorithm>
#include <cctype>
#include <cstdio>
#include <iostream>
#include <iterator>
#include <string>
#include <unordered_set>

#include "db.hpp"
#include "json.hpp"
#include "noise.hpp"
#include "search.hpp"

namespace cml {
namespace {

constexpr std::size_t kMaxHits = 3;      // a briefing, not a transcript
constexpr std::size_t kSnippet = 160;    // ~3 lines of context total
constexpr std::size_t kMaxTerms = 12;    // an OR query over a whole paste is not a query
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

// Function words carry no retrieval signal but would dominate an OR query.
constexpr std::string_view kStop[] = {
    "the",   "and",   "for",    "are",   "but",   "not",    "you",  "all",   "can",
    "was",   "one",   "our",    "out",   "how",   "its",    "who",  "did",   "yes",
    "get",   "has",   "him",    "his",   "she",   "her",    "too",  "use",   "why",
    "what",  "this",  "that",   "with",  "have",  "from",   "they", "been",  "were",
    "when",  "your",  "said",   "each",  "which", "them",   "then", "than",  "some",
    "would", "there", "their",  "about", "into",  "just",   "like", "make",  "made",
    "does",  "doing", "dont",   "cant",  "wont",  "should", "could", "need", "want",
    "please","okay",  "also",   "only",  "very",  "more",   "most", "much",  "still",
    "again", "now",   "here",   "let",   "lets",  "will",   "any",  "because",
};

bool is_stop(const std::string& w) {
    return std::find(std::begin(kStop), std::end(kStop), std::string_view(w)) != std::end(kStop);
}

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

std::string lower(std::string_view s) {
    std::string out;
    out.reserve(s.size());
    // ponytail: ASCII fold only. FTS5's unicode61 tokenizer folds Cyrillic and accents
    // for the retrieval itself; this is the overlap floor, and both sides of the compare
    // go through this same function, so the two stay consistent.
    for (const char c : s)
        out.push_back(static_cast<char>(std::tolower(static_cast<unsigned char>(c))));
    return out;
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

std::vector<std::string> content_terms(std::string_view prompt) {
    std::vector<std::string> out;
    std::unordered_set<std::string> seen;
    std::string cur;
    const auto flush = [&] {
        if (cur.size() >= 3 && !is_stop(cur) && seen.insert(cur).second) out.push_back(cur);
        cur.clear();
    };
    for (const char c : prompt) {
        const auto u = static_cast<unsigned char>(c);
        // Letters, digits and any non-ASCII byte stay in the token, so a Russian word
        // survives as one unit instead of shattering into per-byte noise.
        if (std::isalnum(u) || u >= 0x80)
            cur.push_back(static_cast<char>(std::tolower(u)));
        else
            flush();
    }
    flush();
    if (out.size() > kMaxTerms) out.resize(kMaxTerms);
    return out;
}

std::size_t term_overlap(std::string_view text, const std::vector<std::string>& terms) {
    const std::string hay = lower(text);
    std::size_t n = 0;
    for (const auto& t : terms)
        if (hay.find(t) != std::string::npos) ++n;
    return n;
}

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

void recall() {
    // Whatever happens, the prompt must go through.
    const auto passthrough = []() { std::printf("{\"continue\": true}\n"); };

    std::string payload((std::istreambuf_iterator<char>(std::cin)),
                        std::istreambuf_iterator<char>());
    if (payload.empty()) return passthrough();

    simdjson::dom::parser parser;
    simdjson::dom::element data;
    if (parser.parse(payload).get(data) != simdjson::SUCCESS) return passthrough();

    std::string_view prompt, session;
    if (data["prompt"].get(prompt) != simdjson::SUCCESS || prompt.empty()) return passthrough();
    if (data["session_id"].get(session) != simdjson::SUCCESS) session = {};

    Db db = open_db();
    if (!db) return passthrough();

    // Over-fetch: rows already injected this session are skipped below, and the next
    // fresh one should take the freed slot rather than leaving the briefing short.
    const auto conv = retrieve(db, prompt, session, kMaxHits * 4);

    // The work — commands and their output, in its own table. It gets a RESERVED slot,
    // not an appended one: appending put it after the conversation hits, which had
    // already spent the whole three-line budget, so 29,703 tool rows stayed unreachable
    // exactly as if the lane had never been wired up. A lane that only runs when the
    // other lane comes up short is not wired up.
    RetrieveOpts work_opts;
    work_opts.tools = true;
    const auto work = retrieve(db, prompt, session, 2, work_opts);
    if (conv.empty() && work.empty()) return passthrough();

    Stmt mark(db, "INSERT OR IGNORE INTO recalled(session, key) VALUES(?1, ?2)");
    const auto fresh = [&](const Hit& h) {
        if (session.empty() || !mark) return true;
        mark.reset();
        mark.bind(1, session).bind(2, h.key);
        return mark.run() && sqlite3_changes(db.raw()) != 0;
    };

    std::vector<std::string> lines;
    const std::size_t conv_budget = work.empty() ? kMaxHits : kMaxHits - 1;
    for (const auto& h : conv) {
        if (lines.size() >= conv_budget) break;
        if (fresh(h)) lines.push_back(h.line);
    }
    for (const auto& h : work) {
        if (lines.size() >= kMaxHits) break;
        if (fresh(h)) lines.push_back("(ran) " + h.line);
    }
    if (lines.empty()) return passthrough();

    std::string msg =
        "[cml recall] from your own history, matched on this prompt — it is what was true "
        "when written, so check it still holds before relying on it:";
    for (const auto& l : lines) msg += "\n\xC2\xB7 " + l;  // U+00B7 MIDDLE DOT

    std::string out =
        "{\"continue\": true, \"hookSpecificOutput\": {\"hookEventName\": "
        "\"UserPromptSubmit\", \"additionalContext\": ";
    json::quote_into(out, msg);
    out += "}}";
    std::printf("%s\n", out.c_str());
}

}  // namespace cml
