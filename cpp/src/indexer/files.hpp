// What both scanners need: is this file new, and how do its old rows get cleared.
//
// Transcripts and markdown notes are indexed by two independent scanners that agree on
// exactly this much — a size+mtime staleness check, and a delete-then-reinsert per file.
// Everything else about them differs, which is why they are separate translation units.
#pragma once

#include <chrono>
#include <cstdint>
#include <filesystem>
#include <string>

#include "budget.hpp"
#include "db.hpp"

namespace cml::idx {

namespace fs = std::filesystem;

struct FileMeta {
    std::int64_t size = 0;
    std::int64_t mtime = 0;
};

struct Counts {
    std::size_t files = 0;
    std::size_t rows = 0;
};

inline bool meta_of(const fs::path& p, FileMeta& out) {
    std::error_code ec;
    if (!fs::is_regular_file(p, ec)) return false;
    out.size = static_cast<std::int64_t>(fs::file_size(p, ec));
    if (ec) return false;
    const auto t = fs::last_write_time(p, ec);
    if (ec) return false;
    // file_time_type -> unix seconds, without the C++20 clock_cast some libstdc++
    // versions still lack.
    const auto sys = std::chrono::time_point_cast<std::chrono::system_clock::duration>(
        t - fs::file_time_type::clock::now() + std::chrono::system_clock::now());
    out.mtime = std::chrono::duration_cast<std::chrono::seconds>(sys.time_since_epoch()).count();
    return true;
}

inline bool unchanged(Db& db, const std::string& path, const FileMeta& fm) {
    Stmt s(db, "SELECT size, mtime FROM files WHERE path=?1");
    s.bind(1, path);
    return s.step() && s.i64(0) == fm.size && s.i64(1) == fm.mtime;
}

inline void upsert_file(Db& db, const std::string& path, const FileMeta& fm) {
    Stmt s(db,
           "INSERT INTO files(path,size,mtime) VALUES(?1,?2,?3) "
           "ON CONFLICT(path) DO UPDATE SET size=?2, mtime=?3");
    s.bind(1, path).bind(2, fm.size).bind(3, fm.mtime);
    s.run();
}

inline void drop_rows_for_file(Db& db, const std::string& path) {
    {
        Stmt w(db, "DELETE FROM work WHERE file = ?1");
        w.bind(1, path);
        w.run();
    }
    // vec_mem only exists once `cml embed` has run; failing here is normal.
    Stmt v(db, "DELETE FROM vec_mem WHERE rowid IN (SELECT rowid FROM mem WHERE file=?1)");
    if (v) {
        v.bind(1, path);
        v.run();
    }
    Stmt m(db, "DELETE FROM mem WHERE file=?1");
    m.bind(1, path).run();
}

Counts index_transcripts(Db& db, bool force, const Budget& budget);
Counts index_md_dir(Db& db, const fs::path& dir, const char* role, const std::string& project,
                    bool force);

}  // namespace cml::idx
