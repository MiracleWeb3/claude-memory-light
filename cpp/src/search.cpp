// Keyword search over the FTS5 index.
//
// Two legs — BM25 keyword plus a local-embedding KNN — fused with reciprocal rank
// fusion. Both are live; `--semantic` refuses loudly rather than silently returning
// keyword hits under a semantic flag.
//
// The vector leg RERANKS, it does not recall. Measured with equal RRF weight: the
// query "guest mode zero lookups enrichment" returned "Jarvis mode v1.4.0" from a
// different project at #1, because a row at vector rank 0 scored identically to the
// top BM25 hit while sharing not one word with the query. Embeddings over whole short
// rows put every project's rows in each other's neighbourhood, so a row that matches
// nothing lexically must not be able to enter the result set at all.

#include <algorithm>
#include <cstdio>
#include <string>
#include <unordered_map>
#include <vector>

#include "db.hpp"
#include "noise.hpp"
#include "search.hpp"
#include "utf8.hpp"
#include "vec.hpp"

namespace cml {
namespace {

constexpr int kCandidates = 60;
constexpr double kRrfK = 60.0;

std::string lower(std::string s) {
    std::transform(s.begin(), s.end(), s.begin(),
                   [](unsigned char c) { return std::tolower(c); });
    return s;
}

// FTS5 phrase query: every whitespace-separated token quoted, all required.
std::string fts_query(const std::vector<std::string>& terms) {
    std::string q;
    for (const auto& term : terms) {
        std::size_t start = 0;
        while (start < term.size()) {
            const std::size_t end = term.find(' ', start);
            const std::string tok = term.substr(start, end - start);
            if (!tok.empty()) {
                std::string escaped;
                for (const char c : tok) {
                    escaped.push_back(c);
                    if (c == '"') escaped.push_back('"');  // FTS5 doubles the quote
                }
                if (!q.empty()) q += " AND ";
                q += '"' + escaped + '"';
            }
            if (end == std::string::npos) break;
            start = end + 1;
        }
    }
    return q;
}

std::string pad(std::string s, std::size_t width) {
    if (s.size() < width) s.append(width - s.size(), ' ');
    return s;
}

}  // namespace

std::vector<std::int64_t> rank_rowids(Db& db, const std::string& fts,
                                      const std::string& semantic_query, int candidates,
                                      std::string* semantic_error, bool ungated) {
    // Keyword leg: FTS5 BM25.
    std::vector<std::int64_t> hits;
    if (!fts.empty()) {
        Stmt s(db, "SELECT rowid FROM mem WHERE mem MATCH ?1 ORDER BY rank LIMIT ?2");
        if (s) {
            s.bind(1, fts).bind(2, candidates);
            while (s.step()) hits.push_back(s.i64(0));
        } else if (semantic_error) {
            *semantic_error = db.error();
        }
    }

    // Semantic leg: local embeddings + sqlite-vec KNN. Hybrid by default once
    // `cml embed` has run; silent when the model or the vector table is absent.
    std::vector<std::int64_t> vec_hits;
    if (!semantic_query.empty()) {
        std::string err;
        vec_hits = semantic_hits(db, semantic_query, candidates, err);
        if (vec_hits.empty() && semantic_error && semantic_error->empty()) *semantic_error = err;
    }

    // Reciprocal rank fusion across both legs.
    std::unordered_map<std::int64_t, double> score;
    for (std::size_t i = 0; i < hits.size(); ++i) {
        score[hits[i]] += 1.0 / (kRrfK + static_cast<double>(i));
    }
    // ungated lets the embedding leg ADD rows BM25 never found, instead of only
    // reordering the ones it did. That is the configuration the gate was written to
    // forbid, back when the embeddings were static and added noise.
    const bool gated = !fts.empty() && !ungated;
    for (std::size_t i = 0; i < vec_hits.size(); ++i) {
        // Lexical gate: reorder rows the keyword leg already found, never add new ones.
        if (gated && score.find(vec_hits[i]) == score.end()) continue;
        score[vec_hits[i]] += 1.0 / (kRrfK + static_cast<double>(i));
    }
    // Deterministic order. The Rust original sorted a Vec built from a HashMap, whose
    // iteration order Rust randomises per process — the same query on the same database
    // returned three different orderings across five runs. Ties break on rowid so the
    // result is reproducible, which matters when two binaries are being diffed.
    std::vector<std::pair<std::int64_t, double>> ranked(score.begin(), score.end());
    std::sort(ranked.begin(), ranked.end(), [](const auto& a, const auto& b) {
        if (a.second != b.second) return a.second > b.second;
        return a.first < b.first;
    });

    std::vector<std::int64_t> out;
    out.reserve(ranked.size());
    for (const auto& [rowid, _] : ranked) out.push_back(rowid);
    return out;
}

int search(const std::vector<std::string>& args) {
    std::size_t limit = 12;
    std::string project, role;
    bool semantic_only = false;
    bool keyword_only = false;
    std::vector<std::string> terms;

    for (std::size_t i = 0; i < args.size(); ++i) {
        const std::string& a = args[i];
        if (a == "--limit" && i + 1 < args.size()) {
            limit = std::strtoul(args[++i].c_str(), nullptr, 10);
            if (limit == 0) limit = 12;
        } else if (a == "--project" && i + 1 < args.size()) {
            project = args[++i];
        } else if (a == "--role" && i + 1 < args.size()) {
            role = args[++i];
        } else if (a == "--semantic") {
            semantic_only = true;
        } else if (a == "--keyword") {
            keyword_only = true;
        } else {
            terms.push_back(a);
        }
    }

    if (terms.empty()) {
        std::fprintf(stderr,
                     "usage: cml search <terms> [--project P] [--role R] [--limit N] "
                     "[--semantic|--keyword]\n");
        return 2;
    }
    Db db = open_db();
    if (!db) {
        std::fprintf(stderr, "cml: cannot open index\n");
        return 1;
    }

    std::string joined;
    for (const auto& t : terms) {
        if (!joined.empty()) joined += ' ';
        joined += t;
    }
    // `--semantic` refuses loudly rather than silently returning keyword hits.
    std::string err;
    const auto ranked = rank_rowids(db, semantic_only ? std::string{} : fts_query(terms),
                                    keyword_only ? std::string{} : joined, kCandidates, &err);
    if (ranked.empty() && semantic_only && !err.empty()) {
        std::fprintf(stderr, "cml: %s\n", err.c_str());
        return 1;
    }

    const auto gists = gist_lookup(db);
    Stmt fetch(db,
               "SELECT ts, role, project, session, substr(text,1,170), substr(text,1,64) "
               "FROM mem WHERE rowid=?1");
    const std::string want_project = lower(project);

    std::size_t printed = 0;
    for (const std::int64_t rowid : ranked) {
        if (printed >= limit) break;
        fetch.reset();
        fetch.bind(1, rowid);
        if (!fetch.step()) continue;

        const std::string ts = fetch.text(0);
        const std::string rrole = fetch.text(1);
        const std::string proj = fetch.text(2);
        const std::string sess = fetch.text(3);
        const std::string raw = fetch.text(4);
        const std::string head = fetch.text(5);

        if (!want_project.empty() && lower(proj).find(want_project) == std::string::npos) continue;
        if (!role.empty() && rrole != role) continue;

        const auto it = gists.find(stable_key(sess, ts, rrole, head));
        // squeeze keeps the row on one line; 170 is what the SELECT already clipped to,
        // so nothing here re-clips.
        const std::string flat = squeeze((it != gists.end()) ? it->second : raw, 170);

        const std::string date = (ts.size() >= 10) ? ts.substr(0, 10) : "no-date   ";
        std::printf("%s %s %s %s | %s\n", date.c_str(), pad(rrole, 9).c_str(),
                    pad(proj, 14).c_str(), utf8_take(sess, 8).c_str(), flat.c_str());
        ++printed;
    }

    if (printed == 0) {
        std::printf("no hits for: %s (%s)\n", joined.c_str(),
                    (keyword_only || !err.empty()) ? "keyword" : "hybrid");
    }
    return 0;
}

}  // namespace cml
