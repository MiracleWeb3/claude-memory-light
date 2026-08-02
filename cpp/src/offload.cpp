#include "offload.hpp"

#include <algorithm>
#include <cctype>
#include <string_view>
#include <vector>

namespace cml {
namespace {

constexpr std::size_t kMinBytes = 2048;
constexpr std::size_t kHead = 5;
constexpr std::size_t kTail = 15;

// Substring, case-folded. A regex engine is not worth linking for nine needles, and
// these are the words a failing build actually prints.
constexpr std::string_view kErrorNeedles[] = {
    "error", "fail", "panic", "traceback", "undefined reference", "fatal",
    "segmentation fault", "assertion", "exception",
};
constexpr std::string_view kWarnNeedles[] = {"warning", "deprecated"};

std::string fold(std::string_view s) {
    std::string out;
    out.reserve(s.size());
    for (const char c : s) out.push_back(static_cast<char>(std::tolower(static_cast<unsigned char>(c))));
    return out;
}

bool has_any(const std::string& folded, const std::string_view* needles, std::size_t n) {
    for (std::size_t i = 0; i < n; ++i)
        if (folded.find(needles[i]) != std::string::npos) return true;
    return false;
}

std::vector<std::string_view> split_lines(std::string_view s) {
    std::vector<std::string_view> out;
    std::size_t start = 0;
    while (start <= s.size()) {
        const std::size_t nl = s.find('\n', start);
        if (nl == std::string_view::npos) {
            if (start < s.size()) out.push_back(s.substr(start));
            break;
        }
        out.push_back(s.substr(start, nl - start));
        start = nl + 1;
    }
    return out;
}

}  // namespace

bool condensable(std::string_view tool_name, std::size_t bytes) {
    if (bytes < kMinBytes) return false;
    // Bash only, for now. Measured over 146 transcripts: of the 11.60 MB carried by
    // results at or above 2 KB, Read is 53.7% and Bash 35.2%. Read is deliberately never
    // touched — at PostToolUse you have just read that file because you need it. That
    // leaves Bash as the whole reachable win, and Grep/Glob/MCP wait until their result
    // shapes are recorded the way Task 1 recorded Bash's: a shape mismatch is rejected
    // SILENTLY, so guessing produces a feature that does nothing and reports success.
    return tool_name == "Bash";
}

Condensed condense(std::string_view out, std::string_view spill_path) {
    Condensed c;
    const auto lines = split_lines(out);

    std::vector<bool> keep(lines.size(), false);
    for (std::size_t i = 0; i < lines.size(); ++i) {
        if (i < kHead || i + kTail >= lines.size()) keep[i] = true;
        const std::string f = fold(lines[i]);
        if (has_any(f, kErrorNeedles, std::size(kErrorNeedles))) {
            keep[i] = true;
            ++c.errors;
        } else if (has_any(f, kWarnNeedles, std::size(kWarnNeedles))) {
            ++c.warnings;
        }
    }

    std::string text;
    bool in_gap = false;
    for (std::size_t i = 0; i < lines.size(); ++i) {
        if (keep[i]) {
            if (in_gap) in_gap = false;
            text.append(lines[i]);
            text.push_back('\n');
        } else {
            ++c.elided;
            in_gap = true;
        }
    }

    // ⟨ and ⟩ as raw bytes. The split after \xA8 is load-bearing: a hex escape eats every
    // hex digit that follows it, so "\xE2\x9F\xA8cml" is \xA8c — out of range, and -Werror.
    std::string tail = "\xE2\x9F\xA8" "cml: " + std::to_string(c.elided) + " lines elided \xC2\xB7 " +
                       std::to_string(c.errors) + " errors \xC2\xB7 " +
                       std::to_string(c.warnings) + " warnings";
    if (!spill_path.empty()) tail += " \xC2\xB7 full: " + std::string(spill_path);
    tail += "\xE2\x9F\xA9\n";
    c.text = tail + text;
    return c;
}

}  // namespace cml
