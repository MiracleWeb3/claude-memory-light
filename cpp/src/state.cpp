#include "state.hpp"

#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <unordered_map>

#include "loops.hpp"
#include "noise.hpp"
#include "paths.hpp"

namespace cml {
namespace {

constexpr std::size_t kLine = 140;      // one row, one line
constexpr std::size_t kPerSection = 3;  // a briefing, not a report

// A memory file leads with YAML frontmatter, so the first 140 characters of one are
// "--- name: ... description:" — the header, not the fact. The description field IS
// the one-line summary the file was written with, so use it and skip the rest.
std::string essence(const std::string& text) {
    if (text.rfind("---", 0) != 0) return text;
    const std::size_t d = text.find("description:");
    if (d == std::string::npos) return text;
    std::size_t b = text.find_first_not_of(" \t\"", d + 12);
    if (b == std::string::npos) return text;
    std::size_t e = text.find('\n', b);
    if (e == std::string::npos) e = text.size();
    while (e > b && (text[e - 1] == '"' || text[e - 1] == ' ')) --e;
    return text.substr(b, e - b);
}

// The learning inbox is a scratch pad the Stop hook appends to, indexed like any other
// markdown under the memory dir. It is raw signal awaiting consolidation, never a
// standing rule, and it would otherwise win a slot every session by being newest.
bool is_scratch(const std::string& text) {
    return text.rfind("# Learning inbox", 0) == 0;
}

// Prefer the curated gist over the raw row: it is the reusable essence in 120 chars,
// which is exactly what a standing brief wants. Falls back to the row itself, clipped.
std::vector<std::string> pick(Db& db, const char* sql, std::string_view project,
                              std::size_t n,
                              const std::unordered_map<std::string, std::string>& gists,
                              bool require_gist) {
    std::vector<std::string> out;
    Stmt s(db, sql);
    if (!s) return out;
    s.bind(1, project).bind(2, static_cast<std::int64_t>(n * (require_gist ? 12 : 1)));
    while (s.step() && out.size() < n) {
        const std::string ts = s.text(0), role = s.text(1), sess = s.text(2);
        const std::string text = s.text(3), head = s.text(4);
        if (is_scratch(text)) continue;
        const auto it = gists.find(stable_key(sess, ts, role, head));
        if (require_gist && it == gists.end()) continue;
        const std::string body = (it != gists.end()) ? it->second : essence(text);
        if (body.size() < 20) continue;
        out.push_back((ts.size() >= 10 ? ts.substr(0, 10) : "?") + " " + squeeze(body, kLine));
    }
    return out;
}

void section(std::string& out, const char* label, const std::vector<std::string>& lines,
             std::size_t budget) {
    if (lines.empty()) return;
    for (const auto& l : lines) {
        if (out.size() + l.size() + 16 > budget) return;
        out += "\n";
        out += label;
        out += " · ";
        out += l;
    }
}

}  // namespace

std::string project_state(Db& db, std::string_view project, std::size_t budget) {
    if (project.empty()) return {};

    std::int64_t sessions = 0;
    std::string last;
    {
        Stmt s(db, "SELECT count(DISTINCT session), max(ts) FROM mem WHERE project=?1");
        if (!s) return {};
        s.bind(1, project);
        if (s.step()) {
            sessions = s.i64(0);
            last = s.text(1);
        }
    }
    if (sessions == 0) return {};  // nothing on file: say nothing rather than a stub

    const auto gists = gist_lookup(db);
    // The `rule` lane used to re-surface role IN ('memory','wiki') here — the newest
    // hand-written memory descriptions, verbatim, every session. Dropped: the harness
    // already injects the curated MEMORY.md and the wiki index natively each session, so
    // this only duplicated them, and a newest-first raw dump of memory descriptions puts
    // whatever was last worked on into every prompt — which is what surfaced this project's
    // context to the input classifier on benign requests. cml's unique standing signal is
    // what MEMORY.md does NOT track: cross-session recurrence (open) and recent outcomes.
    const char* kDurable =
        "SELECT ts, role, session, substr(text,1,400), substr(text,1,64) FROM mem "
        "WHERE role IN ('assistant','user') AND project=?1 ORDER BY ts DESC LIMIT ?2";

    std::string out = "[cml state] " + std::string(project) + " — " +
                      std::to_string(sessions) + " session(s) on file" +
                      (last.size() >= 10 ? ", last " + last.substr(0, 10) : "") +
                      ". Standing context, not an answer to anything:";
    const std::size_t head = out.size();

    {
        // Recurrence is measured across sessions, so it is the one signal that says
        // "this is still not fixed" without anyone having to mark it as such.
        auto loops = loop_lines(db, 45, kPerSection, project);
        for (auto& l : loops) l = squeeze(l, kLine);
        section(out, "open", loops, budget);
    }
    section(out, "done", pick(db, kDurable, project, kPerSection, gists, true), budget);

    if (out.size() == head) return {};  // header only is noise
    return out;
}

int state_cmd(const std::vector<std::string>& args) {
    std::string project;
    std::size_t budget = 1400;
    for (std::size_t i = 0; i < args.size(); ++i) {
        if (args[i] == "--project" && i + 1 < args.size()) {
            project = args[++i];
        } else if (args[i] == "--budget" && i + 1 < args.size()) {
            if (const long v = std::strtol(args[++i].c_str(), nullptr, 10); v > 0)
                budget = static_cast<std::size_t>(v);
        }
    }
    Db db = open_db();
    if (!db) {
        std::fprintf(stderr, "cml: cannot open index\n");
        return 1;
    }
    if (project.empty()) {
        std::error_code ec;
        project = project_label(flatten(std::filesystem::current_path(ec).string()));
    }
    const std::string s = project_state(db, project, budget);
    if (s.empty()) {
        std::printf("no state on file for project '%s'\n", project.c_str());
        return 0;
    }
    std::printf("%s\n", s.c_str());
    return 0;
}

}  // namespace cml
