#include "loops.hpp"

#include <algorithm>
#include <cctype>
#include <cstdio>
#include <cstdlib>
#include <unordered_set>

#include "db.hpp"
#include "noise.hpp"
#include "paths.hpp"

namespace cml {
namespace {

// Lowercased alphanumeric token set — the unit recurrence is judged on.
std::unordered_set<std::string> tokens_of(std::string_view text) {
    std::unordered_set<std::string> out;
    std::string cur;
    const auto flush = [&] {
        if (cur.size() >= 2) out.insert(cur);
        cur.clear();
    };
    for (const char c : text) {
        const auto u = static_cast<unsigned char>(c);
        if (std::isalnum(u))
            cur.push_back(static_cast<char>(std::tolower(u)));
        else
            flush();
    }
    flush();
    return out;
}

double jaccard(const std::unordered_set<std::string>& a,
               const std::unordered_set<std::string>& b) {
    if (a.empty() || b.empty()) return 0.0;
    std::size_t inter = 0;
    for (const auto& t : a)
        if (b.count(t)) ++inter;
    return static_cast<double>(inter) / static_cast<double>(a.size() + b.size() - inter);
}

struct Group {
    std::unordered_set<std::string> tokens;  // the founding ask's tokens
    std::unordered_set<std::string> sessions;
    std::string first_text;
    std::string first_ts;
};

}  // namespace

std::vector<std::string> group_recurring(const std::vector<Ask>& asks,
                                         std::size_t min_sessions, std::size_t limit) {
    // ponytail: O(n^2) greedy first-fit against each group's founding ask; fine for a
    // windowed month of user rows, revisit with shingle hashing past ~10k asks.
    std::vector<Group> groups;
    for (const auto& a : asks) {
        auto toks = tokens_of(a.text);
        if (toks.size() < 5) continue;  // "fix it" is not a loop, it's a Tuesday
        Group* home = nullptr;
        for (auto& g : groups) {
            if (jaccard(toks, g.tokens) >= 0.6) {
                home = &g;
                break;
            }
        }
        if (!home) {
            groups.push_back({std::move(toks), {}, a.text, a.ts});
            home = &groups.back();
        }
        home->sessions.insert(a.session);
        if (home->first_ts.empty() || (!a.ts.empty() && a.ts < home->first_ts)) {
            home->first_ts = a.ts;
            home->first_text = a.text;
        }
    }

    std::stable_sort(groups.begin(), groups.end(), [](const Group& x, const Group& y) {
        if (x.sessions.size() != y.sessions.size()) return x.sessions.size() > y.sessions.size();
        return x.first_ts < y.first_ts;
    });

    std::vector<std::string> out;
    for (const auto& g : groups) {
        if (out.size() >= limit) break;
        if (g.sessions.size() < min_sessions) continue;
        out.push_back("carried across " + std::to_string(g.sessions.size()) +
                      " sessions since " + g.first_ts.substr(0, 10) + ": " +
                      squeeze(g.first_text, 110));
    }
    return out;
}

std::vector<std::string> loop_lines(Db& db, int days, std::size_t limit) {
    const std::string cutoff =
        iso_minute(now_secs() - static_cast<std::int64_t>(days) * 86400).substr(0, 10);
    std::vector<Ask> asks;
    Stmt s(db, "SELECT text, session, ts FROM mem WHERE role='user' AND ts >= ?1 ORDER BY ts");
    if (!s) return {};
    s.bind(1, cutoff);
    while (s.step()) {
        std::string text = s.text(0);
        if (is_noise(text)) continue;
        // A markdown heading start is a pasted doc or skill dump, not an ask.
        if (text.rfind("# ", 0) == 0) continue;
        asks.push_back({std::move(text), s.text(1), s.text(2)});
    }
    return group_recurring(asks, 2, limit);
}

int loops(const std::vector<std::string>& args) {
    int days = 30;
    std::size_t limit = 10;
    for (std::size_t i = 0; i < args.size(); ++i) {
        const auto next = [&]() -> long {
            return (i + 1 < args.size()) ? std::strtol(args[++i].c_str(), nullptr, 10) : 0;
        };
        if (args[i] == "--days") {
            if (const long v = next(); v > 0) days = static_cast<int>(v);
        } else if (args[i] == "--limit") {
            if (const long v = next(); v > 0) limit = static_cast<std::size_t>(v);
        }
    }

    Db db = open_db();
    if (!db) {
        std::fprintf(stderr, "cml: cannot open index\n");
        return 1;
    }
    const auto lines = loop_lines(db, days, limit);
    if (lines.empty()) {
        std::printf("no chronic loops in the last %d days\n", days);
        return 0;
    }
    for (const auto& l : lines) std::printf("%s\n", l.c_str());
    return 0;
}

}  // namespace cml
