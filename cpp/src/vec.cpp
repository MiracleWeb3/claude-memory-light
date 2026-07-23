#include "vec.hpp"

#include <sqlite3.h>

#include <algorithm>
#include <chrono>
#include <cstdio>

#include "embed.hpp"

extern "C" int sqlite3_vec_init(sqlite3* db, char** pzErrMsg,
                                const sqlite3_api_routines* pApi);

namespace cml {
namespace {

// Matches the Rust binary: only the first 2000 characters of a row are embedded.
constexpr int kEmbedChars = 2000;
constexpr std::size_t kBatch = 256;

}  // namespace

void register_vec_extension() {
    static bool done = false;
    if (done) return;
    done = true;
    // The cast mirrors what the Rust binary does through rusqlite's ffi.
    sqlite3_auto_extension(reinterpret_cast<void (*)()>(sqlite3_vec_init));
}

namespace {

// Shared by `cml embed` and the incremental pass inside `cml index`.
std::size_t embed_pending(Db& db, const StaticModel& model) {
    std::vector<std::pair<std::int64_t, std::string>> pending;
    {
        Stmt s(db,
               "SELECT rowid, substr(text,1,?1) FROM mem "
               "WHERE rowid NOT IN (SELECT rowid FROM vec_mem)");
        if (!s) return 0;
        s.bind(1, static_cast<std::int64_t>(kEmbedChars));
        while (s.step()) pending.emplace_back(s.i64(0), s.text(1));
    }
    if (pending.empty()) return 0;

    std::size_t done = 0;
    Stmt ins(db, "INSERT INTO vec_mem(rowid, embedding) VALUES (?1, ?2)");
    for (std::size_t i = 0; i < pending.size(); i += kBatch) {
        db.exec("BEGIN");
        const std::size_t end = std::min(i + kBatch, pending.size());
        for (std::size_t j = i; j < end; ++j) {
            const std::vector<float> v = model.encode(pending[j].second);
            ins.reset();
            ins.bind(1, pending[j].first).bind_blob(2, v.data(), v.size() * sizeof(float));
            if (ins.run()) ++done;
        }
        db.exec("COMMIT");
    }
    return done;
}

}  // namespace

std::size_t embed_new(Db& db) {
    // Gated on the table existing: `cml index` runs from a Stop hook on every turn,
    // and it must never load a model for someone who never ran `cml embed`.
    if (!vec_table_exists(db)) return 0;
    std::string error;
    const StaticModel model = StaticModel::load(embed_model_id(), error);
    if (!model.ok()) return 0;  // silent: a hook must not break the session
    return embed_pending(db, model);
}

int embed_cmd(const std::vector<std::string>& args) {
    const bool all = std::any_of(args.begin(), args.end(),
                                 [](const std::string& a) { return a == "--all"; });

    Db db = open_db();
    if (!db) {
        std::fprintf(stderr, "cml: cannot open index\n");
        return 1;
    }
    if (all && vec_table_exists(db)) db.exec("DROP TABLE vec_mem;");

    std::string error;
    const std::string id = embed_model_id();
    const StaticModel model = StaticModel::load(id, error);
    if (!model.ok()) {
        std::fprintf(stderr, "cml: %s\n", error.c_str());
        return 1;
    }

    const std::string create = "CREATE VIRTUAL TABLE IF NOT EXISTS vec_mem USING vec0("
                               "embedding float[" + std::to_string(model.dim()) + "]);";
    if (!db.exec(create.c_str())) {
        std::fprintf(stderr, "cml: cannot create vector table: %s\n", db.error().c_str());
        return 1;
    }

    const auto t0 = std::chrono::steady_clock::now();
    const std::size_t done = embed_pending(db, model);

    const double secs = std::chrono::duration<double>(std::chrono::steady_clock::now() - t0).count();
    const std::int64_t total = db.scalar("SELECT count(*) FROM vec_mem");
    std::printf("embedded %zu new row(s) in %.1fs (%lld total, %zu-dim, %s)\n", done, secs,
                static_cast<long long>(total), model.dim(), id.c_str());
    return 0;
}

std::vector<std::int64_t> semantic_hits(Db& db, const std::string& query, int k,
                                        std::string& error) {
    std::vector<std::int64_t> out;
    if (!vec_table_exists(db)) {
        error = "semantic index missing — run `cml embed` once to build it";
        return out;
    }
    const StaticModel model = StaticModel::load(embed_model_id(), error);
    if (!model.ok()) return out;

    const std::vector<float> q = model.encode(query);
    Stmt s(db, "SELECT rowid FROM vec_mem WHERE embedding MATCH ?1 AND k = ?2 ORDER BY distance");
    if (!s) {
        error = "vector query failed: " + db.error();
        return out;
    }
    s.bind_blob(1, q.data(), q.size() * sizeof(float));
    s.bind(2, static_cast<std::int64_t>(k));
    while (s.step()) out.push_back(s.i64(0));
    return out;
}

}  // namespace cml
