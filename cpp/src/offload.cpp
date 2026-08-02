#include "offload.hpp"

#include <cctype>
#include <string_view>
#include <vector>

namespace cml {
namespace {

constexpr std::size_t kMinBytes = 2048;
constexpr std::size_t kHead = 5;
constexpr std::size_t kTail = 15;

// Substring, case-folded. Measured against real output, not guessed: the first version of
// this list was "a line containing the word error", and real tools mostly do not print that
// word. It dropped all eleven of `command not found`, `Permission denied`, `No such file or
// directory`, `cannot find -lfoo`, `Unable to locate package`, `Couldn't connect`, `Killed`,
// and every frame of a Python traceback.
//
// Warnings are flagged, not merely counted: this tree builds with -Werror, which makes a
// warning an error. The old split also produced an accident — `-Werror=unused-variable`
// matched the *error* needle while `-Wunused-variable` did not, so the same warning survived
// or vanished depending on a compiler flag's spelling.
constexpr std::string_view kFlagNeedles[] = {
    "error", "fail", "panic", "traceback", "undefined reference", "fatal",
    "segmentation fault", "assertion", "exception", "warning", "deprecated",
    "command not found", "no such file", "permission denied", "cannot access",
    "cannot stat", "cannot find", "cannot open", "unable to", "not found",
    "couldn't", "could not", "refused", "timed out", "killed", "aborted",
    "denied", "invalid", "unexpected", "missing", "expected",
};

std::string fold(std::string_view s) {
    std::string out;
    out.reserve(s.size());
    for (const char c : s) out.push_back(static_cast<char>(std::tolower(static_cast<unsigned char>(c))));
    return out;
}

// "1 line", not "1 lines". The header and the gap markers are read by a model that reasons
// over them; sloppy grammar there is one more thing it has to decide whether to trust.
std::string plural(std::size_t n, std::string_view noun) {
    return std::to_string(n) + " " + std::string(noun) + (n == 1 ? "" : "s");
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

// How far a flagged line may drag its context, in either direction. Unbounded is not an
// option: the first version seeded the downward drag from ANY kept line, so an output whose
// body is indented under line 4 — pretty-printed JSON, `kubectl get -o yaml`, `curl | jq` —
// never terminated the run and kept the entire file. Measured: a 483-line pretty JSON went
// from 96% saved to 0% saved, silently, with no test able to see it.
constexpr std::size_t kDragUp = 6;

// An indented line is a continuation of the unindented line above it — that is how tracebacks,
// GCC carets, gtest expectations and pytest assertions all print. Keeping a flagged line
// without its indented run keeps the fact that something failed and throws away where.
bool indented(std::string_view s) {
    return !s.empty() && (s.front() == ' ' || s.front() == '\t');
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
    const std::size_t n = lines.size();

    std::vector<bool> keep(n, false), seed(n, false);
    for (std::size_t i = 0; i < n; ++i) {
        if (i < kHead || i + kTail >= n) keep[i] = true;
        if (has_any(fold(lines[i]), kFlagNeedles, std::size(kFlagNeedles))) {
            keep[i] = true;
            seed[i] = true;  // ONLY a needle match seeds a drag — never a head/tail edge
            ++c.flagged;
        }
    }
    // Downward pass: a flagged line drags the contiguous indented run beneath it. Measured
    // on a real pytest traceback this recovers 8 frames of 8 — no `File "..."` line matches
    // any needle, so this rule is doing all the work there, not refining it.
    //
    // Seeded from `seed`, not from `keep`. Seeding from any kept line is the N1 regression:
    // line 4 is kept as a head edge, an indented body hangs off it, and the run swallows the
    // file. Propagating `seed[i+1]` is what carries the drag down a multi-line run.
    //
    // No `!keep[i+1]` guard. It reads as a harmless redundancy check but doubles as the
    // propagation gate: a line already kept for an unrelated reason never gets seeded, so the
    // run stops dead there. The head window is where that always bites — a needle at index
    // 0-3 could not reach past index 4, and a combined stdout+stderr capture puts the failure
    // on line 0 routinely. Measured: a 40-line candidate list under an `error:` on line 0
    // survived 4 of 40.
    for (std::size_t i = 0; i + 1 < n; ++i) {
        if (seed[i] && indented(lines[i + 1])) {
            keep[i + 1] = true;
            seed[i + 1] = true;
        }
    }
    // Upward pass, bounded: GCC prints its instantiation chain ABOVE the error line and at
    // column 0, so neither the needles nor the downward rule can reach it —
    // `tpl2.cpp:6:14:   required from here` is the line naming the file the user owns, and
    // without it the surviving `error:` lines all point into /usr/include/c++/13.
    for (std::size_t i = 0; i < n; ++i) {
        if (!seed[i]) continue;
        for (std::size_t k = 1; k <= kDragUp && k <= i; ++k) {
            if (lines[i - k].empty()) break;  // a blank line ends the block
            keep[i - k] = true;
        }
    }

    std::string text;
    std::size_t gap = 0;
    const auto flush_gap = [&] {
        if (!gap) return;
        text += "\xE2\x9F\xA8\xE2\x80\xA6 " + plural(gap, "line") + " \xE2\x80\xA6\xE2\x9F\xA9\n";
        gap = 0;
    };
    for (std::size_t i = 0; i < n; ++i) {
        if (keep[i]) {
            flush_gap();  // a marker where the jump happened, or the text claims false adjacency
            text.append(lines[i]);
            text.push_back('\n');
        } else {
            ++c.elided;
            ++gap;
        }
    }
    flush_gap();

    // ⟨ and ⟩ as raw bytes. The split after \xA8 is load-bearing: a hex escape eats every
    // hex digit that follows it, so "\xE2\x9F\xA8cml" is \xA8c — out of range, and -Werror.
    std::string header = "\xE2\x9F\xA8" "cml: " + plural(c.elided, "line") +
                         " elided \xC2\xB7 " + std::to_string(c.flagged) + " flagged";
    if (!spill_path.empty()) header += " \xC2\xB7 full: " + std::string(spill_path);
    header += "\xE2\x9F\xA9\n";
    c.text = header + text;

    // Never hand back more than we were given. A single-line 9 KB JSON, or any output of 20
    // lines or fewer, keeps every line by construction (kHead + kTail = 20) and would other-
    // wise be re-emitted under a header claiming nothing was elided — costing tokens, not
    // saving them. Measured: a 9,692-byte curl result came back 9,770 bytes.
    if (c.text.size() >= out.size()) {
        c.text.assign(out);
        c.elided = 0;
        c.flagged = 0;  // the pass that produced it was thrown away; do not report its count
        c.grew = true;
    }
    return c;
}

}  // namespace cml
