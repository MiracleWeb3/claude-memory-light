// stats and doctor: what a human types to see whether this thing is actually working.
// The SessionStart briefing lives in nudge.cpp.

#include <simdjson.h>

#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <string>

#include <vector>

#include "curate.hpp"
#include "db.hpp"
#include "embed.hpp"
#include "embedder.hpp"
#include "indexer/curator.hpp"
#include "loops.hpp"
#include "noise.hpp"
#include "paths.hpp"
#include "search.hpp"

namespace fs = std::filesystem;

namespace cml {
namespace {

const char* mark(bool present) { return present ? "ok" : "MISSING"; }

// `which`, without spawning a shell.
std::string which(const char* exe) {
    const char* path = std::getenv("PATH");
    if (!path) return {};
    std::string p(path), item;
    std::size_t start = 0;
    while (start <= p.size()) {
        const std::size_t end = p.find(':', start);
        item = p.substr(start, end - start);
        if (!item.empty()) {
            std::error_code ec;
            const fs::path cand = fs::path(item) / exe;
            if (fs::is_regular_file(cand, ec)) return cand.string();
        }
        if (end == std::string::npos) break;
        start = end + 1;
    }
    return {};
}


}  // namespace

int stats() {
    Db db = open_db();
    if (!db) {
        std::fprintf(stderr, "cml: cannot open index\n");
        return 1;
    }
    const std::int64_t msgs = db.scalar("SELECT count(*) FROM mem");
    const std::int64_t sessions = db.scalar(
        "SELECT count(DISTINCT session) FROM mem WHERE role IN ('user','assistant','summary')");
    const std::int64_t files = db.scalar("SELECT count(*) FROM files");

    std::string by_role;
    {
        Stmt s(db, "SELECT role, count(*) FROM mem GROUP BY role ORDER BY 2 DESC");
        while (s.step()) {
            if (!by_role.empty()) by_role += ", ";
            by_role += s.text(0) + "=" + std::to_string(s.i64(1));
        }
    }

    const fs::path dbp = data_dir() / "index.db";
    std::error_code ec;
    const double mb = static_cast<double>(fs::file_size(dbp, ec)) / 1e6;

    // Rows are searchable substrate; knowledge is the durable subset the map plots. The
    // two were read as one number ("1152 brain points") when the second was a fraction of
    // the first, so both are printed and named.
    const std::int64_t knowledge =
        db.scalar("SELECT count(*) FROM distilled WHERE gist != ''") +
        db.scalar("SELECT count(*) FROM mem WHERE role IN ('memory','wiki')");
    // L2, counted separately: a scene is neither a row nor a gist, and it is the one
    // number that says whether `cml distill --scenes` has ever run.
    const std::int64_t scenes = db.scalar("SELECT count(*) FROM scene");

    std::printf(
        "%lld rows (%s) | %lld knowledge | %lld scenes | %lld sessions | %lld files | "
        "%.1f MB at %s\n",
        static_cast<long long>(msgs), by_role.c_str(), static_cast<long long>(knowledge),
        static_cast<long long>(scenes), static_cast<long long>(sessions),
        static_cast<long long>(files), ec ? 0.0 : mb, dbp.string().c_str());
    return 0;
}

int doctor() {
    std::error_code ec;
    const fs::path projects = home() / ".claude/projects";
    const fs::path data = data_dir();
    const fs::path dbp = data / "index.db";

    // First line on purpose: every other number here describes whichever binary is
    // speaking, and the hooks run an installed copy that nothing used to keep current.
    std::printf("binary          : cml %s at %s\n", CML_VERSION, idx::self_exe().c_str());
    std::printf("transcripts dir : %s (%s)\n", projects.string().c_str(),
                mark(fs::is_directory(projects, ec)));
    std::printf("data dir        : %s (%s)\n", data.string().c_str(),
                mark(fs::is_directory(data, ec)));
    std::printf("index db        : %s (%s)\n", dbp.string().c_str(),
                mark(fs::is_regular_file(dbp, ec)));

    const std::string g = which("graphify");
    std::printf("graphify        : %s (optional structural-memory companion)\n",
                g.empty() ? "not installed" : g.c_str());

    {
        Db db = open_db();
        if (db && vec_table_exists(db)) {
            const std::int64_t v = db.scalar("SELECT count(*) FROM vec_mem");
            const std::int64_t m = db.scalar("SELECT count(*) FROM mem");
            // Two separate facts, not one guess. Nothing records which model wrote the
            // stored vectors, so naming one here was a lie as soon as a second backend
            // existed: it printed potion-base-8M over vectors the transformer had made.
            // The width is what the vectors actually are; the id is what would be used
            // now. When they disagree, `cml embed --all` is the answer.
            std::printf("semantic        : %lld/%lld rows embedded; backend now: %s\n",
                        static_cast<long long>(v), static_cast<long long>(m),
                        embedder_id().c_str());
        } else {
            std::printf("semantic        : off — run `cml embed` once to enable hybrid search\n");
        }
    }

    // doc2query coverage. Same silent-failure shape the recall counter exists for: the
    // expansions can be at zero for every row and nothing else would ever say so.
    {
        Db db = open_db();
        const std::int64_t withasks =
            db ? db.scalar("SELECT count(*) FROM mem WHERE asks != ''") : 0;
        const std::int64_t rows = db ? db.scalar("SELECT count(*) FROM mem") : 0;
        if (withasks)
            std::printf("expansions      : %lld/%lld rows carry search phrasings\n",
                        static_cast<long long>(withasks), static_cast<long long>(rows));
        else
            std::printf("expansions      : none — `cml distill` writes them, needs a curator key\n");
    }

    // Curation status. Omitting this is how I convinced myself curation was off when
    // a key had been sitting in llm.key since July — doctor is the only place that
    // reports it, so a missing line reads as a missing feature.
    {
        Db db = open_db();
        // `distilled` records a verdict either way — a drop demotes rather than deletes
        // (see distill.cpp) — so this count is rows JUDGED, not rows kept, and calling it
        // "judged kept" flattered it by every dropped row. The gist count is the one that
        // says how much of the judging produced something. `forgotten` is a different
        // mechanism entirely: only `cml forget` writes it, so a curator run never moves it.
        const std::int64_t judged = db ? db.scalar("SELECT count(*) FROM distilled") : 0;
        const std::int64_t gists =
            db ? db.scalar("SELECT count(*) FROM distilled WHERE gist != ''") : 0;
        const std::int64_t forgot = db ? db.scalar("SELECT count(*) FROM forgotten") : 0;
        if (llm_key()) {
            const auto [url, model] = llm_conf();
            // host from "https://host/path"
            std::string host = url;
            const std::size_t s = host.find("//");
            if (s != std::string::npos) host = host.substr(s + 2);
            const std::size_t e = host.find('/');
            if (e != std::string::npos) host = host.substr(0, e);
            std::printf(
                "curation        : on (%s @ %s) — %lld rows judged, %lld carry a gist, "
                "%lld hand-forgotten\n",
                model.c_str(), host.empty() ? "?" : host.c_str(),
                static_cast<long long>(judged), static_cast<long long>(gists),
                static_cast<long long>(forgot));
            // Curation is detached now, so "on" only means a key exists — it says nothing
            // about whether the runs work. This line is what the last run actually did.
            std::string last = idx::curator_last_line();
            if (last.size() > 160) last = last.substr(0, 157) + "...";
            if (!last.empty()) std::printf("curator run     : %s\n", last.c_str());
        } else {
            std::printf("curation        : off — put a key in ~/.claude/claude-memory-light/"
                        "llm.key (any OpenAI-compatible provider; CML_LLM_URL / CML_LLM_MODEL "
                        "to configure)\n");
        }
    }
    // Retrieval is the half that fails silently. An index can be perfect, embedded and
    // curated while nothing ever reads it: `cml search` ran in 2% of sessions before
    // recall was hooked, and nobody noticed for months because no number reported it.
    // Worse, a hook can be written, shipped and documented without ever being installed
    // — the plugin on the machine this was built on sat at 1.4.0 for eleven days while
    // 2.5.0's hint was in the README. This line is that number. Zero means check the
    // hook, not the code.
    // No ratio here on purpose: `mem` counts sessions already on disk, `recalled` counts
    // sessions the hook has run in since it was installed. Dividing one by the other
    // reads as a percentage and is not one.
    {
        Db db = open_db();
        const std::int64_t sess = db ? db.scalar("SELECT count(DISTINCT session) FROM recalled") : 0;
        const std::int64_t rows = db ? db.scalar("SELECT count(*) FROM recalled") : 0;
        if (sess)
            std::printf("recall          : fired in %lld sessions, %lld memories injected\n",
                        static_cast<long long>(sess), static_cast<long long>(rows));
        else
            std::printf("recall          : never fired — is the UserPromptSubmit hook "
                        "installed? (`/plugin update claude-memory-light`)\n");
    }
    std::printf("hint: keep transcripts forever with \"cleanupPeriodDays\": 3650 in "
                "~/.claude/settings.json\n");

    if (fs::is_regular_file(dbp, ec)) return stats();
    std::printf("run `cml index` to build the index\n");
    return 0;
}

}  // namespace cml
