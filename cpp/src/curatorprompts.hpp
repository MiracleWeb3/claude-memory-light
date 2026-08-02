// The curator's rubrics. One per row author, because the two are not judged alike.
//
// An assistant row earns its place by standing alone: read it by itself and you either
// learn something reusable or you don't. A *user* row often cannot pass that test and
// still matters enormously — "wtf this doesnt touch real db?" reads as noise in
// isolation and is the origin of a standing rule. Its value is relational.
//
// Measured on this corpus (Jul 26 2026): judging user rows with the assistant rubric
// dropped 3 messages that had already become permanent rules. Hence two rubrics.
#pragma once

#include <string_view>

namespace cml {

// Assistant and User judge KEEP (is there content?). Durability is a SEPARATE second call
// judging only whether a kept row earns a gist.
//
// It has to be its own call. Appending the durability clause to a keep rubric was measured
// at 45-84% minted against 16% for the same clause asked on its own: the keep list is
// deliberately generous, and that generosity bleeds into the gist decision when one call
// is asked to do both.
// Scene is the odd one out: it judges nothing and drops nothing. It reads a whole
// session and writes one L2 summary of it, so it is a rubric only in the sense that it
// is a system prompt selected here. It rides the same wire as the rest.
enum class Rubric { Assistant, User, Durability, Scene };

// The system prompt for a rubric. Editing these strings IS a behaviour change.
std::string_view curator_prompt(Rubric r);

// Which rubric judges a row of this role. Unknown roles fall back to Assistant.
Rubric rubric_for_role(std::string_view role);

}  // namespace cml
