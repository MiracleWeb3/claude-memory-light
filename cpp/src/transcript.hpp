// Session transcript parsing.
//
// Claude Code writes each content block as its own JSONL entry, so narration cannot be
// spotted within a single line. Parse the whole stream first, then keep an assistant
// text only if NOTHING tool-related follows it before the next human message — that is
// the turn's actual answer. Everything else is "doing X now" and is not memory.
#pragma once

#include <cstddef>
#include <string>
#include <vector>

namespace cml {

struct Entry {
    enum class Kind { AssistantText, AssistantTool, UserHuman, UserTool, Summary };
    Kind kind = Kind::Summary;
    std::string text;
    std::string ts;
    std::string session;
};

std::vector<Entry> parse_transcript(const std::string& path,
                                    const std::string& session_fallback);

// True when entries[i] is the last assistant text before the next human turn.
bool turn_final(const std::vector<Entry>& entries, std::size_t i);

}  // namespace cml
