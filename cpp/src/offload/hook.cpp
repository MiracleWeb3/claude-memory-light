// PostToolUse: swap the big Bash result for its condensed form, after the original is safe
// on disk.
//
// The replacement is what gets persisted to the transcript — the original output is gone
// from it entirely — and `updatedToolOutput` is validated against the tool's own output
// schema and REJECTED SILENTLY on a mismatch. The binary carries the message "PostToolUse
// hook returned updatedToolOutput that does not match `${toolName}`'s output shape; using
// original output", but it never reaches stdout, stderr, or --debug. A wrong shape is
// indistinguishable from a hook that did nothing, which is why the emitted object mirrors
// Bash's five keys instead of the bare string that reads so much more naturally.

#include <simdjson.h>

#include <cstdio>
#include <iostream>
#include <string>

#include "json.hpp"
#include "offload.hpp"

namespace cml {

void offload() {
    // Whatever happens, the tool result must go through untouched.
    const auto passthrough = []() { std::printf("{\"continue\": true}\n"); };

    std::string payload((std::istreambuf_iterator<char>(std::cin)),
                        std::istreambuf_iterator<char>());
    if (payload.empty()) return passthrough();

    simdjson::dom::parser parser;
    simdjson::dom::element data;
    if (parser.parse(payload).get(data) != simdjson::SUCCESS) return passthrough();

    // Field names taken from cpp/testdata/posttooluse-payload.json — a recorded payload,
    // not the documentation. `tool_response` is an object, never a string.
    std::string_view tool, session, stdout_text;
    if (data["tool_name"].get(tool) != simdjson::SUCCESS) return passthrough();
    if (data["session_id"].get(session) != simdjson::SUCCESS) session = {};
    if (data["tool_response"]["stdout"].get(stdout_text) != simdjson::SUCCESS)
        return passthrough();

    if (!condensable(tool, stdout_text.size())) return passthrough();

    std::string_view tuid;
    if (data["tool_use_id"].get(tuid) != simdjson::SUCCESS) tuid = {};
    // The spill goes first, and a failed spill means no replacement at all: the transcript
    // keeps only what we emit, so condensing without a saved original destroys the output
    // permanently.
    const std::string spill = spill_write(session, tuid, stdout_text);
    if (spill.empty()) return passthrough();
    const auto c = condense(stdout_text, spill);

    // condense() detects its own growth and hands back the original; that is a no-op here.
    if (c.grew) return passthrough();

    // Every sibling field is echoed back from the payload unchanged; only stdout moves.
    std::string_view stderr_text;
    if (data["tool_response"]["stderr"].get(stderr_text) != simdjson::SUCCESS) stderr_text = {};
    const auto flag = [&](const char* k) {
        bool v = false;
        return data["tool_response"][k].get(v) == simdjson::SUCCESS && v;
    };

    std::string out =
        "{\"continue\": true, \"hookSpecificOutput\": {\"hookEventName\": "
        "\"PostToolUse\", \"updatedToolOutput\": {\"stdout\": ";
    json::quote_into(out, c.text);
    out += ", \"stderr\": ";
    json::quote_into(out, stderr_text);
    out += std::string(", \"interrupted\": ") + (flag("interrupted") ? "true" : "false");
    out += std::string(", \"isImage\": ") + (flag("isImage") ? "true" : "false");
    out += std::string(", \"noOutputExpected\": ") + (flag("noOutputExpected") ? "true" : "false");
    out += "}}}";
    std::printf("%s\n", out.c_str());
}

}  // namespace cml
