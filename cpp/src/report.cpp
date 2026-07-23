// stats, doctor, and the SessionStart nudge.

#include <simdjson.h>

#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <string>

#include <vector>

#include "curate.hpp"
#include "db.hpp"
#include "embed.hpp"
#include "loops.hpp"
#include "noise.hpp"
#include "paths.hpp"
#include "search.hpp"

namespace fs = std::filesystem;

namespace cml {
namespace {

const char* mark(bool present) { return present ? "ok" : "MISSING"; }

// `which`, without spawning a shell.
std::string which(const char* exe) {
    const char* path = std::getenv("PATH");
    if (!path) return {};
    std::string p(path), item;
    std::size_t start = 0;
    while (start <= p.size()) {
        const std::size_t end = p.find(':', start);
        item = p.substr(start, end - start);
        if (!item.empty()) {
            std::error_code ec;
            const fs::path cand = fs::path(item) / exe;
            if (fs::is_regular_file(cand, ec)) return cand.string();
        }
        if (end == std::string::npos) break;
        start = end + 1;
    }
    return {};
}

// Escape a string for embedding in the JSON the hook prints.
std::string json_escape(std::string_view s) {
    std::string out;
    out.reserve(s.size() + 16);
    for (const char c : s) {
        switch (c) {
            case '"': out += "\\\""; break;
            case '\\': out += "\\\\"; break;
            case '\n': out += "\\n"; break;
            case '\r': out += "\\r"; break;
            case '\t': out += "\\t"; break;
            default:
                if (static_cast<unsigned char>(c) < 0x20) {
                    char buf[8];
                    std::snprintf(buf, sizeof buf, "\\u%04x", c);
                    out += buf;
                } else {
                    out.push_back(c);
                }
        }
    }
    return out;
}

}  // namespace

int stats() {
    Db db = open_db();
    if (!db) {
        std::fprintf(stderr, "cml: cannot open index\n");
        return 1;
    }
    const std::int64_t msgs = db.scalar("SELECT count(*) FROM mem");
    const std::int64_t sessions = db.scalar(
        "SELECT count(DISTINCT session) FROM mem WHERE role IN ('user','assistant','summary')");
    const std::int64_t files = db.scalar("SELECT count(*) FROM files");

    std::string by_role;
    {
        Stmt s(db, "SELECT role, count(*) FROM mem GROUP BY role ORDER BY 2 DESC");
        while (s.step()) {
            if (!by_role.empty()) by_role += ", ";
            by_role += s.text(0) + "=" + std::to_string(s.i64(1));
        }
    }

    const fs::path dbp = data_dir() / "index.db";
    std::error_code ec;
    const double mb = static_cast<double>(fs::file_size(dbp, ec)) / 1e6;

    std::printf("%lld rows (%s) | %lld sessions | %lld files | %.1f MB at %s\n",
                static_cast<long long>(msgs), by_role.c_str(),
                static_cast<long long>(sessions), static_cast<long long>(files),
                ec ? 0.0 : mb, dbp.string().c_str());
    return 0;
}

int doctor() {
    std::error_code ec;
    const fs::path projects = home() / ".claude/projects";
    const fs::path data = data_dir();
    const fs::path dbp = data / "index.db";

    std::printf("transcripts dir : %s (%s)\n", projects.string().c_str(),
                mark(fs::is_directory(projects, ec)));
    std::printf("data dir        : %s (%s)\n", data.string().c_str(),
                mark(fs::is_directory(data, ec)));
    std::printf("index db        : %s (%s)\n", dbp.string().c_str(),
                mark(fs::is_regular_file(dbp, ec)));

    const std::string g = which("graphify");
    std::printf("graphify        : %s (optional structural-memory companion)\n",
                g.empty() ? "not installed" : g.c_str());

    {
        Db db = open_db();
        if (db && vec_table_exists(db)) {
            const std::int64_t v = db.scalar("SELECT count(*) FROM vec_mem");
            const std::int64_t m = db.scalar("SELECT count(*) FROM mem");
            std::printf("semantic        : %lld/%lld rows embedded (%s)\n",
                        static_cast<long long>(v), static_cast<long long>(m),
                        embed_model_id().c_str());
        } else {
            std::printf("semantic        : off — run `cml embed` once to enable hybrid search\n");
        }
    }

    // Curation status. Omitting this is how I convinced myself curation was off when
    // a key had been sitting in llm.key since July — doctor is the only place that
    // reports it, so a missing line reads as a missing feature.
    {
        Db db = open_db();
        const std::int64_t judged = db ? db.scalar("SELECT count(*) FROM distilled") : 0;
        const std::int64_t forgot = db ? db.scalar("SELECT count(*) FROM forgotten") : 0;
        if (llm_key()) {
            const auto [url, model] = llm_conf();
            // host from "https://host/path"
            std::string host = url;
            const std::size_t s = host.find("//");
            if (s != std::string::npos) host = host.substr(s + 2);
            const std::size_t e = host.find('/');
            if (e != std::string::npos) host = host.substr(0, e);
            std::printf("curation        : on (%s @ %s) — %lld judged kept, %lld forgotten\n",
                        model.c_str(), host.empty() ? "?" : host.c_str(),
                        static_cast<long long>(judged), static_cast<long long>(forgot));
        } else {
            std::printf("curation        : off — put a key in ~/.claude/claude-memory-light/"
                        "llm.key (any OpenAI-compatible provider; CML_LLM_URL / CML_LLM_MODEL "
                        "to configure)\n");
        }
    }
    std::printf("hint: keep transcripts forever with \"cleanupPeriodDays\": 3650 in "
                "~/.claude/settings.json\n");

    if (fs::is_regular_file(dbp, ec)) return stats();
    std::printf("run `cml index` to build the index\n");
    return 0;
}

namespace {

// "name — first summary line" for each wiki page, one flat line, capped. The menu
// tells the model what it CAN recall; content stays on disk until asked for.
std::string wiki_topics(std::size_t cap) {
    std::vector<fs::path> pages;
    std::error_code ec;
    for (const auto& e : fs::directory_iterator(data_dir() / "wiki", ec))
        if (e.path().extension() == ".md") pages.push_back(e.path());
    std::sort(pages.begin(), pages.end());

    std::string out;
    std::size_t shown = 0;
    for (const auto& p : pages) {
        if (shown == cap) break;
        std::string summary, line;
        std::ifstream in(p);
        while (std::getline(in, line)) {
            const std::size_t i = line.find_first_not_of(" \t");
            if (i == std::string::npos || line[i] == '#') continue;
            summary = squeeze(line, 60);
            break;
        }
        if (!out.empty()) out += " | ";
        out += p.stem().string() + " — " + (summary.empty() ? "(empty)" : summary);
        ++shown;
    }
    if (pages.size() > shown)
        out += " | +" + std::to_string(pages.size() - shown) + " more";
    return out;
}

}  // namespace

void nudge() {
    // Whatever happens, the session must proceed.
    const auto passthrough = []() { std::printf("{\"continue\": true}\n"); };

    std::string payload((std::istreambuf_iterator<char>(std::cin)),
                        std::istreambuf_iterator<char>());
    if (payload.empty()) return passthrough();

    simdjson::dom::parser parser;
    simdjson::dom::element data;
    if (parser.parse(payload).get(data) != simdjson::SUCCESS) return passthrough();

    std::string_view cwd;
    if (data["cwd"].get(cwd) != simdjson::SUCCESS || cwd.empty()) return passthrough();

    // resume/compact already carry the conversation; re-injecting the static
    // sections would be the exact bloat this briefing is budgeted against.
    std::string_view source;
    if (data["source"].get(source) != simdjson::SUCCESS) source = {};
    const bool pointer = (source == "resume" || source == "compact");

    std::size_t threshold = 5;
    if (const char* t = std::getenv("CML_NUDGE_THRESHOLD")) {
        const long v = std::strtol(t, nullptr, 10);
        if (v > 0) threshold = static_cast<std::size_t>(v);
    }

    const fs::path inbox = inbox_path_for(cwd);
    std::size_t n = 0;
    {
        std::ifstream in(inbox);
        std::string line;
        while (std::getline(in, line)) {
            const std::size_t i = line.find_first_not_of(" \t");
            if (i != std::string::npos && line.compare(i, 3, "- [") == 0) ++n;
        }
    }

    std::vector<std::string> sections;
    if (n >= threshold)
        sections.push_back(
            "[claude-memory-light learning loop] " + std::to_string(n) +
            " raw signals captured since last consolidation (" + inbox.string() +
            "). Early this session, before deep work: read the inbox, distill any durable "
            "correction / preference / non-obvious workflow into persistent memory (and promote "
            "anything recurring into CLAUDE.md), then delete the consolidated lines. Drop the "
            "noise — most lines are nothing. Full-history recall is available via `cml search`.");

    if (!pointer) {
        if (Db db = open_db()) {
            const auto loops = loop_lines(db, 30, 3);
            if (!loops.empty()) {
                std::string s = "[cml] open loops — asks that keep coming back unresolved:";
                for (const auto& l : loops) s += " // " + l;
                sections.push_back(std::move(s));
            }
        }
        if (std::string topics = wiki_topics(8); !topics.empty())
            sections.push_back("[cml] wiki topics on file (recall: cml search --role wiki): " +
                               topics);
    }
    if (sections.empty()) return passthrough();

    std::string msg;
    for (const auto& s : sections) {
        if (!msg.empty()) msg += "\n\n";
        msg += s;
    }
    char meter[40];
    std::snprintf(meter, sizeof meter, " [context injected: %.1fkB]",
                  static_cast<double>(msg.size()) / 1000.0);
    msg += meter;

    std::printf(
        "{\"continue\": true, \"hookSpecificOutput\": {\"hookEventName\": \"SessionStart\", "
        "\"additionalContext\": \"%s\"}}\n",
        json_escape(msg).c_str());
}

}  // namespace cml
