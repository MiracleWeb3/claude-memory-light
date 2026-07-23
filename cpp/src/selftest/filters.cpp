// Every case here is a real line the old filters got wrong on 2026-07-21 — this
// file is the regression net for that day.

#include <filesystem>
#include <fstream>
#include <string>

#include "harness.hpp"
#include "noise.hpp"
#include "paths.hpp"
#include "work.hpp"

namespace cml_test {
namespace {

void test_envelopes_and_acks_are_noise() {
    // Every one of these was a real row in the 1794-row index.
    const char* junk[] = {
        "<command-message>ponytail</command-message>\n<command-name>/ponytail</command-name>",
        "[Request interrupted by user]",
        "[Request interrupted by user for tool use]",
        "<task-notification>\n<task-id>be31bb3c0</task-id>",
        "This session is being continued from a previous conversation",
        // Both of these were dropped when porting the prefix list from Rust and
        // survived every unit test; only a differential index caught them.
        "Continue from where you left off.",
        "(Re-invocation of /superpowers:brainstorming — the skill instructions were",
        "Another Claude session sent a message: <teammate-message id=\"x\">",
        "[Image: source: /home/definitive/.claude/image-cache/8c39e558/1.png]",
        "Stop hook feedback: pantheon verification gate",
        "PreToolUse:Bash hook additional context: use parallel execution",
        "/compact", "ok", "Yes please!", "continue please", "   ",
    };
    for (const char* j : junk) ok(cml::is_noise(j), j);

    // ...and these must survive: real questions, even very short ones.
    const char* real[] = {
        "whats the super?",
        "do we have double tap on tap after right mouse button tap?",
        "no it doesnt work nothing comes at all",
        "install tox please to my system",
        "[Image #1] no it doesnt work, i did everything as youve said",
    };
    for (const char* r : real) ok(!cml::is_noise(r), r);
}

void test_correction_flag_ignores_ordinary_requests() {
    const char* plain[] = {
        "can you tell me about shodan, what is it and why people are so hyped about it?",
        "so that stuff from pantheon is actually super useful, or no?",
        "can you do that fix rq?",
        "install tox please to my system",
        "open it up please",
    };
    for (const char* p : plain) ok(!cml::looks_like_correction(p), p);

    const char* corrections[] = {
        "no it doesnt work nothing comes at all",
        "why did you do that instead of what i asked",
        "thats not what I meant, revert it",
        "still no answer",
    };
    for (const char* c : corrections) ok(cml::looks_like_correction(c), c);
}

void test_paths_match_the_rust_binary() {
    ok(cml::flatten("/home/definitive") == "-home-definitive", "flatten");
    ok(cml::project_label(cml::flat_home()) == "home", "home label");
    ok(cml::project_label(cml::flat_home() + "-dev-proj") == "dev-proj", "sub label");
    ok(cml::project_label("") == "misc", "empty label falls back to misc");
    ok(cml::iso_minute(1784650211) == "2026-07-21 16:10", "iso_minute");
}

std::filesystem::path write_temp(const char* name, const std::string& body) {
    const auto p = std::filesystem::temp_directory_path() / name;
    std::ofstream(p) << body;
    return p;
}

void test_digest_records_work_not_just_the_ask() {
    const std::string body =
        R"({"type":"user","message":{"role":"user","content":"fix the telegram inbound"}})"
        "\n"
        R"({"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/home/u/dev/proj/src/server.ts"}}]}})"
        "\n"
        R"({"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo test --release\nmake"}}]}})"
        "\n"
        R"({"type":"user","message":{"role":"user","content":[{"type":"tool_result","is_error":true}]}})"
        "\n"
        R"({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Root cause was the missing --channels flag."}]}})";

    const auto p = write_temp("cml_cpp_digest.jsonl", body);
    const auto d = cml::digest(p.string(), [](const std::string&) { return true; });

    ok(d.ask == "fix the telegram inbound", "ask");
    ok(d.files.size() == 1 && d.files[0] == "server.ts", "path stripped to basename");
    ok(d.commands.size() == 1 && d.commands[0] == "cargo test --release", "first command only");
    ok(d.failures == 1, "tool_result errors counted");
    ok(d.outcome.find("--channels") != std::string::npos, "outcome");
    ok(d.has_work(), "has_work");

    const auto detail = d.detail_lines();
    ok(detail.size() == 2, "two detail lines");
    ok(detail[0].find("files: server.ts") != std::string::npos, "files line");
    ok(detail[0].find("1 failed") != std::string::npos, "failure count");
    ok(detail[1].rfind("    did:", 0) == 0, "outcome line");
}

void test_a_new_user_turn_resets_the_digest() {
    const std::string body =
        R"({"type":"user","message":{"role":"user","content":"first ask"}})" "\n"
        R"({"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Write","input":{"file_path":"/tmp/old.cpp"}}]}})" "\n"
        R"({"type":"user","message":{"role":"user","content":"second ask"}})" "\n"
        R"({"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Write","input":{"file_path":"/tmp/new.cpp"}}]}})";

    const auto p = write_temp("cml_cpp_reset.jsonl", body);
    const auto d = cml::digest(p.string(), [](const std::string&) { return true; });
    ok(d.ask == "second ask", "last turn wins");
    ok(d.files.size() == 1 && d.files[0] == "new.cpp", "earlier turns must not leak in");
}

void test_conversation_only_turn_has_no_work() {
    const std::string body =
        R"({"type":"user","message":{"role":"user","content":"what is shodan"}})" "\n"
        R"({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"A search engine for devices."}]}})";

    const auto p = write_temp("cml_cpp_convo.jsonl", body);
    const auto d = cml::digest(p.string(), [](const std::string&) { return true; });
    ok(!d.has_work(), "pure Q&A falls back to a plain one-liner");
}

void test_squeeze_collapses_and_clips() {
    ok(cml::squeeze("  a   b \n c ", 100) == "a b c", "whitespace collapsed");
    ok(cml::squeeze("abcdef", 3).rfind("abc", 0) == 0, "clipped at max");
}

}  // namespace

void suite_filters() {
    test_envelopes_and_acks_are_noise();
    test_correction_flag_ignores_ordinary_requests();
    test_paths_match_the_rust_binary();
    test_digest_records_work_not_just_the_ask();
    test_a_new_user_turn_resets_the_digest();
    test_conversation_only_turn_has_no_work();
    test_squeeze_collapses_and_clips();
}

}  // namespace cml_test
