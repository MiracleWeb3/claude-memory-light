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
    ok(c.flagged == 1, "and flagged");
    ok(c.text.size() < log.size() / 2, "and the result actually shrank");
    ok(c.elided > 300, "most lines elided");
    ok(!c.grew, "and it did not grow");
}

// The rule that recovers the diagnosis, not just the fact of failure. Measured on a real
// pytest run, needle-matching alone kept the Traceback header and the final KeyError and
// dropped all 8 frames — the model learned an exception happened and could not see where.
void test_indented_continuation_survives() {
    std::string log = big_log(200, "collecting");
    log += "Traceback (most recent call last):\n";
    log += "  File \"/x/boom.py\", line 3, in <module>\n";
    log += "    raise KeyError('missing')\n";
    log += "KeyError: 'missing'\n";
    log += big_log(200, "teardown");

    const auto c = cml::condense(log, "/tmp/spill/1.txt");
    ok(c.text.find("Traceback (most recent call last):") != std::string::npos,
       "the traceback header is flagged");
    ok(c.text.find("boom.py") != std::string::npos,
       "and its indented frame comes with it — WHERE it failed, not just THAT it failed");
    ok(c.text.find("raise KeyError('missing')") != std::string::npos,
       "the whole contiguous indented run, not only the first line");
}

void test_gap_markers_prevent_false_adjacency() {
    const std::string log = big_log(200, "compiling");
    const auto c = cml::condense(log, "/tmp/spill/1.txt");
    ok(c.text.find("\xE2\x80\xA6") != std::string::npos,
       "a jump is marked, or the text claims lines were adjacent that never were");
    // The head ends at index 4 and the tail resumes at 185; without a marker between them
    // the reader sees "compiling 4" directly above "compiling 185".
    const auto h = c.text.find("compiling 4");
    const auto t = c.text.find("compiling 185");
    ok(h != std::string::npos && t != std::string::npos && h < t, "both edges present");
    ok(c.text.find("\xE2\x80\xA6", h) < t, "and the marker sits between them");
}

void test_short_output_is_returned_untouched() {
    // 20 lines or fewer keeps every line by construction (kHead + kTail == 20), so condensing
    // can only add a header. A 9,692-byte single-line curl result measured 9,770 bytes out.
    const std::string one_liner(9000, 'x');
    const auto c = cml::condense(one_liner, "/tmp/spill/1.txt");
    ok(c.grew, "growth is detected");
    ok(c.text == one_liner, "and the original is handed back byte-for-byte, header and all");
    ok(c.elided == 0, "with nothing claimed as elided");
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

void test_warnings_are_kept_not_merely_counted() {
    // This tree builds with -Werror, which makes a warning an error. Counting them and
    // eliding them tells the model "38 warnings" and hides every one. It also removes an
    // accident: `-Werror=unused-variable` used to match the *error* needle while
    // `-Wunused-variable` did not, so the same warning survived or vanished depending on
    // how the flag was spelled.
    std::string log = big_log(200, "compiling");
    log += "foo.cpp:3:9: warning: unused variable 'x' [-Wunused-variable]\n";
    log += big_log(200, "compiling more");
    const auto c = cml::condense(log, "/tmp/s.txt");
    ok(c.text.find("unused variable 'x'") != std::string::npos,
       "a warning is kept whichever way the flag is spelled");
}

void test_a_successful_listing_is_not_reported_as_errors() {
    // Measured on a real `npm ls`: three package names containing "error" were reported to
    // the model as "3 errors" from a command that exited 0.
    std::string log = big_log(200, "dependency");
    log += "├── error-ex@1.3.2\n";
    const auto c = cml::condense(log, "/tmp/s.txt");
    ok(c.text.find(" flagged") != std::string::npos,
       "the header says flagged, never errors — a substring match is not a verdict");
    ok(c.text.find(" errors") == std::string::npos, "and does not claim errors at all");
}

// N1. The drag used to be seeded from ANY kept line, so line 4 is kept as a head edge, an
// indented body hangs off it, and the run swallows the whole file. Measured on a 483-line
// pretty JSON: 96% saved became 0% saved, silently — and no test in this suite could see it,
// which is the actual defect. Pretty JSON, `kubectl get -o yaml` and `curl | jq` are the
// commonest large Bash results there are.
void test_indented_body_still_condenses() {
    std::string log = "{\n";
    for (int i = 0; i < 400; ++i)
        log += "    \"key" + std::to_string(i) + "\": " + std::to_string(i) + ",\n";
    log += "}\n";
    const auto c = cml::condense(log, "/tmp/s.txt");
    ok(!c.grew, "an all-indented body is still condensed, not handed back whole");
    ok(c.text.size() < log.size() / 4, "and by a lot");
}

// GCC prints its instantiation chain ABOVE the error line and at column 0, so neither the
// needles nor the downward indent rule can reach it. Without the bounded upward pass, every
// surviving `error:` line points into /usr/include/c++/13 and the file the user owns — the
// only line in the block they can act on — greps zero.
void test_gcc_instantiation_chain_names_the_users_file() {
    std::string log = big_log(60, "[..] Building CXX object CMakeFiles/app.dir/src/mod");
    log += "/usr/include/c++/13/bits/predefined_ops.h: In instantiation of 'bool _Iter_less()'\n";
    log += "/usr/include/c++/13/bits/stl_algo.h:1819:14:   required from 'void __insertion_sort()'\n";
    log += "tpl2.cpp:6:14:   required from here\n";
    log += "/usr/include/c++/13/bits/predefined_ops.h:45:23: error: no match for 'operator<'\n";
    log += big_log(60, "[..] Building CXX object CMakeFiles/app.dir/src/mod");
    const auto c = cml::condense(log, "/tmp/s.txt");
    ok(c.text.find("tpl2.cpp:6") != std::string::npos,
       "the line naming the file the user owns survives, not just the system-header error");
    ok(c.text.find("error: no match for 'operator<'") != std::string::npos,
       "along with the error it belongs to");
}

// N2. `elided` was reset on the grew path and `flagged` was not, so a Condensed documented
// as "the untouched original" carried a count from the pass that was discarded.
void test_grew_path_reports_no_stale_counts() {
    std::string small = "error: something broke\n";
    small += std::string(4000, 'y') + "\n";
    const auto c = cml::condense(small, "/tmp/s.txt");
    ok(c.grew, "two lines can only grow under a header");
    ok(c.text == small, "so the original comes back untouched");
    ok(c.flagged == 0, "carrying no count from the pass that was thrown away");
}

}  // namespace

void suite_offload() {
    test_allowlist_and_floor();
    test_errors_survive_condensation();
    test_indented_continuation_survives();
    test_gap_markers_prevent_false_adjacency();
    test_short_output_is_returned_untouched();
    test_head_and_tail_kept();
    test_spill_path_is_named_in_the_output();
    test_warnings_are_kept_not_merely_counted();
    test_a_successful_listing_is_not_reported_as_errors();
    test_indented_body_still_condenses();
    test_gcc_instantiation_chain_names_the_users_file();
    test_grew_path_reports_no_stale_counts();
}

}  // namespace cml_test
