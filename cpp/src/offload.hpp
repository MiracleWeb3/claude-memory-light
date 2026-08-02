// PostToolUse: condense a large tool result before it reaches the context window.
//
// Measured over 146 transcripts on this machine: 19,662 tool results, 18.3 MB, median
// 239 bytes — but the 12.4% at or above 2 KB carry 63.1% of all bytes. One call in
// eight is worth touching; the other seven are already small and are left alone.
//
// Nothing here calls an LLM. A hook that blocks on a network round-trip stalls every
// tool call in the session, so the rules are fixed: keep the errors, keep the edges,
// count the rest.
#pragma once

#include <cstddef>
#include <string>
#include <string_view>

namespace cml {

struct Condensed {
    std::string text;
    std::size_t elided = 0;
    // "flagged", not "errors": the rule is a substring match, and a successful `npm ls`
    // listing error-ex@1.3.2 is not three errors. The header states this number to a model
    // that will reason over it, so it must claim only what it knows — lines worth your eyes.
    std::size_t flagged = 0;
    bool grew = false;
};

// Allowlist, deliberately: an unknown tool is left alone, so a tool added by a future
// Claude Code version is never silently condensed.
bool condensable(std::string_view tool_name, std::size_t bytes);

// Pure. `spill_path` is named in the replacement so the original stays reachable;
// pass empty to omit the pointer.
Condensed condense(std::string_view out, std::string_view spill_path);

// The hook entry: reads PostToolUse JSON on stdin, writes hook JSON on stdout.
void offload();

}  // namespace cml
