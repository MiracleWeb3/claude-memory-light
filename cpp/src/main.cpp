// cml (C++) — zero-token, daemon-free memory for Claude Code.
//
// Complete port of the Rust original: index, search (FTS5 + model2vec/sqlite-vec
// hybrid), capture, nudge, stats, doctor, embed, forget, distill, map.
//
// Dependencies are two C libraries: sqlite3 and simdjson — plus sqlite-vec
// vendored as a single C file. No binding layer anywhere; the curl used by
// distill is exec'd as a subprocess, never linked.

#include <algorithm>
#include <cstdio>
#include <string>
#include <string_view>
#include <vector>

#include "curate.hpp"
#include "hint.hpp"
#include "indexer.hpp"
#include "loops.hpp"
#include "map.hpp"
#include "search.hpp"
#include "vec.hpp"

namespace cml {
void capture();
}

namespace {

int usage() {
    std::printf(
        "cml: index [--all] | search <terms> [--project P] [--role R] [--limit N]\n"
        "     [--semantic|--keyword] | forget <rowid...> | forget --match \"<q>\" [--yes]\n"
        "     | distill [--all] | embed [--all] | map [--limit N] [--no-open]\n"
        "     | loops [--days N] [--limit K] | stats | doctor | capture | nudge | hint\n");
    return 0;
}

}  // namespace

int main(int argc, char** argv) {
    const std::string_view cmd = (argc > 1) ? argv[1] : "";
    std::vector<std::string> rest;
    for (int i = 2; i < argc; ++i) rest.emplace_back(argv[i]);

    // Hook-facing subcommands exit 0 on every path, including failure: a memory hook
    // that returns non-zero can interrupt the user's session.
    if (cmd == "capture") {
        cml::capture();
        return 0;
    }
    if (cmd == "nudge") {
        cml::nudge();
        return 0;
    }
    if (cmd == "hint") {
        cml::hint();
        return 0;
    }

    if (cmd == "index") {
        const bool force = std::any_of(rest.begin(), rest.end(),
                                       [](const std::string& a) { return a == "--all"; });
        return cml::index_all(force);
    }
    if (cmd == "search") return cml::search(rest);
    if (cmd == "stats") return cml::stats();
    if (cmd == "doctor") return cml::doctor();
    if (cmd == "embed") return cml::embed_cmd(rest);
    if (cmd == "forget") return cml::forget(rest);
    if (cmd == "distill") return cml::distill(rest);
    if (cmd == "map") return cml::map(rest);
    if (cmd == "loops") return cml::loops(rest);

    if (!cmd.empty() && cmd != "help" && cmd != "--help" && cmd != "-h") {
        std::fprintf(stderr, "cml: unknown or unported command '%.*s'\n",
                     static_cast<int>(cmd.size()), cmd.data());
        usage();
        return 2;
    }
    return usage();
}
