// Chronic loops: user asks that recur across sessions, unresolved by inertia.
//
// The signal obsidian-mind hand-computes by diffing 1:1 notes falls out of the
// index for free — every ask ever made is already a row. The pure grouping is
// split from the query so the selftest can assert on it without a database.
#pragma once

#include <cstddef>
#include <string>
#include <vector>

namespace cml {

class Db;

struct Ask {
    std::string text;
    std::string session;
    std::string ts;  // ISO 8601, straight from the transcript
};

// Formatted "carried across N sessions since YYYY-MM-DD: ..." lines, most
// recurrent first. Only groups seen in >= min_sessions distinct sessions.
std::vector<std::string> group_recurring(const std::vector<Ask>& asks,
                                         std::size_t min_sessions, std::size_t limit);

// Query + group over the live index; shared by the CLI and the SessionStart brief.
std::vector<std::string> loop_lines(Db& db, int days, std::size_t limit);

// cml loops [--days N] [--limit K]
int loops(const std::vector<std::string>& args);

}  // namespace cml
