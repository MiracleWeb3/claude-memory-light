// The endpoint half of distillation: build a request, POST it, dig the verdicts out.
//
// Split from distill.cpp so the policy half (which rows are judged, under which rubric,
// and what happens to the answer) stays readable on one screen. Transport is a genuine
// seam: nothing here knows what a rubric means.
#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <utility>
#include <vector>

#include "curate.hpp"
#include "curatorprompts.hpp"

namespace cml {

// One call judging a batch of rows under `rubric`. nullopt with `err` set on any failure;
// the caller leaves those rows unjudged so the next run retries them.
std::optional<std::vector<Verdict>> judge_batch(
    const std::string& key, const std::vector<std::pair<std::int64_t, std::string>>& rows,
    std::string& err, Rubric rubric);

// The same call under Rubric::Scene, where a "row" is a whole session's digest and the
// reply is summaries rather than verdicts. Same failure contract: nullopt leaves the
// sessions unwritten, and `build_scenes` picks them up on the next run.
std::optional<std::vector<Scene>> judge_scenes(
    const std::string& key, const std::vector<std::pair<std::int64_t, std::string>>& rows,
    std::string& err);

}  // namespace cml
