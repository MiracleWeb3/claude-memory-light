#include "offload.hpp"

#include <cctype>
#include <filesystem>
#include <fstream>
#include <string_view>
#include <vector>

#include "paths.hpp"

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

bool has_any(const std::string& folded, std::size_t skip) {
    for (std::size_t i = 0; i < std::size(kFlagNeedles); ++i)
        if (i != skip && folded.find(kFlagNeedles[i]) != std::string::npos) return true;
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
//
// 1, picked by the corpus — 1,046 real Bash results, tools/calibrate-offload.cpp:
//
//   depth   0     1     2     3     4     6     8    12
//   bytes  49.4  48.2  47.2  46.4  45.6  44.4  43.4  41.9   % saved
//   surv   48.3  53.3  55.3  57.1  58.0  59.4  60.4  62.6   % leave-one-needle-out
//
// Both curves are monotone and opposed, so the depth is a rate, not a threshold — and 0->1 is
// the knee: 1.2 points of bytes buys 5.0 points of survival, where every later step buys 1-2.
// The first line above a needle is the diagnosis (`tpl2.cpp:6:14: required from here` sits
// directly above the error it explains); the sixth is usually another line of the same build
// log. Task 2's GCC fixture measured the same shape from the other side — depth 1 keeps
// tpl2.cpp:6 in 3,020 bytes, depth 6 keeps the identical content in 9,930.
constexpr std::size_t kDragUp = 1;

// An indented line is a continuation of the unindented line above it — that is how tracebacks,
// GCC carets, gtest expectations and pytest assertions all print. Keeping a flagged line
// without its indented run keeps the fact that something failed and throws away where.
bool indented(std::string_view s) {
    return !s.empty() && (s.front() == ' ' || s.front() == '\t');
}

// Both spill components are pasted into a filesystem path and both arrive as JSON on
// stdin, so anything that is not a plain name is refused rather than resolved.
bool plain_name(std::string_view s) {
    return !s.empty() && s.find('/') == std::string_view::npos &&
           s.find("..") == std::string_view::npos;
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

std::span<const std::string_view> flag_needles() {
    return {kFlagNeedles, std::size(kFlagNeedles)};
}

Condensed condense(std::string_view out, std::string_view spill_path, CondenseOpts o) {
    Condensed c;
    const auto lines = split_lines(out);
    const std::size_t n = lines.size();

    std::vector<bool> keep(n, false), seed(n, false);
    for (std::size_t i = 0; i < n; ++i) {
        if (i < kHead || i + kTail >= n) keep[i] = true;
        if (has_any(fold(lines[i]), o.skip_needle)) {
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
    //
    // `o.drags` gates this pass and the upward one below. It is false only in the calibration
    // harness, to measure what head/tail alone would have kept.
    for (std::size_t i = 0; o.drags && i + 1 < n; ++i) {
        if (seed[i] && indented(lines[i + 1])) {
            keep[i + 1] = true;
            seed[i + 1] = true;
        }
    }
    // Upward pass, bounded: GCC prints its instantiation chain ABOVE the error line and at
    // column 0, so neither the needles nor the downward rule can reach it —
    // `tpl2.cpp:6:14:   required from here` is the line naming the file the user owns, and
    // without it the surviving `error:` lines all point into /usr/include/c++/13.
    for (std::size_t i = 0; o.drags && i < n; ++i) {
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

// The original, kept whole. `work` truncates at kToolMax = 1200 (transcript.cpp:61) and that
// cap is correct for a search index — "a megabyte of source is not a memory". It is wrong for
// recovery, so recovery gets its own store rather than a looser cap.
std::string spill_write(std::string_view session, std::string_view tool_use_id,
                        std::string_view body) {
    // Both components follow the same rule. `tool_use_id` used to fall back to a constant
    // "result", which is traversal-safe and opens a collision instead: the stream is
    // `ios::trunc`, so two unnamed spills in one session write the same file and the second
    // erases the first — whose transcript entry still names that path. The model then reads
    // another command's output believing it is this one's recovery, which is worse than
    // losing it. Refusing costs nothing: no spill already means no condensation.
    if (!plain_name(session) || !plain_name(tool_use_id)) return {};
    std::error_code ec;
    const auto dir = data_dir() / "spill" / std::string(session);
    std::filesystem::create_directories(dir, ec);
    if (ec) return {};
    const auto path = dir / (std::string(tool_use_id) + ".txt");
    std::ofstream out(path, std::ios::binary | std::ios::trunc);
    if (!out) return {};
    out.write(body.data(), static_cast<std::streamsize>(body.size()));
    // close() before the check — not the destructor after the return. ofstream buffers, so a
    // body under libstdc++'s ~8 KB filebuf never reached the disk while `write()` returned:
    // the stream was still good, this handed back a path, and the real I/O happened during
    // destruction with nobody looking at it. Measured on a tmpfs with 8 KB free, a 3,000-byte
    // body passed the old check and landed 0 bytes. That band — 2 KB (kMinBytes) to ~8 KB —
    // is most of the condensed population by count, and the transcript keeps only the
    // condensed form, so the path in it named a file that did not exist and the elided lines
    // were gone from both copies.
    out.close();
    if (!out) {
        std::filesystem::remove(path, ec);  // no empty file left behind looking authoritative
        return {};
    }
    return path.string();
}

}  // namespace cml
