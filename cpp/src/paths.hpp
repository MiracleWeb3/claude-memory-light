// Filesystem layout and time formatting. Must match the Rust binary byte for byte:
// both write the same inbox files and the same index.db, so a divergence here
// silently splits the memory in two.
#pragma once

#include <cstdint>
#include <filesystem>
#include <string>
#include <string_view>

namespace cml {

std::filesystem::path home();

// $CML_HOME, else ~/.claude/claude-memory-light
std::filesystem::path data_dir();

// "/home/user" -> "-home-user"  (Claude Code's project-dir flattening)
std::string flatten(std::string_view path);

std::string flat_home();

// A flattened project dir name -> the short label used for inbox filenames.
std::string project_label(std::string_view dirname);

std::filesystem::path inbox_path_for(std::string_view cwd);

std::int64_t now_secs();

// Unix seconds -> "YYYY-MM-DD HH:MM" (UTC).
std::string iso_minute(std::int64_t secs);

// Unix seconds -> "YYYY-MM-DD" (UTC). Used as the ts for whole-file markdown rows.
std::string iso_date(std::int64_t secs);

}  // namespace cml
