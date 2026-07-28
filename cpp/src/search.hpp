#pragma once

#include <cstdint>
#include <string>
#include <vector>

#include "db.hpp"

namespace cml {

// Ranked rowids for an already-built FTS5 query, fused with the embedding leg by
// reciprocal rank fusion. Either leg is skipped by passing an empty string for it.
// The vector leg RERANKS what the keyword leg found and may never add rows of its
// own — see search.cpp for the measurement that forced that gate.
std::vector<std::int64_t> rank_rowids(Db& db, const std::string& fts,
                                      const std::string& semantic_query, int candidates,
                                      std::string* semantic_error = nullptr);

// Keyword search over the FTS5 index. Returns a process exit code.
int search(const std::vector<std::string>& args);

// One-line index summary: rows by role, sessions, files, size on disk.
int stats();

// Environment check: directories, index, optional companions. Ends with stats().
int doctor();

// SessionStart hook: emit the consolidation nudge when the inbox has piled up.
// Always prints valid passthrough JSON — a hook must never block the session.
void nudge();

}  // namespace cml
