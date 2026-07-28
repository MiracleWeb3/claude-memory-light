// The always-present layer.
//
// Retrieval answers a question. This answers none — it is simply there, every session,
// before anything is asked. That difference is the whole gap against compaction-based
// memory: a tool that surfaces three rows when a prompt happens to match feels like a
// search engine, and a tool that always knows where you are feels like memory.
//
// Built by SELECTION, never by paraphrase. Every line is a real row with a real date,
// so it cannot drift from what happened, and it costs no model call. A summariser has
// to be re-run to improve and can only ever re-read its own summary; this is recomputed
// from the complete archive every time, so it improves retroactively over all history.
#pragma once

#include <cstddef>
#include <string>
#include <string_view>
#include <vector>

#include "db.hpp"

namespace cml {

// Compact standing state for one project: what was decided, what is unresolved, what
// was last touched. Empty when the project has nothing on file yet.
std::string project_state(Db& db, std::string_view project, std::size_t budget);

// `cml state [--project P] [--budget N]` — print it, for looking at by hand.
int state_cmd(const std::vector<std::string>& args);

}  // namespace cml
