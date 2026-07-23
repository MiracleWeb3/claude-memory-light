// Semantic layer: sqlite-vec (vendored, a single C file) plus model2vec vectors.
//
// The Rust build reached sqlite-vec through a crate wrapper. It is C, so C++ compiles
// it straight into the binary — one fewer moving part, and the vendored source is
// pinned rather than resolved at build time.
#pragma once

#include <cstdint>
#include <string>
#include <vector>

#include "db.hpp"

namespace cml {

// Registers sqlite-vec as an auto-extension. Must run before any connection opens;
// calling it repeatedly is harmless.
void register_vec_extension();

// `cml embed [--all]` — build or extend the vector index. Returns an exit code.
int embed_cmd(const std::vector<std::string>& args);

// Embed every mem row that has no vector yet; returns how many were added.
// Only does anything once `vec_mem` exists, so an indexing hook never triggers a
// model load for a user who has not opted into semantic search.
std::size_t embed_new(Db& db);

// Nearest-neighbour rowids for a query string, or empty when the vector table is
// absent or the model cannot be loaded. `error` explains an empty result.
std::vector<std::int64_t> semantic_hits(Db& db, const std::string& query, int k,
                                        std::string& error);

}  // namespace cml
