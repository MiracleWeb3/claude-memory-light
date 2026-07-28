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


// The command that ran, and what it printed.
//
// Assistant text is the conversation ABOUT the work; this is the work. Measured on a
// real transcript: 741 kB of assistant text kept, 2751 kB of tool traffic discarded —
// 3.7x more, and it is the half holding what people actually search for months later.
// "ModuleNotFoundError: No module named 'curl_cffi'" appears nowhere except here.
//
// Capped, because a file read can be a megabyte and a megabyte of source is not a
// memory. The cap keeps the errno and the first lines of a stack trace, which is the
// part that identifies the failure.
constexpr std::size_t kToolMax = 1200;

void append_capped(std::string& out, std::string_view s) {
    if (out.size() >= kToolMax) return;
    if (!out.empty()) out.push_back(' ');
    out.append(s.substr(0, kToolMax - out.size()));
}

// Tool name plus its string arguments: the command, the path, the pattern. Non-string
// arguments are skipped — a Write's `content` is the file, not a description of it.
std::string tool_use_text(simdjson::dom::element content) {
    simdjson::dom::array arr;
    if (content.get(arr) != simdjson::SUCCESS) return {};
    std::string out;
    for (auto b : arr) {
        std::string_view t;
        if (b["type"].get(t) != simdjson::SUCCESS || t != "tool_use") continue;
        std::string_view name;
        if (b["name"].get(name) == simdjson::SUCCESS) append_capped(out, name);
        simdjson::dom::object input;
        if (b["input"].get(input) != simdjson::SUCCESS) continue;
        for (auto field : input) {
            std::string_view v;
            if (field.value.get(v) == simdjson::SUCCESS) append_capped(out, v);
        }
    }
    return out;
}

// What came back. `content` is either a plain string or an array of text blocks.
std::string tool_result_text(simdjson::dom::element content) {
    simdjson::dom::array arr;
    if (content.get(arr) != simdjson::SUCCESS) return {};
    std::string out;
    for (auto b : arr) {
        std::string_view t;
        if (b["type"].get(t) != simdjson::SUCCESS || t != "tool_result") continue;
        std::string_view s;
        if (b["content"].get(s) == simdjson::SUCCESS) {
            append_capped(out, s);
            continue;
        }
        simdjson::dom::array inner;
        if (b["content"].get(inner) != simdjson::SUCCESS) continue;
        for (auto ib : inner) {
            std::string_view is;
            if (ib["text"].get(is) == simdjson::SUCCESS) append_capped(out, is);
        }
    }
    return out;
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
                entries.push_back({Entry::Kind::AssistantTool, tool_use_text(content), ts, sid});
            } else if (!blank(text)) {
                entries.push_back({Entry::Kind::AssistantText, text, ts, sid});
            }
        } else if (has_block(content, "tool_result")) {
            entries.push_back({Entry::Kind::UserTool, tool_result_text(content), ts, sid});
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
