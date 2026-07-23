#include "work.hpp"

#include <simdjson.h>

#include <algorithm>
#include <fstream>

#include "noise.hpp"

namespace cml {
namespace {

constexpr std::size_t kMaxFiles = 6;
constexpr std::size_t kMaxCmds = 4;
constexpr std::size_t kCmdChars = 48;
constexpr std::size_t kOutcomeChars = 220;

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

void push_unique(std::vector<std::string>& v, std::string item, std::size_t cap) {
    if (item.empty() || v.size() >= cap) return;
    if (std::find(v.begin(), v.end(), item) != v.end()) return;
    v.push_back(std::move(item));
}

// Strip the directory so "src/main.cpp" and a 90-char absolute path read the same.
std::string basename(std::string_view p) {
    const auto slash = p.find_last_of('/');
    return std::string(slash == std::string_view::npos ? p : p.substr(slash + 1));
}

// Record the verb, not the plumbing: first real command, no comments, no chains.
std::string head_of_command(std::string_view cmd) {
    std::size_t start = 0;
    while (start < cmd.size()) {
        std::size_t end = cmd.find_first_of("\n;", start);
        if (end == std::string_view::npos) end = cmd.size();
        std::string_view piece = cmd.substr(start, end - start);
        while (!piece.empty() && std::isspace(static_cast<unsigned char>(piece.front())))
            piece.remove_prefix(1);
        if (!piece.empty() && piece.front() != '#') return squeeze(piece, kCmdChars);
        start = end + 1;
    }
    return {};
}

void collect_tool_use(Digest& d, simdjson::dom::element block) {
    std::string_view name;
    if (block["name"].get(name) != simdjson::SUCCESS) return;
    simdjson::dom::element input;
    if (block["input"].get(input) != simdjson::SUCCESS) return;

    std::string_view sv;
    if (name == "Edit" || name == "Write" || name == "NotebookEdit" || name == "MultiEdit") {
        if (input["file_path"].get(sv) == simdjson::SUCCESS)
            push_unique(d.files, basename(sv), kMaxFiles);
    } else if (name == "Bash") {
        if (input["command"].get(sv) == simdjson::SUCCESS)
            push_unique(d.commands, head_of_command(sv), kMaxCmds);
    } else if (name == "Skill") {
        if (input["skill"].get(sv) == simdjson::SUCCESS)
            push_unique(d.skills, std::string(sv), kMaxCmds);
    } else if (name == "Task" || name == "Agent") {
        if (input["subagent_type"].get(sv) == simdjson::SUCCESS)
            push_unique(d.skills, "agent:" + std::string(sv), kMaxCmds);
    }
}

// Failed tool calls arrive as user-role tool_result blocks flagged is_error.
void count_failures(Digest& d, simdjson::dom::element content) {
    simdjson::dom::array arr;
    if (content.get(arr) != simdjson::SUCCESS) return;
    for (auto block : arr) {
        std::string_view type;
        if (block["type"].get(type) != simdjson::SUCCESS || type != "tool_result") continue;
        bool err = false;
        if (block["is_error"].get(err) == simdjson::SUCCESS && err) ++d.failures;
    }
}

bool blank(std::string_view s) {
    return std::all_of(s.begin(), s.end(),
                       [](unsigned char c) { return std::isspace(c); });
}

}  // namespace

std::vector<std::string> Digest::detail_lines() const {
    std::vector<std::string> out;
    std::vector<std::string> parts;

    const auto join = [](const std::vector<std::string>& v, std::string_view sep) {
        std::string s;
        for (const auto& item : v) {
            if (!s.empty()) s.append(sep);
            s.append(item);
        }
        return s;
    };

    if (!files.empty()) parts.push_back("files: " + join(files, ", "));
    if (!commands.empty()) parts.push_back("ran: " + join(commands, " | "));
    if (!skills.empty()) parts.push_back("skills: " + join(skills, ", "));
    if (failures > 0) parts.push_back(std::to_string(failures) + " failed");

    if (!parts.empty()) out.push_back("    " + join(parts, "  \xC2\xB7  "));  // ' · '
    if (!outcome.empty()) out.push_back("    did: " + outcome);
    return out;
}

Digest digest(const std::string& transcript_path,
              const std::function<bool(const std::string&)>& is_real_user_msg) {
    Digest d;
    std::ifstream in(transcript_path);
    if (!in) return d;

    simdjson::dom::parser parser;
    std::string line;
    while (std::getline(in, line)) {
        if (line.empty()) continue;
        simdjson::dom::element v;
        if (parser.parse(line).get(v) != simdjson::SUCCESS) continue;

        bool sidechain = false;
        if (v["isSidechain"].get(sidechain) == simdjson::SUCCESS && sidechain) continue;

        // Both fields are legitimately absent on some entry kinds; an empty
        // string_view is the correct outcome, so the error code is discarded.
        // Both fields are legitimately absent on some entry kinds; an empty
        // string_view is the correct outcome, so the miss is folded into the value.
        std::string_view entry_type;
        if (v["type"].get(entry_type) != simdjson::SUCCESS) entry_type = {};
        std::string_view role;
        if (v["message"]["role"].get(role) != simdjson::SUCCESS) role = {};

        simdjson::dom::element content;
        const bool has_content = v["message"]["content"].get(content) == simdjson::SUCCESS;

        if (entry_type == "user" || role == "user") {
            const std::string txt = has_content ? text_of(content) : std::string();
            if (!blank(txt) && is_real_user_msg(txt)) {
                d = Digest{};
                d.ask = txt;
            } else if (has_content) {
                count_failures(d, content);
            }
            continue;
        }

        if (entry_type == "assistant" || role == "assistant") {
            if (!has_content) continue;
            simdjson::dom::array arr;
            if (content.get(arr) == simdjson::SUCCESS) {
                for (auto block : arr) {
                    std::string_view type;
                    if (block["type"].get(type) == simdjson::SUCCESS && type == "tool_use")
                        collect_tool_use(d, block);
                }
            }
            const std::string txt = text_of(content);
            if (!blank(txt)) d.outcome = squeeze(txt, kOutcomeChars);
        }
    }
    return d;
}

}  // namespace cml
