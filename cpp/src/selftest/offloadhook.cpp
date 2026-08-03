// The hook entry, driven as the hook drives it: the real binary, real stdin, real stdout.
//
// This suite exists because the failure it guards is invisible. `updatedToolOutput` is
// validated against the tool's own output schema and rejected SILENTLY on a mismatch — a
// replacement missing one of Bash's five keys leaves the original in the transcript and
// reports nothing, anywhere. Calling condense() in-process cannot see that; only the
// emitted bytes can, so the shape is asserted here rather than trusted.
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <string>

#include "harness.hpp"
#include "offload.hpp"

namespace cml_test {
namespace {

namespace fs = std::filesystem;

fs::path sandbox() { return fs::temp_directory_path() / "cml-selftest-offload"; }

// $CML_HOME redirects the spill, so a test run never writes into the real memory dir.
std::string run(const std::string& payload) { return drive("offload", sandbox(), payload); }

std::string payload(const std::string& body, bool with_session = true) {
    std::string esc;
    for (const char c : body) esc += (c == '\n') ? "\\n" : std::string(1, c);
    std::string s = "{\"tool_name\":\"Bash\",\"tool_use_id\":\"toolu_test1\",";
    if (with_session) s += "\"session_id\":\"selftest-sess\",";
    return s + "\"tool_response\":{\"stdout\":\"" + esc +
           "\",\"stderr\":\"boom\",\"interrupted\":false,\"isImage\":false,"
           "\"noOutputExpected\":false}}";
}

std::string long_output() {
    std::string s;
    for (int i = 0; i < 400; ++i) s += "line " + std::to_string(i) + " of output\n";
    return s;
}

void test_replacement_mirrors_bashs_output_shape() {
    fs::remove_all(sandbox());
    fs::create_directories(sandbox());
    const std::string body = long_output();
    const std::string out = run(payload(body));
    if (out.empty()) return;  // no cml binary beside the selftest

    ok(out.find("\"updatedToolOutput\"") != std::string::npos, "a replacement is emitted");
    ok(out.find("\"hookEventName\": \"PostToolUse\"") != std::string::npos,
       "under the PostToolUse event name the runtime dispatches on");
    // All five, or the whole thing is dropped without a word.
    for (const char* k : {"\"stdout\"", "\"stderr\"", "\"interrupted\"", "\"isImage\"",
                          "\"noOutputExpected\""})
        ok(out.find(k) != std::string::npos, k);
    ok(out.find("\\u27e8cml:") != std::string::npos || out.find("\xE2\x9F\xA8" "cml:") != std::string::npos,
       "and the condensed marker is in it");
    ok(out.find("\"stderr\": \"boom\"") != std::string::npos,
       "siblings are echoed back unchanged — only stdout moves");

    const fs::path spill = sandbox() / "spill/selftest-sess/toolu_test1.txt";
    ok(fs::exists(spill), "the original landed on disk before the swap");
    std::ifstream in(spill);
    const std::string back((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
    ok(back == body, "byte-identical to what the command printed");
    fs::remove_all(sandbox());
}

void test_no_spill_means_no_replacement() {
    // The transcript keeps only what we emit, so condensing without a saved original
    // destroys the output permanently. No session means no spill path, which must mean
    // passthrough — not a condensation with nowhere to fall back to.
    fs::remove_all(sandbox());
    fs::create_directories(sandbox());
    const std::string out = run(payload(long_output(), /*with_session=*/false));
    if (out.empty()) return;
    ok(out.find("updatedToolOutput") == std::string::npos,
       "no spill, no replacement — the original output must survive in the transcript");
    ok(out.find("\"continue\"") != std::string::npos, "and the turn still goes through");
    fs::remove_all(sandbox());
}

void test_small_output_passes_through() {
    fs::remove_all(sandbox());
    fs::create_directories(sandbox());
    const std::string out = run(payload("just a line\n"));
    if (out.empty()) return;
    ok(out.find("updatedToolOutput") == std::string::npos,
       "under the 2 KB floor nothing is touched");
    fs::remove_all(sandbox());
}

}  // namespace

void suite_offload_hook() {
    test_replacement_mirrors_bashs_output_shape();
    test_no_spill_means_no_replacement();
    test_small_output_passes_through();
}

}  // namespace cml_test
