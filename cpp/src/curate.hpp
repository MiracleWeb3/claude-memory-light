// The two commands that remove things from the brain.
//
// `forget` is the manual scalpel, `distill` the automatic one: an LLM reads rows the
// index already holds and votes each one keep-or-drop. Both write the dropped row's
// stable_key into `forgotten`, which is what stops the next `cml index` from walking
// it straight back in.
#pragma once

#include <cstddef>
#include <optional>
#include <string>
#include <utility>
#include <vector>

#include "db.hpp"

namespace cml {

// cml forget <rowid...> | --match "<query>" [--yes] | --clear
int forget(const std::vector<std::string>& args);

// cml distill [--all]
int distill(const std::vector<std::string>& args);

// FTS5 phrase query behind `forget --match`: whitespace-split, every token quoted,
// all of them required.
std::string match_query(const std::string& raw);

// Blocklist a row by its stable_key, then delete it (and its embedding, if any).
// False when the rowid is already gone. Shared by both commands.
bool purge_rowid(Db& db, std::int64_t id);

// Curator credentials: $CML_LLM_KEY, else <data_dir>/llm.key or deepseek.key,
// else $DEEPSEEK_API_KEY. Empty is treated as absent — no key means no-op, never
// an error, so the hook path stays silent on machines without one.
std::optional<std::string> llm_key();

// (endpoint, model) — any OpenAI-compatible /chat/completions works.
std::pair<std::string, std::string> llm_conf();

struct Verdict {
    std::int64_t id = 0;
    bool keep = false;
    std::string gist;
};

// The two halves of a judging round that touch no network and no disk, split out
// so they can be asserted on: the request body, and the verdicts dug out of the
// reply. `response` is taken by reference because the parser pads it in place.
std::string build_judge_request(const std::string& model,
                                const std::vector<std::pair<std::int64_t, std::string>>& rows);
std::optional<std::vector<Verdict>> parse_verdicts(std::string& response, std::string& err);

// Judges every unjudged assistant/summary row. `cap` limits rows per run so the
// Stop hook stays fast; nullopt drains everything. Returns (kept, dropped).
// Index-time curation calls this with cap=40, verbose=false.
std::pair<std::size_t, std::size_t> distill_new(Db& db, const std::string& key,
                                                std::optional<std::size_t> cap, bool verbose);

}  // namespace cml
