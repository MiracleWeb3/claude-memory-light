#pragma once

#include <string>
#include <vector>

namespace cml {

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
