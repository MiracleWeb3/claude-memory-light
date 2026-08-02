// Not shipped: a main() outside cml_core, run by hand over a corpus of real tool results to
// decide whether the condenser is safe to wire up.
//
// It does NOT count needle matches before and after. That gate is a tautology — every needle
// match is kept unconditionally, so it reads zero for every possible input, including a
// sabotaged condenser keeping ONLY needle lines and discarding every traceback body, caret
// and candidate list (measured: 92.3% saved / 0 lost, beating the real one's 44.4% / 0).
//
// What it measures instead:
//
//   Leave-one-needle-out. For needle k, the lines matching k AND NO OTHER needle are the ones
//   whose survival depends entirely on k. Condense with k disabled and count how many survive
//   anyway, on head/tail and the two drag passes. That is "a failure line the needle list
//   cannot see", simulated 31 ways over real data — and unlike `errors lost` it moves when
//   the rules move: 56.5% with the drags off, 71.5% with them on.
//
//   Bytes. What the corpus costs before and after, which is the whole point of the feature.
//
//   --per-file. One row per result, so two runs (two binaries, or two kDragUp depths) can be
//   joined and sorted by byte delta. A global rate cannot move on a 9-files-in-1,046
//   regression; this can.
#include <algorithm>
#include <cctype>
#include <cstdint>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <string>
#include <string_view>
#include <vector>

#include "offload.hpp"

namespace {

// Production always names a spill path and the header carries it, so measuring without one
// would under-count the bytes the real hook emits.
constexpr std::string_view kSpill = "/spill/x.txt";

std::string fold(std::string_view s) {
    std::string out;
    out.reserve(s.size());
    for (const char c : s) out.push_back(static_cast<char>(std::tolower(static_cast<unsigned char>(c))));
    return out;
}

// The index of the ONLY needle this line matches, or SIZE_MAX when none or more than one
// does. Lines matching two needles are excluded deliberately: they survive without either,
// so they say nothing about whether the rules can find a line the list has missed.
std::size_t sole_needle(std::string_view line) {
    const auto needles = cml::flag_needles();
    const std::string f = fold(line);
    std::size_t hit = SIZE_MAX;
    for (std::size_t i = 0; i < needles.size(); ++i) {
        if (f.find(needles[i]) == std::string::npos) continue;
        if (hit != SIZE_MAX) return SIZE_MAX;  // two matches, not attributable to one needle
        hit = i;
    }
    return hit;
}

void tally(std::string_view text, std::vector<std::size_t>& out) {
    std::size_t start = 0;
    while (start < text.size()) {
        std::size_t nl = text.find('\n', start);
        if (nl == std::string_view::npos) nl = text.size();
        const std::size_t k = sole_needle(text.substr(start, nl - start));
        if (k != SIZE_MAX) ++out[k];
        start = nl + 1;
    }
}

double pct(std::size_t part, std::size_t whole) {
    return whole ? 100.0 * static_cast<double>(part) / static_cast<double>(whole) : 0.0;
}

}  // namespace

int main(int argc, char** argv) {
    if (argc < 2) {
        std::fprintf(stderr, "usage: calibrate-offload <dir> [--per-file]\n");
        return 2;
    }
    const bool per_file = argc > 2 && std::string_view(argv[2]) == "--per-file";

    std::vector<std::filesystem::path> files;
    for (const auto& e : std::filesystem::directory_iterator(argv[1]))
        if (e.path().extension() == ".txt") files.push_back(e.path());
    std::sort(files.begin(), files.end());  // --per-file rows must line up across runs

    const auto needles = cml::flag_needles();
    const std::size_t nn = needles.size();
    std::vector<std::size_t> total(nn, 0), kept(nn, 0), kept_nodrag(nn, 0);
    std::size_t n = 0;
    unsigned long long before = 0, after = 0;

    for (const auto& p : files) {
        std::ifstream in(p, std::ios::binary);
        const std::string body((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
        if (body.size() < 2048) continue;  // the production floor
        ++n;
        const auto c = cml::condense(body, kSpill);
        before += body.size();
        after += c.text.size();
        if (per_file) {
            std::printf("%s\t%zu\t%zu\n", p.filename().c_str(), body.size(), c.text.size());
            continue;
        }

        std::vector<std::size_t> here(nn, 0);
        tally(body, here);
        for (std::size_t k = 0; k < nn; ++k) {
            if (!here[k]) continue;  // nothing to lose in this file, skip the two condenses
            total[k] += here[k];
            std::vector<std::size_t> got(nn, 0), got0(nn, 0);
            tally(cml::condense(body, kSpill, {.skip_needle = k}).text, got);
            tally(cml::condense(body, kSpill, {.skip_needle = k, .drags = false}).text, got0);
            kept[k] += got[k];
            kept_nodrag[k] += got0[k];
        }
    }
    if (per_file) return 0;

    std::size_t t = 0, s = 0, s0 = 0;
    for (std::size_t k = 0; k < nn; ++k) {
        t += total[k];
        s += kept[k];
        s0 += kept_nodrag[k];
    }
    std::printf("results   : %zu\n", n);
    std::printf("bytes     : %llu -> %llu (%.1f%% saved)\n", before, after,
                pct(static_cast<std::size_t>(before - after), static_cast<std::size_t>(before)));
    std::printf("survival  : %.1f%% shipped, %.1f%% with drags off, over %zu sole-needle lines\n",
                pct(s, t), pct(s0, t), t);

    // Sorted worst-first: a needle far below the rest marks a missing structural rule, not a
    // missing needle — that is the reading this table exists for.
    std::vector<std::size_t> order(nn);
    for (std::size_t k = 0; k < nn; ++k) order[k] = k;
    std::sort(order.begin(), order.end(), [&](std::size_t a, std::size_t b) {
        return pct(kept[a], total[a]) < pct(kept[b], total[b]);
    });
    for (const std::size_t k : order) {
        if (!total[k]) continue;
        std::printf("  %-20.*s %5.1f%%  (drags off %5.1f%%)  n=%zu\n",
                    static_cast<int>(needles[k].size()), needles[k].data(),
                    pct(kept[k], total[k]), pct(kept_nodrag[k], total[k]), total[k]);
    }
    return 0;
}
