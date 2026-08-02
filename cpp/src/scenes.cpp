#include "scenes.hpp"

#include <algorithm>
#include <cctype>
#include <cstdio>
#include <string>
#include <unordered_map>
#include <unordered_set>
#include <utility>
#include <vector>

#include "curate.hpp"
#include "db.hpp"
#include "distillwire.hpp"
#include "noise.hpp"

namespace cml {
namespace {

// Four sessions per call, not twenty. judge_batch runs `curl --max-time 90`
// (distillwire.cpp) and distill_new's kBatch = 20 is sized for 500-char rows; twenty
// digests of this size is a ~120 KB request asking for twenty summaries, which does not
// come back inside the timeout — and a timeout costs the whole call.
constexpr std::size_t kBatch = 4;
constexpr std::size_t kDigest = 6000;  // chars of digest per session
constexpr std::size_t kRow = 300;      // chars of a row with no gist of its own

// The conversation roles, matching stats() (report.cpp:63-64). Without this filter the
// `memory` and `wiki` rows — 194 hand-written note files, not conversation — group into
// a 77th session, and the curator is paid to summarise fragments of MEMORY.md.
constexpr const char* kEligible =
    "session != '' AND role IN ('user','assistant','summary')";

struct Session {
    std::string id, project, ts_start, ts_end, digest;
    std::int64_t rows = 0;
};

// Newest first: the recent sessions are both the likeliest to be recalled and the
// hardest case for the rubric, so a --limit run calibrates against dense build work
// rather than whatever happens to sort first.
std::vector<Session> pending(Db& db, std::optional<std::size_t> cap) {
    std::unordered_set<std::string> done;
    {
        Stmt s(db, "SELECT session FROM scene");
        while (s.step()) done.insert(s.text(0));
    }
    std::vector<Session> out;
    Stmt s(db, std::string("SELECT session, project, min(ts), max(ts), count(*) FROM mem WHERE ") +
                   kEligible + " GROUP BY session HAVING count(*) >= 5 ORDER BY max(ts) DESC");
    while (s.step()) {
        if (done.count(s.text(0))) continue;
        out.push_back({s.text(0), s.text(1), s.text(2), s.text(3), {}, s.i64(4)});
        if (cap && out.size() >= *cap) break;
    }
    return out;
}

std::string lower(std::string s) {
    std::transform(s.begin(), s.end(), s.begin(),
                   [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    return s;
}

}  // namespace

// Over the cap this keeps the FIRST TWO ROWS AND THE ENDING, never a prefix. 27 of 76
// sessions here exceed 6,000 chars (median 4,460, p90 17,575, max 67,395), and they are
// the long ones a scene is most for. `outcome` is decided by how a session ended, so a
// prefix cap would judge the 282-row session on its first 9% and then ask it how things
// turned out.
std::string session_digest(Db& db, const std::string& session,
                           const std::unordered_map<std::string, std::string>& gists) {
    std::vector<std::string> lines;
    std::size_t bytes = 0;
    Stmt s(db, std::string("SELECT ts, role, substr(text,1,") + std::to_string(kRow) +
                   "), substr(text,1,64) FROM mem WHERE session=?1 AND " + kEligible +
                   " ORDER BY ts");
    s.bind(1, session);
    while (s.step()) {
        const std::string ts = s.text(0), role = s.text(1), text = s.text(2), head = s.text(3);
        const auto g = gists.find(stable_key(session, ts, role, head));
        lines.push_back(role + ": " + squeeze(g != gists.end() ? g->second : text, kRow));
        bytes += lines.back().size() + 1;
    }
    if (lines.empty()) return {};

    // Rows [tail, end) are the ending that survives; tail == 2 means nothing was cut.
    std::size_t tail = 2;
    if (bytes > kDigest) {
        tail = lines.size();
        // Two rows plus one more cannot reach the cap (a row is clipped to 300 chars),
        // so the ending is never the part that gets dropped.
        std::size_t used = lines[0].size() + lines[1].size() + 2;
        while (tail > 2 && used + lines[tail - 1].size() + 1 <= kDigest) {
            used += lines[tail - 1].size() + 1;
            --tail;
        }
    }
    std::string out;
    for (std::size_t i = 0; i < lines.size(); ++i) {
        if (i == 2 && tail > 2) {
            out += "[... " + std::to_string(tail - 2) + " turns omitted ...]\n";
            i = tail - 1;
            continue;
        }
        out += lines[i];
        out += '\n';
    }
    return out;
}

std::size_t build_scenes(Db& db, const std::string& key, std::optional<std::size_t> cap,
                         bool verbose) {
    auto todo = pending(db, cap);
    if (todo.empty()) return 0;
    {
        const auto gists = gist_lookup(db);
        for (auto& s : todo) s.digest = session_digest(db, s.id, gists);
    }

    const std::size_t total = (todo.size() + kBatch - 1) / kBatch;
    std::size_t written = 0;
    for (std::size_t bi = 0; bi < total; ++bi) {
        const std::size_t lo = bi * kBatch, hi = std::min(lo + kBatch, todo.size());
        std::vector<std::pair<std::int64_t, std::string>> rows;
        for (std::size_t i = lo; i < hi; ++i) {
            rows.emplace_back(static_cast<std::int64_t>(i), todo[i].digest);
        }

        std::string err;
        const auto scenes = judge_scenes(key, rows, err);
        if (!scenes) {
            if (verbose) {
                std::fprintf(stderr,
                             "batch %zu/%zu skipped (%s) — sessions stay, retried next run\n",
                             bi + 1, total, err.c_str());
            }
            continue;
        }
        // `scene` has no PRIMARY KEY and pending() skips whatever is already in it, so a
        // wrong id is written once and never revisited. An id outside the batch is a
        // hallucination that would attach this summary to a session nobody asked about;
        // a repeated one would give a session two contradictory scenes.
        std::vector<bool> seen(hi - lo, false);
        for (const auto& sc : *scenes) {
            const auto i = static_cast<std::size_t>(sc.id);
            if (sc.id < 0 || i < lo || i >= hi || seen[i - lo]) continue;
            seen[i - lo] = true;
            const Session& s = todo[i];
            Stmt ins(db,
                     "INSERT INTO scene(title,summary,outcome,session,project,ts_start,ts_end,"
                     "n_rows) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)");
            ins.bind(1, sc.title)
                .bind(2, sc.summary)
                .bind(3, lower(sc.outcome))
                .bind(4, s.id)
                .bind(5, s.project)
                .bind(6, s.ts_start)
                .bind(7, s.ts_end)
                .bind(8, s.rows);
            if (ins.run()) ++written;
        }
        if (verbose) {
            std::printf("batch %zu/%zu: %zu of %zu sessions summarised so far\n", bi + 1, total,
                        written, todo.size());
            std::fflush(stdout);  // progress is the point; a pipe would hold it to the end
        }
    }
    return written;
}

}  // namespace cml
