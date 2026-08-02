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
#include <tuple>

#include "budget.hpp"
#include "curate.hpp"
#include "curatorprompts.hpp"
#include "db.hpp"
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

    // Automatic curation: judge new rows when a curator key is configured. Each call
    // reaches the network, so it is gated on the budget and told how many seconds are
    // left — see budget.hpp for what happens when that is not done.
    //
    // And a curator that is not answering must not tax every turn. Measured 2026-08-02:
    // with the endpoint reachable but the call not landing, four consecutive index runs
    // each burned the whole 4s budget and curated 0 rows. Bounded waste is still waste
    // when it repeats every Stop hook, so a run that spends real time and curates nothing
    // stands the curator down for a while; the backlog keeps until it is back, or until
    // `cml distill` is run by hand.
    std::string curated;
    if (const auto key = llm_key(); key && !budget.spent() && !curator_backoff_active()) {
        const int left_before = budget.seconds_left();  // a plain snapshot: copying a
                                                        // Budget shares its end time, so
                                                        // the delta would always be 0.
        setenv("CML_HTTP_TIMEOUT", std::to_string(budget.seconds_left()).c_str(), 1);
        auto [kept, dropped] = distill_new(db, *key, 40, false, Rubric::Assistant);
        auto ukept = std::size_t{0}, udropped = std::size_t{0};
        if (!budget.spent()) {
            setenv("CML_HTTP_TIMEOUT", std::to_string(budget.seconds_left()).c_str(), 1);
            std::tie(ukept, udropped) = distill_new(db, *key, 40, false, Rubric::User);
        }
        kept += ukept;
        dropped += udropped;
        if (kept + dropped > 0) {
            curated = ", curated " + std::to_string(kept) + "+" + std::to_string(dropped) +
                      "dropped";
            clear_curator_backoff();
        } else if (left_before - budget.seconds_left() >= 1) {
            start_curator_backoff();
            curated = ", curator not answering - standing down for 15m";
        }
    }

    std::printf(
        "indexed %zu file(s), %zu row(s), %zu embedded%s  [transcripts %zu/%zu, memory %zu/%zu, wiki %zu/%zu]\n",
        t.files + m.files + w.files, t.rows + m.rows + w.rows, embedded, curated.c_str(),
        t.files, t.rows, m.files, m.rows, w.files, w.rows);
    return 0;
}

}  // namespace cml
