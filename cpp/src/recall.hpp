// UserPromptSubmit hook: the read half of the memory loop.
#pragma once

#include <cstddef>
#include <cstdint>
#include <string>
#include <string_view>
#include <vector>

#include "db.hpp"

namespace cml {

struct Hit {
    std::int64_t rowid;
    std::string line;  // "2026-07-08 memory/project: text…", ready to inject
    std::string key;   // stable_key, so dedupe survives a reindex changing rowids
};

// The retrieval core, shared by the hook and the benchmark. Applies every gate except
// per-session dedupe — that is the hook's business, and running it inside a benchmark
// would both pollute the table and change the numbers being measured.
//
// `exclude_session` drops rows from one session: the live hook excludes the session
// being typed in (already on screen), and `cml eval` excludes the session a question
// was asked in (the point is whether the answer is findable from somewhere else).
// `keyword_only` drops the embedding leg, so `cml eval` can ablate it. It is a
// measurement switch, not a setting: the hook always runs the full hybrid.
std::vector<Hit> retrieve(Db& db, std::string_view prompt, std::string_view exclude_session,
                          std::size_t limit, bool keyword_only = false);

// Retrieve against the incoming prompt and inject what clears the gate.
// Always prints valid passthrough JSON — a hook must never block the session.
void recall();

// `cml eval` — recall@k over your own history. Returns a process exit code.
int eval(const std::vector<std::string>& args);

// Content words worth querying on: lowercased, >=3 chars, de-duplicated, function
// words dropped, capped. Exposed for the selftest.
std::vector<std::string> content_terms(std::string_view prompt);

// How many distinct query terms appear in `text`. The relevance floor: a row that
// shares one word with a prompt is a coincidence, two is a topic.
std::size_t term_overlap(std::string_view text, const std::vector<std::string>& terms);

}  // namespace cml
