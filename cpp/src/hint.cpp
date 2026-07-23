#include "hint.hpp"

#include <simdjson.h>

#include <cctype>
#include <cstdio>
#include <initializer_list>
#include <iostream>
#include <string>

#include "db.hpp"
#include "json.hpp"
#include "noise.hpp"

namespace cml {
namespace {

struct Cat {
    const char* name;
    const char* message;
    std::initializer_list<const char*> phrases;  // matched space-padded, lowercased
};

// Order is priority: a correction outranks the preference wording inside it.
constexpr Cat kCats[] = {
    {"correction",
     "[cml] reads like a correction — once resolved, capture it to memory so it sticks "
     "(learning loop).",
     {}},
    {"preference",
     "[cml] reads like a durable preference — consider capturing it so future sessions "
     "inherit it.",
     {" from now on ", " always ", " never ", " prefer", " by default ", " going forward "}},
    {"decision",
     "[cml] reads like a decision — wiki material once it's concluded.",
     {" we will ", " let's go with ", " lets go with ", " decided to ", " we choose "}},
    {"method",
     "[cml] reads like a handed method — try exactly that first; capture it if it works.",
     {" do it like ", " the fix is ", " try this ", " instead use ", " use this approach "}},
    {"reference",
     "[cml] contains a link — worth a wiki/reference note if it should be findable later.",
     {"http://", "https://"}},
};

}  // namespace

const char* classify_prompt(std::string_view prompt) {
    if (looks_like_correction(prompt)) return kCats[0].name;

    std::string lowered;
    lowered.reserve(prompt.size() + 2);
    lowered.push_back(' ');
    for (const char c : prompt)
        lowered.push_back(static_cast<char>(std::tolower(static_cast<unsigned char>(c))));
    lowered.push_back(' ');

    for (const Cat& cat : kCats)
        for (const char* p : cat.phrases)
            if (lowered.find(p) != std::string::npos) return cat.name;
    return nullptr;
}

void hint() {
    const auto passthrough = []() { std::printf("{\"continue\": true}\n"); };

    std::string payload((std::istreambuf_iterator<char>(std::cin)),
                        std::istreambuf_iterator<char>());
    if (payload.empty()) return passthrough();

    simdjson::dom::parser parser;
    simdjson::dom::element data;
    if (parser.parse(payload).get(data) != simdjson::SUCCESS) return passthrough();

    std::string_view prompt, session;
    if (data["prompt"].get(prompt) != simdjson::SUCCESS || prompt.empty()) return passthrough();
    // No session id means no dedupe; stay silent rather than hint on every prompt.
    if (data["session_id"].get(session) != simdjson::SUCCESS || session.empty())
        return passthrough();

    const char* cat = classify_prompt(prompt);
    if (!cat) return passthrough();

    Db db = open_db();
    if (!db) return passthrough();
    Stmt ins(db, "INSERT OR IGNORE INTO hints(session, category) VALUES(?1, ?2)");
    if (!ins) return passthrough();
    ins.bind(1, session).bind(2, cat);
    if (!ins.run() || sqlite3_changes(db.raw()) == 0) return passthrough();  // already hinted

    const Cat* found = nullptr;
    for (const Cat& c : kCats)
        if (c.name == std::string_view(cat)) found = &c;
    if (!found) return passthrough();

    std::string out =
        "{\"continue\": true, \"hookSpecificOutput\": {\"hookEventName\": "
        "\"UserPromptSubmit\", \"additionalContext\": ";
    json::quote_into(out, found->message);
    out += "}}";
    std::printf("%s\n", out.c_str());
}

}  // namespace cml
