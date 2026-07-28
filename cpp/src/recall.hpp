// UserPromptSubmit hook: the read half of the memory loop.
#pragma once

#include <cstddef>
#include <string>
#include <string_view>
#include <vector>

namespace cml {

// Retrieve against the incoming prompt and inject what clears the gate.
// Always prints valid passthrough JSON — a hook must never block the session.
void recall();

// Content words worth querying on: lowercased, >=3 chars, de-duplicated, function
// words dropped, capped. Exposed for the selftest.
std::vector<std::string> content_terms(std::string_view prompt);

// How many distinct query terms appear in `text`. The relevance floor: a row that
// shares one word with a prompt is a coincidence, two is a topic.
std::size_t term_overlap(std::string_view text, const std::vector<std::string>& terms);

}  // namespace cml
