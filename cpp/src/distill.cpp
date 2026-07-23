// distill: an LLM judges what deserves memory; junk gets forgotten.
//
// The transport is the `curl` binary, not libcurl — same as the Rust binary, and for
// the same reason: one POST every twenty rows does not justify a link-time dependency
// (libcurl's headers are not even installed here, only its runtime .so). exec'd
// directly with an argv, never through a shell: the key comes from a file on disk and
// the endpoint from the environment, and neither should ever meet sh.

#include <sys/wait.h>
#include <unistd.h>

#include <simdjson.h>

#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <string>
#include <unordered_set>
#include <vector>

#include "curate.hpp"
#include "db.hpp"
#include "json.hpp"
#include "paths.hpp"
#include "utf8.hpp"

namespace cml {
namespace {

constexpr const char* kDistillModel = "deepseek-v4-pro";
constexpr std::size_t kBatch = 20;

// Carried over verbatim from the Rust implementation: the curator's entire
// behaviour lives in this string, so edits to it are behaviour changes.
constexpr const char* kSystemPrompt = R"CML(You curate a developer's AI-assistant history into long-term memory. Only content with real, specific information — logical sense that stands on its own — belongs here. Everything else is noise and must be dropped.
KEEP (keep=true): a specific fact; a root cause; a decision and its reason; an explanation of how something works; a non-obvious gotcha or finding; a real measurement; or a recorded user preference/correction. Read the row alone — if you learn something concrete and reusable, keep it.
DROP (keep=false): generic phrases that carry no information ('what are we building?', 'here is where things stand', 'let me check', 'sounds good'); status and progress chatter ('doing X now', 'blocked on Y', 'walking it once more'); completion and acknowledgment reports ('Done', 'Fixed', 'Pushed', 'Set X to Y' — even with a value); pleasantries; and any row that would be meaningless out of its moment.
THE TEST: read the row by itself. Does it state specific, reusable information, or is it a generic / process / status phrase? Generic, process, or noise → DROP. When unsure, DROP.
For kept rows add gist: the reusable essence in at most 120 characters.
Reply ONLY with JSON: {"verdicts":[{"id":<id>,"keep":true|false,"gist":"..."}]})CML";

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

// Runs argv and returns its stdout. Empty on any failure, which the caller reports
// as an unreadable response — the same shape a network error takes.
std::string run_capture(const std::vector<std::string>& argv) {
    int fds[2];
    if (pipe(fds) != 0) return {};
    const pid_t pid = fork();
    if (pid < 0) {
        close(fds[0]);
        close(fds[1]);
        return {};
    }
    if (pid == 0) {
        close(fds[0]);
        dup2(fds[1], STDOUT_FILENO);
        close(fds[1]);
        std::vector<char*> raw;
        raw.reserve(argv.size() + 1);
        for (const auto& a : argv) raw.push_back(const_cast<char*>(a.c_str()));
        raw.push_back(nullptr);
        execvp(raw[0], raw.data());
        _exit(127);
    }
    close(fds[1]);
    std::string out;
    char buf[8192];
    for (ssize_t n; (n = read(fds[0], buf, sizeof buf)) > 0;) {
        out.append(buf, static_cast<std::size_t>(n));
    }
    close(fds[0]);
    int status = 0;
    waitpid(pid, &status, 0);
    return out;
}

// One call judging a batch of rows. nullopt with `err` set on any failure; the
// caller leaves those rows unjudged so the next run retries them.
std::optional<std::vector<Verdict>> judge_batch(
    const std::string& key, const std::vector<std::pair<std::int64_t, std::string>>& rows,
    std::string& err) {
    const auto [url, model] = llm_conf();
    const auto tmp = data_dir() / ".distill-req.json";
    { std::ofstream(tmp, std::ios::binary) << build_judge_request(model, rows); }
    std::string out = run_capture({"curl", "-s", "--max-time", "90", "-H",
                                   "Content-Type: application/json", "-H",
                                   "Authorization: Bearer " + key, "-d",
                                   "@" + tmp.string(), url});
    std::error_code ec;
    std::filesystem::remove(tmp, ec);
    return parse_verdicts(out, err);
}

}  // namespace

std::string build_judge_request(const std::string& model,
                               const std::vector<std::pair<std::int64_t, std::string>>& rows) {
    std::string items = "{\"rows\":[";
    for (std::size_t i = 0; i < rows.size(); ++i) {
        if (i) items += ',';
        items += "{\"id\":" + std::to_string(rows[i].first) + ",\"text\":";
        json::quote_into(items, rows[i].second);
        items += '}';
    }
    items += "]}";

    // Field order is serde_json's (BTreeMap: alphabetical), so a diff of the two
    // binaries' requests shows nothing.
    std::string req = "{\"messages\":[{\"content\":";
    json::quote_into(req, kSystemPrompt);
    req += ",\"role\":\"system\"},{\"content\":";
    json::quote_into(req, items);
    req += ",\"role\":\"user\"}],\"model\":";
    json::quote_into(req, model);
    req += ",\"response_format\":{\"type\":\"json_object\"},\"temperature\":0.0}";
    return req;
}

std::optional<std::vector<Verdict>> parse_verdicts(std::string& response, std::string& err) {
    simdjson::dom::parser parser;
    simdjson::dom::element resp;
    if (const auto e = parser.parse(response).get(resp); e != simdjson::SUCCESS) {
        err = "deepseek response unreadable: " + std::string(simdjson::error_message(e));
        return std::nullopt;
    }
    std::string_view content;
    simdjson::dom::array choices;
    if (resp["choices"].get(choices) != simdjson::SUCCESS || choices.size() == 0 ||
        choices.at(0)["message"]["content"].get(content) != simdjson::SUCCESS) {
        std::string_view msg;
        if (resp["error"]["message"].get(msg) != simdjson::SUCCESS) msg = "no content";
        err = "deepseek error: " + std::string(msg);
        return std::nullopt;
    }

    // The model's answer is JSON inside a JSON string — parsed again, on its own.
    std::string body(content);
    simdjson::dom::parser inner;
    simdjson::dom::element parsed;
    if (const auto e = inner.parse(body).get(parsed); e != simdjson::SUCCESS) {
        err = "verdict json bad: " + std::string(simdjson::error_message(e));
        return std::nullopt;
    }
    std::vector<Verdict> verdicts;
    simdjson::dom::array arr;
    if (parsed["verdicts"].get(arr) == simdjson::SUCCESS) {
        for (auto v : arr) {
            std::int64_t id = 0;
            bool keep = false;
            if (v["id"].get(id) != simdjson::SUCCESS) continue;
            if (v["keep"].get(keep) != simdjson::SUCCESS) continue;
            std::string_view gist;
            if (v["gist"].get(gist) != simdjson::SUCCESS) gist = "";
            verdicts.push_back({id, keep, std::string(gist)});
        }
    }
    return verdicts;
}

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
                                                std::optional<std::size_t> cap, bool verbose) {
    std::unordered_set<std::string> judged;
    {
        Stmt s(db, "SELECT key FROM distilled");
        while (s.step()) judged.insert(s.text(0));
    }

    // Read the whole worklist before touching anything: the loop below deletes rows.
    std::vector<std::pair<std::int64_t, std::string>> todo;
    {
        Stmt s(db,
               "SELECT rowid, substr(text,1,500), session || '|' || ts || '|' || role FROM mem "
               "WHERE role IN ('assistant','summary')");
        while (s.step()) {
            std::string text = s.text(1);
            if (judged.count(s.text(2) + "|" + utf8_take(text, 64))) continue;
            todo.emplace_back(s.i64(0), std::move(text));
            if (cap && todo.size() >= *cap) break;
        }
    }
    if (todo.empty()) return {0, 0};

    const std::size_t total = (todo.size() + kBatch - 1) / kBatch;
    std::size_t kept = 0, dropped = 0;
    for (std::size_t bi = 0; bi < total; ++bi) {
        const auto first = todo.begin() + static_cast<std::ptrdiff_t>(bi * kBatch);
        const auto last = (bi + 1) * kBatch >= todo.size() ? todo.end() : first + kBatch;
        const std::vector<std::pair<std::int64_t, std::string>> batch(first, last);

        std::string err;
        const auto verdicts = judge_batch(key, batch, err);
        if (!verdicts) {
            if (verbose) {
                std::fprintf(stderr,
                             "batch %zu/%zu skipped (%s) — rows stay, retried next run\n",
                             bi + 1, total, err.c_str());
            }
            continue;
        }
        for (const auto& [id, keep, gist] : *verdicts) {
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
            if (keep) {
                Stmt ins(db, "INSERT OR REPLACE INTO distilled(key, gist) VALUES (?1, ?2)");
                ins.bind(1, stable_key(s, t, ro, h)).bind(2, gist);
                ins.run();
                ++kept;
            } else if (purge_rowid(db, id)) {
                ++dropped;
            }
        }
        if (verbose) {
            std::printf("batch %zu/%zu: kept %zu, dropped %zu so far\n", bi + 1, total, kept,
                        dropped);
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
    const auto [kept, dropped] = distill_new(db, *key, std::nullopt, true);
    const std::chrono::duration<double> secs = std::chrono::steady_clock::now() - t0;
    std::printf("distilled: kept %zu, dropped %zu in %.0fs (%s); dropped rows are blocklisted "
                "(undo: cml forget --clear)\n",
                kept, dropped, secs.count(), llm_conf().second.c_str());
    return 0;
}

}  // namespace cml
