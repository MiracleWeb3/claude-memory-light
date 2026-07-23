#pragma once

namespace cml {

// Walk ~/.claude/projects for transcripts and memory notes, plus the wiki vault, and
// bring the FTS5 index up to date. `force` re-indexes files that look unchanged.
// Prints the same one-line summary as the Rust binary. Returns a process exit code.
int index_all(bool force);

}  // namespace cml
