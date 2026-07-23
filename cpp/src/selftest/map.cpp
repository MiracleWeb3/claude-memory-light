#include <string>

#include "harness.hpp"
#include "map.hpp"

namespace cml_test {
namespace {

void test_wikilinks_are_the_edges_between_notes() {
    const auto found = cml::wikilinks("see [[ Feedback-User-Authority ]] and [[b]] and [[");
    ok(found.size() == 2, "unterminated link ends the scan");
    ok(found[0] == "feedback-user-authority", "trimmed and lowercased");
    ok(found[1] == "b", "second link");
    ok(cml::wikilinks("[[" + std::string(120, 'x') + "]]").empty(), "runaway link ignored");
    ok(cml::wikilinks("[[   ]]").empty(), "blank link ignored");
}

void test_payload_places_every_row_and_never_closes_the_script() {
    cml::MapData d;
    d.proj_names = {"proj"};
    d.sess_list = {{"abcdef0123", 0}};
    d.role_counts["user"] = 1;
    d.msgs.push_back({1, "user", 0, true, 0, "2026-07-21", "a </script> snippet", "abcdef01",
                      "proj"});
    d.msgs.push_back({2, "wiki", 0, false, 0, "2026-07-21", "note", "", "wiki"});
    d.note_ids["target"] = "m:2";
    d.pending_wikilinks.push_back({"m:1", {"target", "missing"}});

    const auto p = cml::build_payload(d);
    ok(p.nodes == 5, "center + project + session + 2 messages");
    ok(p.links == 5, "3 spine/leaf pairs plus the one resolvable wikilink");
    ok(p.code == 0, "no code graph here");
    ok(p.json.find("</script>") == std::string::npos, "an inline <script> stays open");
    ok(p.json.find("<\\/script>") != std::string::npos, "...because the slash is escaped");
    ok(p.json.find("\"kind\":\"wikilink\",\"source\":\"m:1\",\"target\":\"m:2\"") !=
           std::string::npos,
       "wikilink resolved to the note's node");
    ok(p.json.find("\"stats\":{\"db_mb\":\"?\",\"projects\":1,\"roles\":{\"user\":1},\"rows\":2,"
                   "\"sessions\":1}") != std::string::npos,
       "stats block");
}

}  // namespace

void suite_map() {
    test_wikilinks_are_the_edges_between_notes();
    test_payload_places_every_row_and_never_closes_the_script();
}

}  // namespace cml_test
