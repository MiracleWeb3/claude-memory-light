// The whole test harness: a counter and an abort-on-fail check. Suites live one
// concern per file; the entry point (../selftest.cpp) runs them in order.
#pragma once

#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <string>

namespace cml_test {

inline int checks = 0;

inline void ok(bool cond, const char* what) {
    ++checks;
    if (!cond) {
        std::fprintf(stderr, "FAIL: %s\n", what);
        std::abort();
    }
}

// Drive a hook subcommand the way the runtime drives it: the real binary, real stdin,
// real stdout, under a $CML_HOME that is never the user's own index. In-process calls
// cannot see what the hook contract is actually about — a replacement rejected on its
// output schema, or three lines that must stay three — because that lives in the emitted
// bytes. Empty return means the selftest was built without `cml` beside it; callers skip.
inline std::string drive(const char* subcommand, const std::filesystem::path& home,
                         const std::string& payload) {
    namespace fs = std::filesystem;
    const fs::path exe = fs::read_symlink("/proc/self/exe").parent_path() / "cml";
    if (!fs::exists(exe)) return {};
    const fs::path in = home / "stdin.json";
    { std::ofstream(in) << payload; }
    const std::string cmd = "CML_HOME=" + home.string() + " " + exe.string() + " " +
                            subcommand + " < " + in.string();
    FILE* p = popen(cmd.c_str(), "r");
    if (!p) return {};
    std::string out;
    char buf[4096];
    while (std::size_t n = std::fread(buf, 1, sizeof buf, p)) out.append(buf, n);
    pclose(p);
    return out;
}

void suite_filters();    // noise, correction flag, paths, digest, squeeze
void suite_embedding();  // tokenizer + vectors (skips without the model cache)
void suite_curate();     // forget --match, judge request/verdicts
void suite_map();        // wikilinks + map payload
void suite_ports();      // hook subcommands: chronic loops, prompt hints, recall gates
void suite_encoder();    // transformer kernels; semantic checks skip without the model
void suite_offload();       // tool-output condenser: allowlist, floor, what survives
void suite_offload_hook();  // the PostToolUse entry, driven through the real binary
void suite_scene();         // the L2 scene table — opens a db, under a temp $CML_HOME

}  // namespace cml_test
