// The SessionStart briefing hook.
//
// Split out of report.cpp, which had reached 289 lines holding three unrelated jobs:
// stats and doctor report to a human at a terminal, this reports to the model at the
// top of a session. The local json_escape died in the split: json::quote_into already
// existed, names the backspace and form-feed escapes instead of emitting them as
// numeric ones, and casts correctly before formatting.

#include <simdjson.h>

#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <string>
#include <vector>

#include "db.hpp"
#include "json.hpp"
#include "loops.hpp"
#include "noise.hpp"
#include "paths.hpp"
#include "search.hpp"

namespace fs = std::filesystem;

namespace cml {
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

    std::string out =
        "{\"continue\": true, \"hookSpecificOutput\": {\"hookEventName\": \"SessionStart\", "
        "\"additionalContext\": ";
    json::quote_into(out, msg);
    out += "}}";
    std::printf("%s\n", out.c_str());
}

}  // namespace cml
