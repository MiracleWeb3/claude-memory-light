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
// Ablation switches for `cml eval`. Measurement only — the defaults are what the
// hook runs.
//
// `with_vectors` is OFF by default because the embedding leg was measured to make
// this path worse in every configuration tried: -5/-9 rows with potion-base-8M,
// -2/-3 with a real bge-small transformer, and identical numbers ungated. It stays
// switchable so the finding can be re-checked rather than taken on trust.
// `text_only` confines the FTS5 match to the text column, which is how the index
// behaves with no doc2query expansions, without having to throw them away to find out.
struct RetrieveOpts {
    bool with_vectors = false;
    bool text_only = false;
    bool ungated = false;  // only meaningful with with_vectors
};

std::vector<Hit> retrieve(Db& db, std::string_view prompt, std::string_view exclude_session,
                          std::size_t limit, RetrieveOpts opts = {});

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
