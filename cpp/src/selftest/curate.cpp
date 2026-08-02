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

// Scenes are L2: a whole session in, one summary out. Nothing here judges or drops.
void test_scene_request_and_parse() {
    const std::vector<std::pair<std::int64_t, std::string>> rows = {
        {0, "user: daita keeps dropping on patro1a\nassistant: path MTU is 1280, "
            "constant-size padding cannot fit\nuser: turning daita off fixed it"}};
    const std::string req = cml::build_judge_request("m", rows, cml::Rubric::Scene);
    ok(req.find("outcome") != std::string::npos, "the rubric asks for an outcome");
    ok(req.find("daita") != std::string::npos, "the digest is in the body");

    // {"scenes":[…]}, an object — never a bare array. build_judge_request hard-codes
    // response_format json_object for every rubric, and every key lookup on an array
    // returns INCORRECT_TYPE, which is the same value as an empty batch.
    std::string reply = R"({"choices":[{"message":{"content":
      "{\"scenes\":[{\"id\":0,\"title\":\"DAITA vs 1280 path MTU\",\"summary\":\"DAITA padding cannot fit under a 1280-byte path MTU on patro1a; disabling DAITA fixed it, verified A/B on the real network.\",\"outcome\":\"solved\"}]}"}}]})";
    std::string err;
    const auto scenes = cml::parse_scenes(reply, err);
    ok(scenes.has_value(), "scenes parse");
    ok(scenes->size() == 1 && (*scenes)[0].outcome == "solved", "outcome read");
    ok((*scenes)[0].title.rfind("DAITA vs", 0) == 0, "title read");

    // The shape that costs real money. Read as an empty batch, a whole paid run reports
    // success with nothing written and no diagnostic anywhere; it must be an error.
    std::string bare =
        R"({"choices":[{"message":{"content":"[{\"id\":0,\"summary\":\"s\",\"outcome\":\"solved\"}]"}}]})";
    ok(!cml::parse_scenes(bare, err).has_value(), "a bare array is refused, not read as zero");
    ok(err.find("scenes") != std::string::npos, "and the error names what was missing");

    // A summary-less row is skipped, not written hollow: `scene` has no PRIMARY KEY and
    // build_scenes skips sessions already in it, so an empty row would be permanent.
    std::string thin = R"({"choices":[{"message":{"content":"{\"scenes\":[{\"id\":1,\"title\":\"t\"}]}"}}]})";
    const auto none = cml::parse_scenes(thin, err);
    ok(none.has_value() && none->empty(), "a scene with no summary is skipped");
}

// The rubric earns its place only by refusing the activity log — the summary a model
// writes by default, fluent and worth nothing months later.
void test_scene_rubric_forbids_activity_logs() {
    const std::string p{cml::curator_prompt(cml::Rubric::Scene)};
    ok(p.find("worked on") != std::string::npos,
       "the prompt names the failure mode it must refuse");
    // curator_prompt used to be an if plus a ternary: a rubric with no prompt of its own
    // silently got this one. That is a wrong system prompt on a paid call.
    ok(p != std::string(cml::curator_prompt(cml::Rubric::Assistant)),
       "Scene reaches its own prompt, not the assistant's by fallthrough");
    for (const char* word : {"solved", "abandoned", "ongoing"}) {
        ok(p.find(word) != std::string::npos, "the outcome vocabulary is closed and stated");
    }
    ok(p.find("\"scenes\"") != std::string::npos, "and the object shape is spelled out");
}

}  // namespace

void suite_curate() {
    test_forget_match_builds_an_fts_query();
    test_judge_request_is_the_shape_deepseek_expects();
    test_user_rows_are_judged_by_their_own_rubric();
    test_durability_is_judged_apart_from_keeping();
    test_verdicts_survive_a_real_reply_and_a_broken_one();
    test_scene_request_and_parse();
    test_scene_rubric_forbids_activity_logs();
}

}  // namespace cml_test
