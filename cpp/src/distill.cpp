// distill: an LLM judges what deserves memory; junk gets forgotten.
//
// The transport is the `curl` binary, not libcurl — same as the Rust binary, and for
// the same reason: one POST every twenty rows does not justify a link-time dependency
// (libcurl's headers are not even installed here, only its runtime .so). exec'd
// directly with an argv, never through a shell: the key comes from a file on disk and
// the endpoint from the environment, and neither should ever meet sh.

#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <string>
#include <unordered_map>
#include <unordered_set>
#include <vector>

#include "curate.hpp"
#include "distillwire.hpp"
#include "curatorprompts.hpp"
#include "db.hpp"
#include "paths.hpp"
#include "utf8.hpp"

namespace cml {
namespace {

constexpr const char* kDistillModel = "deepseek-v4-pro";
constexpr std::size_t kBatch = 20;

// The rows each rubric owns. User rows were excluded until Jul 26 2026, which left
// every message the developer typed judged by nothing but a 34-word ack list and a
// four-word floor — "continue please" was caught, "continue please im sorry" was not.
constexpr const char* kAssistantRoles = "role IN ('assistant','summary')";
constexpr const char* kUserRoles = "role = 'user'";

std::string env_or(const char* name, const std::string& fallback) {
    const char* v = std::getenv(name);
    return (v && *v) ? std::string(v) : fallback;
}

std::string trimmed_file(const std::filesystem::path& p) {
    std::ifstream in(p);
    if (!in) return {};
    std::string s((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
    const auto a = s.find_first_not_of(" \t\r\n");
    if (a == std::string::npos) return {};
    return s.substr(a, s.find_last_not_of(" \t\r\n") - a + 1);
}

}  // namespace

std::optional<std::string> llm_key() {
    if (const char* v = std::getenv("CML_LLM_KEY"); v && *v) return std::string(v);
    for (const char* f : {"llm.key", "deepseek.key"}) {
        const std::string s = trimmed_file(data_dir() / f);
        if (!s.empty()) return s;
    }
    if (const char* v = std::getenv("DEEPSEEK_API_KEY"); v && *v) return std::string(v);
    return std::nullopt;
}

std::pair<std::string, std::string> llm_conf() {
    return {env_or("CML_LLM_URL", "https://api.deepseek.com/chat/completions"),
            env_or("CML_LLM_MODEL", kDistillModel)};
}

std::pair<std::size_t, std::size_t> distill_new(Db& db, const std::string& key,
                                                std::optional<std::size_t> cap, bool verbose,
                                                Rubric rubric) {
    std::unordered_set<std::string> judged;
    {
        Stmt s(db, "SELECT key FROM distilled");
        while (s.step()) judged.insert(s.text(0));
    }

    // Read the whole worklist before touching anything: the loop below deletes rows.
    std::vector<std::pair<std::int64_t, std::string>> todo;
    {
        Stmt s(db,
               std::string("SELECT rowid, substr(text,1,500), session || '|' || ts || '|' || role "
                           "FROM mem WHERE ") +
                   (rubric == Rubric::User ? kUserRoles : kAssistantRoles));
        while (s.step()) {
            std::string text = s.text(1);
            if (judged.count(s.text(2) + "|" + utf8_take(text, 64))) continue;
            todo.emplace_back(s.i64(0), std::move(text));
            if (cap && todo.size() >= *cap) break;
        }
    }
    if (todo.empty()) return {0, 0};

    const std::size_t total = (todo.size() + kBatch - 1) / kBatch;
    std::size_t kept = 0, dropped = 0, minted = 0;
    for (std::size_t bi = 0; bi < total; ++bi) {
        const auto first = todo.begin() + static_cast<std::ptrdiff_t>(bi * kBatch);
        const auto last = (bi + 1) * kBatch >= todo.size() ? todo.end() : first + kBatch;
        const std::vector<std::pair<std::int64_t, std::string>> batch(first, last);

        std::string err;
        const auto verdicts = judge_batch(key, batch, err, rubric);
        if (!verdicts) {
            if (verbose) {
                std::fprintf(stderr,
                             "batch %zu/%zu skipped (%s) — rows stay, retried next run\n",
                             bi + 1, total, err.c_str());
            }
            continue;
        }
        // Second pass: the rows that survived keep are re-asked, on their own, whether they
        // earn a gist. Folding this into the keep call was measured at 45-84% minted against
        // 16% for the same clause asked separately — a generous keep list drags the gist
        // decision up with it. A failure here costs nothing: the row is stored gistless,
        // still searchable, simply not plotted.
        std::unordered_map<std::int64_t, std::string> durable;
        {
            std::vector<std::pair<std::int64_t, std::string>> survivors;
            for (const auto& v : *verdicts) {
                if (!v.keep) continue;
                for (const auto& [rid, text] : batch) {
                    if (rid == v.id) survivors.emplace_back(rid, text);
                }
            }
            if (!survivors.empty()) {
                std::string derr;
                if (const auto dv = judge_batch(key, survivors, derr, Rubric::Durability)) {
                    for (const auto& d : *dv) {
                        if (!d.gist.empty()) durable.emplace(d.id, d.gist);
                        // Expansions land straight on the row: they are search surface,
                        // not a verdict, so they are stored where FTS5 will index them.
                        if (!d.asks.empty()) {
                            Stmt up(db, "UPDATE mem SET asks=?1 WHERE rowid=?2");
                            up.bind(1, d.asks).bind(2, d.id);
                            up.run();
                        }
                    }
                } else if (verbose) {
                    std::fprintf(stderr, "  durability pass skipped (%s) — rows kept gistless\n",
                                 derr.c_str());
                }
            }
        }

        for (const auto& [id, keep, gist, asks] : *verdicts) {
            std::string s, t, ro, h;
            {
                Stmt row(db, "SELECT session, ts, role, substr(text,1,64) FROM mem WHERE rowid=?1");
                row.bind(1, id);
                if (!row.step()) continue;
                s = row.text(0);
                t = row.text(1);
                ro = row.text(2);
                h = row.text(3);
            }
            // A "drop" verdict demotes, it does not delete. Measured: deleting them
            // cost recall@3 19.7% -> 14.6%, and destroyed 547 of 821 question/answer
            // pairs outright — two thirds of the history had no answer left to find.
            // The curator is good at judging what deserves a permanent gist and has no
            // business deciding what still exists, because it judges a row before the
            // question that needs it has been asked. Recorded either way so the row is
            // not re-judged every run; `cml forget` remains the deliberate, manual
            // delete.
            const auto d = durable.find(id);
            Stmt ins(db, "INSERT OR REPLACE INTO distilled(key, gist) VALUES (?1, ?2)");
            ins.bind(1, stable_key(s, t, ro, h))
               .bind(2, (keep && d != durable.end()) ? d->second : "");
            ins.run();
            if (keep) {
                ++kept;
                if (d != durable.end()) ++minted;
            } else {
                ++dropped;
            }
        }
        if (verbose) {
            std::printf("batch %zu/%zu: kept %zu (%zu durable), dropped %zu so far\n", bi + 1,
                        total, kept, minted, dropped);
            std::fflush(stdout);  // progress is the point; a pipe would hold it to the end
        }
    }
    return {kept, dropped};
}

int distill(const std::vector<std::string>& args) {
    const auto key = llm_key();
    if (!key) {
        std::fprintf(stderr,
                     "cml: no curator key — put one in ~/.claude/claude-memory-light/llm.key "
                     "(or set CML_LLM_KEY)\n");
        return 1;
    }
    Db db = open_db();
    if (!db) {
        std::fprintf(stderr, "cml: cannot open index\n");
        return 1;
    }
    for (const auto& a : args) {
        if (a != "--all") continue;
        db.exec("DELETE FROM distilled");
        std::printf("re-judging every row from scratch (%d prior verdicts cleared)\n",
                    sqlite3_changes(db.raw()));
        break;
    }
    // Wall clock, not CPU: nearly all of it is spent waiting on the endpoint.
    const auto t0 = std::chrono::steady_clock::now();
    auto [kept, dropped] = distill_new(db, *key, std::nullopt, true, Rubric::Assistant);
    const auto [ukept, udropped] = distill_new(db, *key, std::nullopt, true, Rubric::User);
    kept += ukept;
    dropped += udropped;
    const std::chrono::duration<double> secs = std::chrono::steady_clock::now() - t0;
    std::printf("distilled: kept %zu, dropped %zu in %.0fs (%s); dropped rows are blocklisted "
                "(undo: cml forget --clear)\n",
                kept, dropped, secs.count(), llm_conf().second.c_str());
    return 0;
}

}  // namespace cml
