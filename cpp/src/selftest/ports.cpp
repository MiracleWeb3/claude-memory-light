// The hook-facing subcommands: chronic-loop grouping, prompt hints and the recall
// gates, all asserted without a database.

#include <algorithm>
#include <filesystem>
#include <string>
#include <string_view>
#include <vector>

#include "db.hpp"
#include "harness.hpp"
#include "hint.hpp"
#include "loops.hpp"
#include "recall.hpp"

namespace cml_test {
namespace {

namespace fs = std::filesystem;

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

// The L2 line, driven through the real binary. The claim being asserted is a byte-for-byte
// one — adding a lane to a hook that fires on every prompt must change nothing for the
// prompts it does not answer — and only the emitted JSON can carry that claim. A unit call
// to scene_hit() returning nullopt proves the lane is quiet, not that the briefing beside
// it is untouched.
fs::path sandbox() { return fs::temp_directory_path() / "cml-selftest-recall"; }

std::string run(const std::string& prompt) {
    return drive("recall", sandbox(),
                 "{\"prompt\":\"" + prompt + "\",\"session_id\":\"live-session\"}");
}

// `recalled` is cleared with the scene: per-session dedupe would otherwise make the second
// run of the same prompt silent, and two empty briefings compare equal for the wrong reason.
void reseed(bool with_scene) {
    cml::Db db = cml::open_db();
    ok(db.exec("DELETE FROM scene; DELETE FROM recalled;"), "table reset");
    if (!with_scene) return;
    ok(db.exec("INSERT INTO scene(title,summary,outcome,session,project,ts_start,ts_end,"
               "n_rows) VALUES('DAITA vs 1280 path MTU','constant-size padding cannot fit "
               "under a 1280-byte path MTU on patro1a; turning daita off fixed it, verified "
               "A/B on the real network.','solved','older-session','p','2026-07-31',"
               "'2026-07-31',42)"),
       "scene seeded");
}

void test_a_prompt_matching_no_scene_is_unchanged() {
    fs::remove_all(sandbox());
    fs::create_directories(sandbox());
    setenv("CML_HOME", sandbox().c_str(), 1);
    {
        cml::Db db = cml::open_db();
        ok(static_cast<bool>(db), "sandbox index opens");
        cml::Stmt ins(db, "INSERT INTO mem(text,asks,role,project,session,ts,file) "
                          "VALUES(?1,'','assistant','p','older-session','2026-07-01','f')");
        ins.bind(1, "the kitty equalize shortcut needs a remap in kitty.conf");
        ok(ins.run(), "one conversation row for the briefing to find");
    }

    reseed(true);
    const std::string with = run("why is the kitty equalize shortcut broken");
    if (with.empty()) {  // selftest built without `cml` beside it
        unsetenv("CML_HOME");
        fs::remove_all(sandbox());
        return;
    }
    reseed(false);
    const std::string without = run("why is the kitty equalize shortcut broken");
    ok(with == without, "a prompt no scene answers emits byte-identical output");
    ok(with.find("kitty") != std::string::npos,
       "and it is the real briefing being compared, not two passthroughs");

    // The other half of the claim: a lane that is never allowed to fire is not a lane.
    reseed(true);
    const std::string hit = run("daita dropping on patro1a with the 1280 mtu path");
    ok(hit.find("DAITA vs 1280 path MTU (solved, 2026-07-31)") != std::string::npos,
       "a matching scene is injected as title (outcome, date)");
    // Three lines, not four: the middle dot is U+00B7, one per injected line.
    std::size_t dots = 0;
    for (std::size_t i = hit.find("\\u00b7"); i != std::string::npos;
         i = hit.find("\\u00b7", i + 1))
        ++dots;
    for (std::size_t i = hit.find("\xC2\xB7"); i != std::string::npos;
         i = hit.find("\xC2\xB7", i + 1))
        ++dots;
    ok(dots >= 1 && dots <= 3, "the briefing budget is still three lines");

    unsetenv("CML_HOME");
    fs::remove_all(sandbox());
}

}  // namespace

void suite_ports() {
    test_recurring_asks_surface_and_singletons_do_not();
    test_prompt_hints_fire_on_durable_signals_only();
    test_recall_gates();
    test_a_prompt_matching_no_scene_is_unchanged();
}

}  // namespace cml_test
