// UserPromptSubmit: the read half of the memory loop, fired on every prompt without the
// model being consulted (see retrieve.cpp for why). This file owns the third gate — a row
// already injected this session is not injected twice — and the contract that whatever
// happens in here, the prompt still goes through.

#include "recall.hpp"

#include <simdjson.h>

#include <cstdio>
#include <iostream>
#include <iterator>
#include <string>

#include "db.hpp"
#include "json.hpp"

namespace cml {
namespace {

constexpr std::size_t kMaxHits = 3;      // a briefing, not a transcript

}  // namespace

void recall() {
    // Whatever happens, the prompt must go through.
    const auto passthrough = []() { std::printf("{\"continue\": true}\n"); };

    std::string payload((std::istreambuf_iterator<char>(std::cin)),
                        std::istreambuf_iterator<char>());
    if (payload.empty()) return passthrough();

    simdjson::dom::parser parser;
    simdjson::dom::element data;
    if (parser.parse(payload).get(data) != simdjson::SUCCESS) return passthrough();

    std::string_view prompt, session;
    if (data["prompt"].get(prompt) != simdjson::SUCCESS || prompt.empty()) return passthrough();
    if (data["session_id"].get(session) != simdjson::SUCCESS) session = {};

    Db db = open_db();
    if (!db) return passthrough();

    // L2 first, and it gets one line of three. A scene says what a whole session
    // concluded, which is the one thing no single row can say; behind the row lanes it
    // would compete for a budget they have already spent, which is exactly how the work
    // lane sat unreachable while being fully wired.
    const auto scene = scene_hit(db, prompt, session);

    // Over-fetch: rows already injected this session are skipped below, and the next
    // fresh one should take the freed slot rather than leaving the briefing short.
    const auto conv = retrieve(db, prompt, session, kMaxHits * 4);

    // The work — commands and their output, in its own table. It gets a RESERVED slot,
    // not an appended one: appending put it after the conversation hits, which had
    // already spent the whole three-line budget, so 29,703 tool rows stayed unreachable
    // exactly as if the lane had never been wired up. A lane that only runs when the
    // other lane comes up short is not wired up.
    RetrieveOpts work_opts;
    work_opts.tools = true;
    const auto work = retrieve(db, prompt, session, 2, work_opts);
    if (conv.empty() && work.empty() && !scene) return passthrough();

    Stmt mark(db, "INSERT OR IGNORE INTO recalled(session, key) VALUES(?1, ?2)");
    const auto fresh = [&](const Hit& h) {
        if (session.empty() || !mark) return true;
        mark.reset();
        mark.bind(1, session).bind(2, h.key);
        return mark.run() && sqlite3_changes(db.raw()) != 0;
    };

    // Three lines, still. A scene spends one of them, it does not add a fourth: this is
    // seen on every prompt, and growing the budget is how a briefing becomes wallpaper.
    // When no scene matches — or it was already injected this session — nothing below
    // changes and the output is byte-identical to what it was (asserted in suite_ports).
    std::vector<std::string> lines;
    if (scene && fresh(*scene)) lines.push_back(scene->line);
    const std::size_t conv_budget = kMaxHits - lines.size() - (work.empty() ? 0 : 1);
    for (const auto& h : conv) {
        if (lines.size() >= conv_budget) break;
        if (fresh(h)) lines.push_back(h.line);
    }
    for (const auto& h : work) {
        if (lines.size() >= kMaxHits) break;
        if (fresh(h)) lines.push_back("(ran) " + h.line);
    }
    if (lines.empty()) return passthrough();

    std::string msg =
        "[cml recall] from your own history, matched on this prompt — it is what was true "
        "when written, so check it still holds before relying on it:";
    for (const auto& l : lines) msg += "\n\xC2\xB7 " + l;  // U+00B7 MIDDLE DOT

    std::string out =
        "{\"continue\": true, \"hookSpecificOutput\": {\"hookEventName\": "
        "\"UserPromptSubmit\", \"additionalContext\": ";
    json::quote_into(out, msg);
    out += "}}";
    std::printf("%s\n", out.c_str());
}

}  // namespace cml
