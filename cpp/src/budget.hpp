// A wall-clock budget for work that runs from a hook.
//
// `cml index` runs on the Stop hook of every turn, so its cost has to be bounded no matter
// how far behind the index has fallen or how the network is behaving. Measured 2026-08-02:
// a deepseek hiccup let two `curl --max-time 90` calls stack up inside one index_all, and
// the Stop phase sat at 3m24s. Every unit of work here is already resumable — transcripts
// commit per file, curation and embedding drain over turns — so stopping early costs
// nothing but a little latency on the next turn.
#pragma once

#include <algorithm>
#include <chrono>
#include <filesystem>
#include <fstream>
#include <cstdlib>
#include <string>

#include "paths.hpp"

namespace cml {

class Budget {
  public:
    explicit Budget(int ms) : end_(clock::now() + std::chrono::milliseconds(ms)) {}

    bool spent() const { return clock::now() >= end_; }

    // Whole seconds left, floored at 1 — curl takes seconds and 0 would mean "no limit".
    int seconds_left() const {
        const auto ms = std::chrono::duration_cast<std::chrono::milliseconds>(end_ - clock::now());
        return std::max(1, static_cast<int>((ms.count() + 999) / 1000));
    }

  private:
    using clock = std::chrono::steady_clock;
    clock::time_point end_;
};

// Default 4s: comfortably above a warm run (~420ms measured) and far below the point where
// a turn feels stalled. CML_INDEX_BUDGET_MS overrides; 0 disables the budget entirely.
inline int index_budget_ms() {
    if (const char* e = std::getenv("CML_INDEX_BUDGET_MS")) {
        const int v = std::atoi(e);
        if (v > 0) return v;
        if (std::string(e) == "0") return 24 * 60 * 60 * 1000;  // effectively unbounded
    }
    return 4000;
}


// A curator that is not answering must not be re-tried on every single turn. The stamp is
// a file rather than a DB row on purpose: this has to work even when the index is locked
// or mid-write, and losing it just means one extra attempt.
inline std::filesystem::path curator_stamp() { return data_dir() / "curator-standdown"; }

inline bool curator_backoff_active() {
    std::error_code ec;
    const auto p = curator_stamp();
    if (!std::filesystem::exists(p, ec)) return false;
    std::ifstream in(p);
    long long until = 0;
    if (!(in >> until)) return false;
    const auto now = std::chrono::duration_cast<std::chrono::seconds>(
                         std::chrono::system_clock::now().time_since_epoch()).count();
    return now < until;
}

inline void start_curator_backoff(int minutes = 15) {
    const auto now = std::chrono::duration_cast<std::chrono::seconds>(
                         std::chrono::system_clock::now().time_since_epoch()).count();
    std::error_code ec;
    std::filesystem::create_directories(curator_stamp().parent_path(), ec);
    std::ofstream(curator_stamp()) << (now + minutes * 60);
}

inline void clear_curator_backoff() {
    std::error_code ec;
    std::filesystem::remove(curator_stamp(), ec);
}

}  // namespace cml
