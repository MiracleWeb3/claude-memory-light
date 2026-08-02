// L2: one summary per real working session, written by the curator under Rubric::Scene.
//
// Lives apart from distill.cpp because it shares nothing with row distillation but the
// wire: nothing is kept or dropped here, the unit is a session rather than a row, and
// the output is a new `scene` row rather than a verdict against an existing one.
#pragma once

#include <cstddef>
#include <optional>
#include <string>
#include <unordered_map>

#include "db.hpp"

namespace cml {

// One session as the curator reads it: a line per turn, the curated gist where the row
// earned one (see gist_lookup), otherwise the row clipped. Over the size cap it keeps
// the first two turns and the ENDING — exposed only because that is the half nobody can
// see is wrong: a digest cut to a prefix still produces a fluent, confident summary, and
// `outcome` is decided by how a session finished.
std::string session_digest(Db& db, const std::string& session,
                           const std::unordered_map<std::string, std::string>& gists);

// Summarises every eligible session not already in `scene`, newest first, and returns
// how many were written. `cap` bounds the run in SESSIONS, not rows — the point of it
// is calibrating on 20 before spending the rest of the money. Sessions whose call fails
// are simply left out of `scene`, so the next run retries them.
std::size_t build_scenes(Db& db, const std::string& key, std::optional<std::size_t> cap,
                         bool verbose);

}  // namespace cml
