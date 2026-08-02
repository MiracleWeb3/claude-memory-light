// The condenser: which results it touches, and what survives when it does.
#include <string>
#include "harness.hpp"
#include "offload.hpp"

namespace cml_test {
namespace {

std::string big_log(int lines, const char* body) {
    std::string s;
    for (int i = 0; i < lines; ++i) s += std::string(body) + " " + std::to_string(i) + "\n";
    return s;
}

void test_allowlist_and_floor() {
    ok(cml::condensable("Bash", 4000), "a big Bash result is condensable");
    ok(!cml::condensable("Bash", 100), "a small one is not");
    ok(!cml::condensable("Read", 400000), "Read is never touched, at any size");
    ok(!cml::condensable("Edit", 400000), "nor Edit");
    ok(!cml::condensable("Write", 400000), "nor Write");
    // v1 is Bash and nothing else. updatedToolOutput is validated against the tool's own
    // output schema and REJECTED SILENTLY on a mismatch (Task 1), so a tool whose result
    // shape has not been recorded cannot be condensed safely — it would look like a no-op.
    ok(!cml::condensable("Grep", 400000), "Grep's result shape is not recorded yet");
    ok(!cml::condensable("mcp__foo__bar", 400000), "nor any MCP tool's");
    ok(!cml::condensable("SomeFutureTool", 400000),
       "an unknown tool is left alone — allowlist, not denylist");
}

void test_errors_survive_condensation() {
    std::string log = big_log(200, "compiling module");
    log += "error: undefined reference to `foo'\n";
    log += big_log(200, "compiling module");
    const auto c = cml::condense(log, "/tmp/spill/1.txt");
    ok(c.text.find("undefined reference to `foo'") != std::string::npos,
       "the error line is kept verbatim — this is the whole point");
    ok(c.errors == 1, "and counted");
    ok(c.text.size() < log.size() / 2, "and the result actually shrank");
    ok(c.elided > 300, "most lines elided");
}

void test_head_and_tail_kept() {
    const std::string log = big_log(200, "line");
    const auto c = cml::condense(log, "/tmp/spill/1.txt");
    ok(c.text.find("line 0") != std::string::npos, "first line kept");
    ok(c.text.find("line 4") != std::string::npos, "5 head lines kept");
    ok(c.text.find("line 199") != std::string::npos, "last line kept");
    ok(c.text.find("line 185") != std::string::npos, "15 tail lines kept");
    ok(c.text.find("line 100") == std::string::npos, "the middle is gone");
}

void test_spill_path_is_named_in_the_output() {
    const auto c = cml::condense(big_log(200, "x"), "/tmp/spill/abc/7.txt");
    ok(c.text.find("/tmp/spill/abc/7.txt") != std::string::npos,
       "the recovery path is stated, or the original is unreachable");
}

void test_warnings_counted_but_not_all_kept() {
    std::string log;
    for (int i = 0; i < 40; ++i) log += "warning: unused variable x" + std::to_string(i) + "\n";
    log += big_log(200, "ok");
    const auto c = cml::condense(log, "/tmp/s.txt");
    ok(c.warnings == 40, "every warning counted");
}

}  // namespace

void suite_offload() {
    test_allowlist_and_floor();
    test_errors_survive_condensation();
    test_head_and_tail_kept();
    test_spill_path_is_named_in_the_output();
    test_warnings_counted_but_not_all_kept();
}

}  // namespace cml_test
