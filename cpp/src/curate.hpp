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

#include "curatorprompts.hpp"
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
    // doc2query expansions: how someone would ASK for this row months later, in words
    // the row itself does not contain. Indexed alongside the text so keyword search
    // stops depending on the user remembering the vocabulary they used at the time.
    std::string asks;
};

// The two halves of a judging round that touch no network and no disk, split out
// so they can be asserted on: the request body, and the verdicts dug out of the
// reply. `response` is taken by reference because the parser pads it in place.
std::string build_judge_request(const std::string& model,
                                const std::vector<std::pair<std::int64_t, std::string>>& rows,
                                Rubric rubric = Rubric::Assistant);
std::optional<std::vector<Verdict>> parse_verdicts(std::string& response, std::string& err);

// Judges every unjudged row belonging to `rubric` (Assistant covers assistant and
// summary rows, User covers the developer's own messages). `cap` limits rows per
// run; nullopt drains everything. Returns (kept, dropped). The only caller is the
// `distill` command, which the background curator re-execs with --limit — nothing
// curates on the Stop hook's own clock any more (indexer/curator.hpp).
std::pair<std::size_t, std::size_t> distill_new(Db& db, const std::string& key,
                                                std::optional<std::size_t> cap, bool verbose,
                                                Rubric rubric = Rubric::Assistant);

}  // namespace cml
