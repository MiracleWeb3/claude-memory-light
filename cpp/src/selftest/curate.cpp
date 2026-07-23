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
    test_verdicts_survive_a_real_reply_and_a_broken_one();
}

}  // namespace cml_test
