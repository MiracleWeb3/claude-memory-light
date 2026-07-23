// Work digest: what actually happened during a turn, not what was asked.
//
// Capturing the prompt records the *question* — which the user already knows and can
// already read. This records the *work*: files touched, commands run, whether anything
// failed, and how the turn ended. That is the part nobody remembers a week later.
//
// Pure transcript parsing. No LLM call, no network, no state.
#pragma once

#include <cstddef>
#include <functional>
#include <string>
#include <vector>

namespace cml {

struct Digest {
    std::string ask;
    std::vector<std::string> files;
    std::vector<std::string> commands;
    std::vector<std::string> skills;
    std::size_t failures = 0;
    std::string outcome;

    // True when the turn did something worth recording beyond the ask itself.
    bool has_work() const {
        return !files.empty() || !commands.empty() || !skills.empty();
    }

    // Indented continuation lines. Empty for a pure-conversation turn, so the
    // caller falls back to a plain one-line entry.
    std::vector<std::string> detail_lines() const;
};

// Digest of the LAST turn in the transcript.
//
// Single pass, no buffering: every real user message resets the accumulator, so
// whatever survives to EOF belongs to the final turn. `is_real_user_msg` lets the
// caller supply its own noise filter — hook injections and slash-command envelopes
// wear the user role and must not start a new turn.
Digest digest(const std::string& transcript_path,
              const std::function<bool(const std::string&)>& is_real_user_msg);

}  // namespace cml
