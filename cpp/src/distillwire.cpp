#include "distillwire.hpp"

#include <fcntl.h>

#include <cstdlib>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#include <simdjson.h>

#include <filesystem>
#include <fstream>

#include "json.hpp"
#include "paths.hpp"

namespace cml {
namespace {

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

}  // namespace

// One call judging a batch of rows. nullopt with `err` set on any failure; the
// caller leaves those rows unjudged so the next run retries them.
std::optional<std::vector<Verdict>> judge_batch(
    const std::string& key, const std::vector<std::pair<std::int64_t, std::string>>& rows,
    std::string& err, Rubric rubric) {
    const auto [url, model] = llm_conf();
    const auto tmp = data_dir() / ".distill-req.json";
    { std::ofstream(tmp, std::ios::binary) << build_judge_request(model, rows, rubric); }

    // The key goes in a --config file, never in argv. /proc/<pid>/cmdline is world
    // readable on Linux, so passing it as `-H "Authorization: Bearer sk-..."` published
    // the user's key to every process on the machine for the life of the request — it
    // was plainly visible in `ps` output. The file is created 0600 before anything is
    // written to it, so there is no window where it exists with looser permissions.
    const auto cfg = data_dir() / ".distill-auth";
    {
        std::error_code ec;
        std::filesystem::remove(cfg, ec);
        const int fd = ::open(cfg.c_str(), O_WRONLY | O_CREAT | O_EXCL, S_IRUSR | S_IWUSR);
        if (fd < 0) {
            err = "cannot create a private file for the API key";
            std::filesystem::remove(tmp, ec);
            return std::nullopt;
        }
        const std::string line = "header = \"Authorization: Bearer " + key + "\"\n";
        const bool wrote = ::write(fd, line.data(), line.size()) == static_cast<ssize_t>(line.size());
        ::close(fd);
        if (!wrote) {
            err = "cannot write the API key config";
            std::filesystem::remove(cfg, ec);
            std::filesystem::remove(tmp, ec);
            return std::nullopt;
        }
    }
    // 90s was fine for an interactive `cml distill`; it is not fine on the Stop hook,
    // where two of these stacked into a 3m24s turn on 2026-08-02. index_all sets
    // CML_HTTP_TIMEOUT to whatever is left of its budget.
    const char* tmo_env = std::getenv("CML_HTTP_TIMEOUT");
    const std::string tmo = (tmo_env && std::atoi(tmo_env) > 0) ? tmo_env : "90";
    std::string out = run_capture({"curl", "-s", "--max-time", tmo, "--config",
                                   cfg.string(), "-H", "Content-Type: application/json", "-d",
                                   "@" + tmp.string(), url});
    std::error_code ec;
    std::filesystem::remove(cfg, ec);
    std::filesystem::remove(tmp, ec);
    return parse_verdicts(out, err);
}

std::string build_judge_request(const std::string& model,
                               const std::vector<std::pair<std::int64_t, std::string>>& rows,
                               Rubric rubric) {
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
    json::quote_into(req, std::string(curator_prompt(rubric)));
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
            // asks[] is flattened to one space-joined string: it goes into a single FTS5
            // column, where the separation between phrasings carries no meaning anyway.
            std::string asks;
            simdjson::dom::array arr;
            if (v["asks"].get(arr) == simdjson::SUCCESS) {
                for (const auto a : arr) {
                    std::string_view one;
                    if (a.get(one) != simdjson::SUCCESS || one.empty()) continue;
                    if (!asks.empty()) asks += ' ';
                    asks.append(one);
                }
            }
            verdicts.push_back({id, keep, std::string(gist), std::move(asks)});
        }
    }
    return verdicts;
}

}  // namespace cml
