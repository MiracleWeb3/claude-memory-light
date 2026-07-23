#include "noise.hpp"

#include <algorithm>
#include <array>
#include <cctype>

namespace cml {
namespace {

// Note "<command-message>": the old list had only "<command-name>", which is the
// SECOND tag in the envelope, so every slash-command row was indexed as user prose.
constexpr std::array<std::string_view, 18> kEnvelopePrefixes{
    "Caveat:",
    "<command-message>",
    "<command-name>",
    "<local-command",
    "<system-reminder>",
    "<task-notification>",
    "<channel source=",
    "<user-prompt-submit-hook>",
    "<teammate-message",
    "Another Claude session sent a message:",
    "[Image: source:",
    "Base directory for this skill",
    "Launching skill:",
    "Stop hook feedback:",
    "[Request interrupted",
    "This session is being continued",
    "Continue from where you left off",
    "(Re-invocation of",
};

constexpr std::array<std::string_view, 34> kAcks{
    "ok",   "okay", "k",       "yes",   "yep",     "yeah",  "no",     "nope", "sure",
    "go",   "go ahead",        "do it", "continue", "continue please", "proceed",
    "next", "thanks", "thank you", "ty",  "nice",   "cool",  "great",  "perfect",
    "y",    "n",    "/compact", "stop", "wait",    "hi",    "hey",    "hello",
    "yes please", "no please", "implement please",
};

constexpr std::array<std::string_view, 29> kCorrectionPhrases{
    "no,",         "nope",       "dont ",       "do not ",   "wrong",
    "not what",    "thats not",  "instead of",  "why did you", "why do you",
    "why are you", "you broke",  "you missed",  "you forgot", "shouldnt",
    "should not",  "isnt ",      "doesnt ",     "never do",   "revert",
    "undo ",       "actually no", "not correct", "not right", "didnt work",
    "doesnt work", "still no",   "nothing changed", "stop doing",
};

std::string_view trim(std::string_view s) {
    const auto not_space = [](unsigned char c) { return !std::isspace(c); };
    const auto b = std::find_if(s.begin(), s.end(), not_space);
    const auto e = std::find_if(s.rbegin(), s.rend(), not_space).base();
    return (b >= e) ? std::string_view{} : s.substr(b - s.begin(), e - b);
}

std::string lower_no_apostrophes(std::string_view s) {
    std::string out;
    out.reserve(s.size());
    for (std::size_t i = 0; i < s.size(); ++i) {
        // U+2019 RIGHT SINGLE QUOTATION MARK arrives as the 3 bytes E2 80 99.
        if (i + 2 < s.size() && static_cast<unsigned char>(s[i]) == 0xE2 &&
            static_cast<unsigned char>(s[i + 1]) == 0x80 &&
            static_cast<unsigned char>(s[i + 2]) == 0x99) {
            i += 2;
            continue;
        }
        if (s[i] == '\'') continue;
        out.push_back(static_cast<char>(std::tolower(static_cast<unsigned char>(s[i]))));
    }
    return out;
}

bool starts_with(std::string_view s, std::string_view p) {
    return s.size() >= p.size() && s.compare(0, p.size(), p) == 0;
}

}  // namespace

bool is_noise(std::string_view text) {
    const std::string_view t = trim(text);
    if (t.empty()) return true;

    for (const auto& p : kEnvelopePrefixes) {
        if (starts_with(t, p)) return true;
    }
    if (t.find("hook additional context:") != std::string_view::npos) return true;
    if (t.find("hook success:") != std::string_view::npos) return true;

    // Strip surrounding punctuation but keep '/' so "/compact" still matches.
    const auto keep = [](unsigned char c) { return std::isalnum(c) || c == '/'; };
    std::size_t b = 0, e = t.size();
    while (b < e && !keep(static_cast<unsigned char>(t[b]))) ++b;
    while (e > b && !keep(static_cast<unsigned char>(t[e - 1]))) --e;
    const std::string bare = lower_no_apostrophes(t.substr(b, e - b));

    return std::find(kAcks.begin(), kAcks.end(), bare) != kAcks.end();
}

bool looks_like_correction(std::string_view text) {
    const std::string lowered = lower_no_apostrophes(text);
    return std::any_of(kCorrectionPhrases.begin(), kCorrectionPhrases.end(),
                       [&](std::string_view p) {
                           return lowered.find(p) != std::string::npos;
                       });
}

std::string squeeze(std::string_view s, std::size_t max) {
    std::string out;
    out.reserve(std::min(s.size(), max + 4));
    bool in_gap = true;
    for (const char c : s) {
        if (std::isspace(static_cast<unsigned char>(c))) {
            in_gap = true;
            continue;
        }
        if (in_gap && !out.empty()) out.push_back(' ');
        in_gap = false;
        out.push_back(c);
    }
    if (out.size() <= max) return out;
    // Back off to a UTF-8 boundary so a clipped multibyte char never leaks.
    std::size_t cut = max;
    while (cut > 0 && (static_cast<unsigned char>(out[cut]) & 0xC0) == 0x80) --cut;
    out.resize(cut);
    out += "\xE2\x80\xA6";  // U+2026 HORIZONTAL ELLIPSIS
    return out;
}

}  // namespace cml
