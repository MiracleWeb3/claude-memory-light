// Stop hook: append a digest of the turn to the per-project learning inbox.
//
// Design law, inherited from the Rust binary and from claude-mem's issue tracker:
// a memory hook must NEVER block or break the session. Every path here returns
// cleanly and main() exits 0 regardless.

#include <simdjson.h>

#include <filesystem>
#include <fstream>
#include <iostream>
#include <string>

#include "noise.hpp"
#include "paths.hpp"
#include "work.hpp"

namespace cml {

void capture() {
    std::string payload((std::istreambuf_iterator<char>(std::cin)),
                        std::istreambuf_iterator<char>());
    if (payload.empty()) return;

    simdjson::dom::parser parser;
    simdjson::dom::element data;
    if (parser.parse(payload).get(data) != simdjson::SUCCESS) return;

    std::string_view cwd, transcript;
    if (data["cwd"].get(cwd) != simdjson::SUCCESS) return;
    if (data["transcript_path"].get(transcript) != simdjson::SUCCESS) return;
    if (cwd.empty() || transcript.empty()) return;

    const Digest d = digest(std::string(transcript),
                            [](const std::string& t) { return !is_noise(t); });
    if (d.ask.empty() || is_noise(d.ask)) return;

    const char* flag = looks_like_correction(d.ask) ? "correction?" : "note";
    std::string line = "- [" + iso_minute(now_secs()) + "] (" + flag + ") " +
                       squeeze(d.ask, 300) + "\n";
    if (d.has_work()) {
        for (const auto& extra : d.detail_lines()) {
            line += extra;
            line.push_back('\n');
        }
    }

    const std::filesystem::path inbox = inbox_path_for(cwd);
    std::error_code ec;
    std::filesystem::create_directories(inbox.parent_path(), ec);

    std::ofstream out(inbox, std::ios::app);
    if (out) out << line;
}

}  // namespace cml
