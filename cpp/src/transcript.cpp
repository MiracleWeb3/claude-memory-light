#include "transcript.hpp"

#include <simdjson.h>

#include <algorithm>
#include <cctype>
#include <fstream>

namespace cml {
namespace {

std::string text_of(simdjson::dom::element content) {
    if (content.is_string()) return std::string(content.get_string().value_unsafe());
    simdjson::dom::array arr;
    if (content.get(arr) != simdjson::SUCCESS) return {};

    std::string out;
    for (auto block : arr) {
        std::string_view type;
        if (block["type"].get(type) != simdjson::SUCCESS || type != "text") continue;
        std::string_view txt;
        if (block["text"].get(txt) != simdjson::SUCCESS) continue;
        if (!out.empty()) out.push_back(' ');
        out.append(txt);
    }
    return out;
}

bool has_block(simdjson::dom::element content, std::string_view kind) {
    simdjson::dom::array arr;
    if (content.get(arr) != simdjson::SUCCESS) return false;
    for (auto block : arr) {
        std::string_view type;
        if (block["type"].get(type) == simdjson::SUCCESS && type == kind) return true;
    }
    return false;
}

bool blank(std::string_view s) {
    return std::all_of(s.begin(), s.end(),
                       [](unsigned char c) { return std::isspace(c); });
}

std::string sv_or(simdjson::dom::element v, const char* key, const std::string& fallback) {
    std::string_view out;
    if (v[key].get(out) == simdjson::SUCCESS && !out.empty()) return std::string(out);
    return fallback;
}

}  // namespace

std::vector<Entry> parse_transcript(const std::string& path,
                                    const std::string& session_fallback) {
    std::vector<Entry> entries;
    std::ifstream in(path);
    if (!in) return entries;

    simdjson::dom::parser parser;
    std::string line;
    while (std::getline(in, line)) {
        if (line.empty()) continue;
        simdjson::dom::element v;
        if (parser.parse(line).get(v) != simdjson::SUCCESS) continue;

        std::string_view type;
        if (v["type"].get(type) != simdjson::SUCCESS) continue;

        if (type == "summary") {
            std::string_view s;
            if (v["summary"].get(s) == simdjson::SUCCESS && !blank(s)) {
                entries.push_back({Entry::Kind::Summary, std::string(s), "", session_fallback});
            }
            continue;
        }
        if (type != "user" && type != "assistant") continue;

        bool sidechain = false;
        if (v["isSidechain"].get(sidechain) == simdjson::SUCCESS && sidechain) continue;

        simdjson::dom::element content;
        if (v["message"]["content"].get(content) != simdjson::SUCCESS) continue;

        const std::string text = text_of(content);
        const std::string ts = sv_or(v, "timestamp", "");
        const std::string sid = sv_or(v, "sessionId", session_fallback);

        if (type == "assistant") {
            if (has_block(content, "tool_use")) {
                entries.push_back({Entry::Kind::AssistantTool, "", "", ""});
            } else if (!blank(text)) {
                entries.push_back({Entry::Kind::AssistantText, text, ts, sid});
            }
        } else if (has_block(content, "tool_result")) {
            entries.push_back({Entry::Kind::UserTool, "", "", ""});
        } else if (!blank(text)) {
            entries.push_back({Entry::Kind::UserHuman, text, ts, sid});
        }
    }
    return entries;
}

bool turn_final(const std::vector<Entry>& entries, std::size_t i) {
    for (std::size_t j = i + 1; j < entries.size(); ++j) {
        switch (entries[j].kind) {
            case Entry::Kind::AssistantText:
                continue;  // a later text in the same turn; keep looking
            case Entry::Kind::AssistantTool:
            case Entry::Kind::UserTool:
                return false;  // work followed, so this was narration
            case Entry::Kind::UserHuman:
            case Entry::Kind::Summary:
                return true;  // the turn ended here
        }
    }
    return true;
}

}  // namespace cml
