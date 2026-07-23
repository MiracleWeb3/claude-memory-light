// Keyword search over the FTS5 index.
//
// The Rust binary fuses two legs — BM25 keyword plus a local-embedding KNN — with
// reciprocal rank fusion. Only the keyword leg is ported so far, so the fusion is
// kept (it is four lines and the shape must not drift) while `--semantic` refuses
// loudly rather than silently returning keyword hits under a semantic flag.

#include <algorithm>
#include <cstdio>
#include <string>
#include <unordered_map>
#include <vector>

#include "db.hpp"
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

    // Keyword leg: FTS5 BM25.
    std::vector<std::int64_t> hits;
    if (!semantic_only) {
        Stmt s(db, "SELECT rowid FROM mem WHERE mem MATCH ?1 ORDER BY rank LIMIT ?2");
        if (!s) {
            std::fprintf(stderr, "cml: %s\n", db.error().c_str());
            return 1;
        }
        s.bind(1, fts_query(terms)).bind(2, kCandidates);
        while (s.step()) hits.push_back(s.i64(0));
    }

    // Semantic leg: local embeddings + sqlite-vec KNN. Hybrid by default once
    // `cml embed` has run; a hard failure only when --semantic was demanded.
    std::vector<std::int64_t> vec_hits;
    if (!keyword_only) {
        std::string joined;
        for (const auto& t : terms) {
            if (!joined.empty()) joined += ' ';
            joined += t;
        }
        std::string err;
        vec_hits = semantic_hits(db, joined, kCandidates, err);
        if (vec_hits.empty() && semantic_only) {
            std::fprintf(stderr, "cml: %s\n", err.c_str());
            return 1;
        }
    }

    // Reciprocal rank fusion across both legs.
    std::unordered_map<std::int64_t, double> score;
    for (std::size_t i = 0; i < hits.size(); ++i) {
        score[hits[i]] += 1.0 / (kRrfK + static_cast<double>(i));
    }
    for (std::size_t i = 0; i < vec_hits.size(); ++i) {
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

    const auto gists = gist_lookup(db);
    Stmt fetch(db,
               "SELECT ts, role, project, session, substr(text,1,170), substr(text,1,64) "
               "FROM mem WHERE rowid=?1");
    const std::string want_project = lower(project);

    std::size_t printed = 0;
    for (const auto& [rowid, _] : ranked) {
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
        std::string snip = (it != gists.end()) ? it->second : raw;

        // collapse whitespace so a row stays one line
        std::string flat;
        bool gap = true;
        for (const char c : snip) {
            if (std::isspace(static_cast<unsigned char>(c))) { gap = true; continue; }
            if (gap && !flat.empty()) flat.push_back(' ');
            gap = false;
            flat.push_back(c);
        }

        const std::string date = (ts.size() >= 10) ? ts.substr(0, 10) : "no-date   ";
        std::printf("%s %s %s %s | %s\n", date.c_str(), pad(rrole, 9).c_str(),
                    pad(proj, 14).c_str(), utf8_take(sess, 8).c_str(), flat.c_str());
        ++printed;
    }

    if (printed == 0) {
        std::string joined;
        for (const auto& t : terms) {
            if (!joined.empty()) joined += ' ';
            joined += t;
        }
        std::printf("no hits for: %s (%s)\n", joined.c_str(),
                    vec_hits.empty() ? "keyword" : "hybrid");
    }
    return 0;
}

}  // namespace cml
