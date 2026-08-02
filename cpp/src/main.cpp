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
#include "offload.hpp"
#include "recall.hpp"
#include "search.hpp"
#include "state.hpp"
#include "vec.hpp"

namespace cml {
void capture();
}

namespace {

int usage() {
    std::printf(
        "cml: index [--all] | search <terms> [--project P] [--role R] [--limit N]\n"
        "     [--semantic|--keyword] | forget <rowid...> | forget --match \"<q>\" [--yes]\n"
        "     | distill [--all] [--limit N] | distill --scenes [--all] [--limit SESSIONS]\n"
        "     | embed [--all] | map [--limit N] [--no-open] [--raw]\n"
        "     | loops [--days N] [--limit K] | stats | doctor | capture | nudge | hint | offload\n"
        "     | recall | eval [-k N] [--limit N] | state [--project P] | version\n");
    return 0;
}

}  // namespace

#ifndef CML_VERSION
#define CML_VERSION "unknown"  // built outside CMake; the manifest was not readable
#endif

int main(int argc, char** argv) {
    const std::string_view cmd = (argc > 1) ? argv[1] : "";
    std::vector<std::string> rest;
    for (int i = 2; i < argc; ++i) rest.emplace_back(argv[i]);

    // "Which cml is actually running?" had no answer before this, so a stale install
    // looked exactly like a bug in the source.
    if (cmd == "version" || cmd == "--version") {
        std::printf("cml %s\n", CML_VERSION);
        return 0;
    }

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
    if (cmd == "recall") {
        cml::recall();
        return 0;
    }
    if (cmd == "offload") {
        cml::offload();
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
    if (cmd == "eval") return cml::eval(rest);
    if (cmd == "state") return cml::state_cmd(rest);

    if (!cmd.empty() && cmd != "help" && cmd != "--help" && cmd != "-h") {
        std::fprintf(stderr, "cml: unknown or unported command '%.*s'\n",
                     static_cast<int>(cmd.size()), cmd.data());
        usage();
        return 2;
    }
    return usage();
}
