// The hook-facing subcommands: chronic-loop grouping, prompt hints and the recall
// gates, all asserted without a database.

#include <algorithm>
#include <string>
#include <string_view>
#include <vector>

#include "harness.hpp"
#include "hint.hpp"
#include "loops.hpp"
#include "recall.hpp"

namespace cml_test {
namespace {

void test_recurring_asks_surface_and_singletons_do_not() {
    const std::vector<cml::Ask> asks = {
        {"why is the kitty equalize shortcut still not working today", "s1",
         "2026-07-10T10:00:00Z"},
        {"kitty equalize shortcut still not working, why", "s2", "2026-07-12T09:00:00Z"},
        {"the kitty equalize shortcut is still not working", "s3", "2026-07-14T08:00:00Z"},
        {"add a button to the settings page please", "s1", "2026-07-10T11:00:00Z"},
        {"fix it", "s2", "2026-07-11T10:00:00Z"},
    };
    const auto lines = cml::group_recurring(asks, 2, 5);
    ok(lines.size() == 1, "one recurring group; singletons and two-word asks dropped");
    ok(lines[0].find("carried across 3 sessions") != std::string::npos,
       "counted per session, not per row");
    ok(lines[0].find("since 2026-07-10") != std::string::npos, "dated from first sighting");
}

void test_prompt_hints_fire_on_durable_signals_only() {
    ok(cml::classify_prompt("from now on always use tabs in this repo") != nullptr,
       "a standing preference is worth a hint");
    const char* ref = cml::classify_prompt("see https://example.com/spec for the payload format");
    ok(ref != nullptr && std::string_view(ref) == "reference", "a link is reference material");
    ok(cml::classify_prompt("add a button to the settings page") == nullptr,
       "ordinary work stays silent");
}

// The gates that keep recall from becoming wallpaper. Both are the difference between
// a hook that fires meaningfully and one that fires on every prompt and gets ignored.
void test_recall_gates() {
    const auto terms = cml::content_terms("why is the touchpad tap-drag broken again?");
    ok(std::find(terms.begin(), terms.end(), "touchpad") != terms.end(), "content word kept");
    ok(std::find(terms.begin(), terms.end(), "the") == terms.end(), "function word dropped");
    ok(std::find(terms.begin(), terms.end(), "again") == terms.end(), "filler dropped");
    ok(terms.size() == 4, "touchpad + tap + drag + broken, nothing else");

    ok(cml::content_terms("yes").size() < 2, "a bare ack asks the index nothing");
    ok(cml::content_terms("do it").size() < 2, "so does a two-word go-ahead");

    // De-duplicated, so a repeated word cannot fake its way past the two-term floor.
    ok(cml::content_terms("cache cache cache").size() == 1, "terms are a set, not a bag");

    const std::vector<std::string> t = {"touchpad", "tapdrag", "broken"};
    ok(cml::term_overlap("the touchpad tapdrag fix is settled", t) == 2, "two shared words");
    ok(cml::term_overlap("nothing in common with the ask", t) == 0, "unrelated row scores 0");
    ok(cml::term_overlap("TOUCHPAD settings", t) == 1, "match is case-insensitive");
}

}  // namespace

void suite_ports() {
    test_recurring_asks_surface_and_singletons_do_not();
    test_prompt_hints_fire_on_durable_signals_only();
    test_recall_gates();
}

}  // namespace cml_test
