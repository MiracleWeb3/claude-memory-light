// What `cml index` does, in order — and how long it is allowed to take.
//
// The scanning itself lives in indexer/: transcripts.cpp has the per-entry judgement,
// notes.cpp is whole-file. This file is only the sequence and its budget, because the
// sequence is the part that runs on every Stop hook and therefore has to be bounded.
#include "indexer.hpp"

#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <string>

#include "budget.hpp"
#include "curate.hpp"
#include "db.hpp"
#include "indexer/curator.hpp"
#include "indexer/files.hpp"
#include "paths.hpp"
#include "vec.hpp"

namespace fs = std::filesystem;

namespace cml {

int index_all(bool force) {
    Db db = open_db();
    if (!db) {
        std::fprintf(stderr, "cml: cannot open index\n");
        return 1;
    }

    const Budget budget(index_budget_ms());
    const idx::Counts t = idx::index_transcripts(db, force, budget);
    idx::Counts m;
    std::error_code ec;
    const fs::path projects = home() / ".claude/projects";
    if (fs::is_directory(projects, ec)) {
        for (const auto& pdir : fs::directory_iterator(projects, ec)) {
            if (!pdir.is_directory()) continue;
            const std::string project = project_label(pdir.path().filename().string());
            const idx::Counts c =
                idx::index_md_dir(db, pdir.path() / "memory", "memory", project, force);
            m.files += c.files;
            m.rows += c.rows;
        }
    }
    const idx::Counts w = idx::index_md_dir(db, data_dir() / "wiki", "wiki", "wiki", force);

    // Incremental semantic pass. `cml index` runs from the Stop hook every turn, so
    // without this the vector table would fall permanently behind the FTS index and
    // hybrid search would quietly lose recall on everything recent. No-ops unless
    // `cml embed` has already created the table.
    const std::size_t embedded = budget.spent() ? 0 : embed_new(db);

    // Curation is started, not waited for. It costs ~20s a row on a reasoning-tier
    // curator (102s for five, measured 2026-08-02), so there is no batch size that fits
    // a Stop hook — the previous "call it with whatever budget is left" could only ever
    // time out and look like a broken key. See indexer/curator.hpp.
    std::string curated;
    // No backlog check: a curator with nothing to judge exits in 13ms, and duplicating
    // distill's candidate query here would only drift from it.
    if (llm_key() && idx::spawn_curator()) {
        curated = ", curating in the background";
    }

    std::printf(
        "indexed %zu file(s), %zu row(s), %zu embedded%s  [transcripts %zu/%zu, memory %zu/%zu, wiki %zu/%zu]\n",
        t.files + m.files + w.files, t.rows + m.rows + w.rows, embedded, curated.c_str(),
        t.files, t.rows, m.files, m.rows, w.files, w.rows);
    return 0;
}

}  // namespace cml
