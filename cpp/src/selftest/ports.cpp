// 2.5.0 ports: chronic-loop grouping and prompt hints, asserted without a database.

#include <string>
#include <string_view>
#include <vector>

#include "harness.hpp"
#include "hint.hpp"
#include "loops.hpp"

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

}  // namespace

void suite_ports() {
    test_recurring_asks_surface_and_singletons_do_not();
    test_prompt_hints_fire_on_durable_signals_only();
}

}  // namespace cml_test
