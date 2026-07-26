#include <string>

#include "curate.hpp"
#include "harness.hpp"

namespace cml_test {
namespace {

void test_forget_match_builds_an_fts_query() {
    ok(cml::match_query("touchpad tapdrag") == "\"touchpad\" AND \"tapdrag\"", "tokens ANDed");
    ok(cml::match_query("  spaced \t out\n") == "\"spaced\" AND \"out\"", "any whitespace splits");
    ok(cml::match_query("say \"hi\"") == "\"say\" AND \"\"\"hi\"\"\"", "FTS5 doubles the quote");
    ok(cml::match_query("   ").empty(), "nothing to match");
}

void test_judge_request_is_the_shape_deepseek_expects() {
    const std::string req = cml::build_judge_request("m-1", {{7, "line \"one\"\nline two"}});
    // serde_json orders object keys alphabetically; so does this.
    ok(req.rfind("{\"messages\":[{\"content\":\"You curate a developer's", 0) == 0, "system first");
    ok(req.find("\\\"verdicts\\\"") != std::string::npos, "prompt's own quotes escaped");
    ok(req.find("{\\\"rows\\\":[{\\\"id\\\":7,\\\"text\\\":\\\"line \\\\\\\"one\\\\\\\"\\\\nline "
                "two\\\"}]}") != std::string::npos,
       "rows are a JSON string inside the JSON body");
    ok(req.find("\"role\":\"user\"}],\"model\":\"m-1\",\"response_format\":{\"type\":\"json_"
                "object\"},\"temperature\":0.0}") != std::string::npos,
       "model, format and temperature");
}

// The user rubric exists to stop one specific loss: a standing instruction phrased
// casually, dropped for reading like chatter. Judged with the assistant rubric, three
// messages that had already become permanent rules were marked drop. These assertions
// fail if that guard is ever edited back out of the prompt.
void test_user_rows_are_judged_by_their_own_rubric() {
    ok(cml::rubric_for_role("user") == cml::Rubric::User, "user rows get the user rubric");
    ok(cml::rubric_for_role("assistant") == cml::Rubric::Assistant, "assistant rows do not");
    ok(cml::rubric_for_role("summary") == cml::Rubric::Assistant, "nor do summaries");

    const std::string u(cml::curator_prompt(cml::Rubric::User));
    const std::string a(cml::curator_prompt(cml::Rubric::Assistant));
    ok(u != a, "the two rubrics are different prompts");
    ok(u.find("ALWAYS keep") != std::string::npos, "corrections are an unconditional keep");
    ok(u.find("Tone is not content") != std::string::npos, "a sworn rule is still a rule");
    ok(u.find("their own build") != std::string::npos, "the user is authority on their system");
    // The assistant rubric breaks ties toward DROP; the user rubric must not.
    ok(a.find("When unsure, DROP") != std::string::npos, "assistant tie-break unchanged");
    ok(u.find("When unsure, DROP") == std::string::npos, "user rubric never defaults to drop");

    const std::string req = cml::build_judge_request("m-1", {{7, "hi"}}, cml::Rubric::User);
    ok(req.find("developer's own messages") != std::string::npos, "user prompt reaches the wire");
}

// Keeping and gisting are independent axes. A row can be worth searching without being
// worth plotting, and that state is an empty gist — already understood by gist_lookup
// ("WHERE gist != ''") and by the judged-set ("SELECT key FROM distilled"), so a kept row
// with no gist is searchable, unplotted, and never re-judged. These assertions fail if the
// durability clause is edited out of either rubric, collapsing the two axes back into one.
void test_durability_is_judged_apart_from_keeping() {
    const std::string a(cml::curator_prompt(cml::Rubric::Assistant));
    const std::string u(cml::curator_prompt(cml::Rubric::User));
    const std::string dur(cml::curator_prompt(cml::Rubric::Durability));

    // The three calibrated tests live in the durability prompt and nowhere else. Each one
    // measurably lowers the mint rate: clause alone 80%, +five-minute 30%, +decision+cost 19%.
    ok(dur.find("FIVE-MINUTE TEST") != std::string::npos, "the five-minute test survives");
    ok(dur.find("DECISION TEST") != std::string::npos, "the decision test survives");
    ok(dur.find("COST TEST") != std::string::npos, "the cost test survives");
    ok(dur.find("When unsure, gist:\"\"") != std::string::npos, "refusing a gist is the default");
    ok(dur.find("NOT deciding whether to keep") != std::string::npos,
       "the durability pass is told it cannot delete");

    // ...and must NOT leak into the keep rubrics. Appending it to them was measured at
    // 45-84% minted versus 16% asked separately: a generous keep list drags the gist up.
    ok(a.find("FIVE-MINUTE TEST") == std::string::npos, "keep rubric stays free of the gist bar");
    ok(u.find("FIVE-MINUTE TEST") == std::string::npos, "user rubric too");
    ok(a.find("durability is judged in a separate pass") != std::string::npos,
       "assistant keep rubric defers the gist");
    ok(u.find("durability is judged in a separate pass") != std::string::npos,
       "user keep rubric defers the gist");
    // The asymmetry that makes a strict bar safe: an empty gist must never mean drop.
    ok(u.find("NEVER makes it a drop") != std::string::npos,
       "user: undurable never escalates to deletion");
    for (const std::string* p : {&a, &u, &dur}) {
        ok(p->find("Reply ONLY with JSON") != std::string::npos, "every rubric states its contract");
    }
}

void test_verdicts_survive_a_real_reply_and_a_broken_one() {
    std::string good =
        R"({"choices":[{"message":{"content":"{\"verdicts\":[{\"id\":1,\"keep\":true,\"gist\":\"a fact\"},{\"id\":2,\"keep\":false},{\"keep\":true}]}"}}]})";
    std::string err;
    const auto v = cml::parse_verdicts(good, err);
    ok(v.has_value(), "reply parsed");
    ok(v->size() == 2, "the id-less verdict is skipped, not guessed at");
    ok((*v)[0].id == 1 && (*v)[0].keep && (*v)[0].gist == "a fact", "kept row carries its gist");
    ok((*v)[1].id == 2 && !(*v)[1].keep && (*v)[1].gist.empty(), "dropped row needs no gist");

    // An API error must not read as "nothing to do": these rows get retried.
    std::string bad = R"({"error":{"message":"Authentication Fails"}})";
    ok(!cml::parse_verdicts(bad, err).has_value(), "error reply rejected");
    ok(err == "deepseek error: Authentication Fails", "the endpoint's own words");
    std::string empty = "";
    ok(!cml::parse_verdicts(empty, err).has_value(), "empty reply rejected");
    ok(err.rfind("deepseek response unreadable", 0) == 0, "unreadable reply named as such");
    std::string not_json = R"({"choices":[{"message":{"content":"sorry, no JSON today"}}]})";
    ok(!cml::parse_verdicts(not_json, err).has_value(), "prose instead of verdicts rejected");
    ok(err.rfind("verdict json bad", 0) == 0, "and named as such");
}

}  // namespace

void suite_curate() {
    test_forget_match_builds_an_fts_query();
    test_judge_request_is_the_shape_deepseek_expects();
    test_user_rows_are_judged_by_their_own_rubric();
    test_durability_is_judged_apart_from_keeping();
    test_verdicts_survive_a_real_reply_and_a_broken_one();
}

}  // namespace cml_test
