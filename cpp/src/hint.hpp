// UserPromptSubmit hook: a zero-token nudge when a prompt smells durable.
//
// Classification is a phrase table, not a model — obsidian-mind proved the cheap
// version works. The hint is a routing suggestion the model may ignore; it never
// writes anything itself, and each category fires at most once per session.
#pragma once

#include <string_view>

namespace cml {

// Category name ("correction", "preference", "decision", "method", "reference")
// or nullptr when the prompt is ordinary work. Pure, for the selftest.
const char* classify_prompt(std::string_view prompt);

// Hook entry: reads the event JSON on stdin. Exits cleanly on every path.
void hint();

}  // namespace cml
