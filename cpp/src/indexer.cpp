#include "indexer.hpp"

#include <cstdio>
#include <filesystem>
#include <fstream>
#include <sstream>
#include <string>
#include <unordered_set>

#include "curate.hpp"
#include "db.hpp"
#include "noise.hpp"
#include "paths.hpp"
#include "transcript.hpp"
#include "utf8.hpp"
#include "vec.hpp"

namespace fs = std::filesystem;

namespace cml {
namespace {

// Signal floor. Short assistant rows are mode-acks. Short user rows ("please",
// "ok done", "522556") are never how anyone finds anything in a search index, and
// the assistant reply that follows carries the answer and is kept anyway.
// ponytail: word count, not semantics — drop to 2 if real messages start vanishing.
constexpr std::size_t kUserMinWords = 4;

struct FileMeta {
    std::int64_t size = 0;
    std::int64_t mtime = 0;
};

bool meta_of(const fs::path& p, FileMeta& out) {
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

bool unchanged(Db& db, const std::string& path, const FileMeta& fm) {
    Stmt s(db, "SELECT size, mtime FROM files WHERE path=?1");
    s.bind(1, path);
    return s.step() && s.i64(0) == fm.size && s.i64(1) == fm.mtime;
}

void upsert_file(Db& db, const std::string& path, const FileMeta& fm) {
    Stmt s(db,
           "INSERT INTO files(path,size,mtime) VALUES(?1,?2,?3) "
           "ON CONFLICT(path) DO UPDATE SET size=?2, mtime=?3");
    s.bind(1, path).bind(2, fm.size).bind(3, fm.mtime);
    s.run();
}

void drop_rows_for_file(Db& db, const std::string& path) {
    // vec_mem only exists once `cml embed` has run; failing here is normal.
    Stmt v(db, "DELETE FROM vec_mem WHERE rowid IN (SELECT rowid FROM mem WHERE file=?1)");
    if (v) {
        v.bind(1, path);
        v.run();
    }
    Stmt m(db, "DELETE FROM mem WHERE file=?1");
    m.bind(1, path).run();
}

std::unordered_set<std::string> existing_keys(Db& db) {
    // Continued sessions leave overlapping rows across two transcript files, so dedup
    // against everything already indexed (this file's own rows have just been deleted).
    std::unordered_set<std::string> keys;
    Stmt s(db, "SELECT session, ts, role, substr(text,1,64) FROM mem");
    while (s.step()) keys.insert(stable_key(s.text(0), s.text(1), s.text(2), s.text(3)));
    return keys;
}

// Rust counts the signal floor on text.trim(); counting untrimmed would let a
// whitespace-padded stub slip past a floor the Rust binary enforces.
std::string_view trimmed(std::string_view s) {
    std::size_t b = 0, e = s.size();
    while (b < e && std::isspace(static_cast<unsigned char>(s[b]))) ++b;
    while (e > b && std::isspace(static_cast<unsigned char>(s[e - 1]))) --e;
    return s.substr(b, e - b);
}

std::size_t word_count(std::string_view s) {
    std::size_t n = 0;
    bool in_word = false;
    for (const char c : s) {
        const bool space = std::isspace(static_cast<unsigned char>(c));
        if (!space && !in_word) ++n;
        in_word = !space;
    }
    return n;
}

struct Counts {
    std::size_t files = 0;
    std::size_t rows = 0;
};

Counts index_transcripts(Db& db, bool force) {
    Counts total;
    const auto blocked = forgotten_set(db);
    const fs::path projects = home() / ".claude/projects";
    std::error_code ec;
    if (!fs::is_directory(projects, ec)) return total;

    for (const auto& pdir : fs::directory_iterator(projects, ec)) {
        if (!pdir.is_directory()) continue;
        const std::string project = project_label(pdir.path().filename().string());

        // ponytail: top-level session files only; subagent transcripts are skipped.
        std::error_code inner;
        for (const auto& f : fs::directory_iterator(pdir.path(), inner)) {
            const fs::path path = f.path();
            if (path.extension() != ".jsonl") continue;
            FileMeta fm;
            if (!meta_of(path, fm)) continue;
            const std::string pstr = path.string();
            if (!force && unchanged(db, pstr, fm)) continue;

            const std::string session_fallback = path.stem().string();
            db.exec("BEGIN");
            drop_rows_for_file(db, pstr);
            auto seen = existing_keys(db);

            const auto entries = parse_transcript(pstr, session_fallback);
            Stmt ins(db,
                     "INSERT INTO mem(text, role, project, session, ts, file) "
                     "VALUES(?1,?2,?3,?4,?5,?6)");
            std::size_t n = 0;
            for (std::size_t i = 0; i < entries.size(); ++i) {
                const Entry& e = entries[i];
                const char* role = nullptr;
                switch (e.kind) {
                    case Entry::Kind::Summary: role = "summary"; break;
                    case Entry::Kind::UserHuman: role = "user"; break;
                    case Entry::Kind::AssistantText:
                        if (!turn_final(entries, i)) continue;
                        role = "assistant";
                        break;
                    default: continue;
                }
                const std::string& text = e.text;
                if (is_noise(text)) continue;

                const std::size_t len = utf8_len(trimmed(text));
                const bool assistant = (e.kind == Entry::Kind::AssistantText);
                if ((assistant && len < 80) || len < 4) continue;
                if (e.kind == Entry::Kind::UserHuman && word_count(text) < kUserMinWords) continue;

                const std::string ts = (e.kind == Entry::Kind::Summary) ? "" : e.ts;
                const std::string sid =
                    (e.kind == Entry::Kind::Summary) ? session_fallback : e.session;
                const std::string key = stable_key(sid, ts, role, text);
                if (blocked.count(key)) continue;
                if (!seen.insert(key).second) continue;

                ins.reset();
                ins.bind(1, text).bind(2, role).bind(3, project).bind(4, sid).bind(5, ts).bind(6, pstr);
                if (ins.run()) ++n;
            }
            upsert_file(db, pstr, fm);
            db.exec("COMMIT");
            ++total.files;
            total.rows += n;
        }
    }
    return total;
}

// Whole-file rows for markdown notes (role "memory" or "wiki").
Counts index_md_dir(Db& db, const fs::path& dir, const char* role, const std::string& project,
                    bool force) {
    Counts total;
    std::error_code ec;
    if (!fs::is_directory(dir, ec)) return total;

    for (const auto& f : fs::directory_iterator(dir, ec)) {
        const fs::path path = f.path();
        if (path.extension() != ".md") continue;
        FileMeta fm;
        if (!meta_of(path, fm)) continue;
        const std::string pstr = path.string();
        if (!force && unchanged(db, pstr, fm)) continue;

        std::ifstream in(path);
        if (!in) continue;
        std::ostringstream buf;
        buf << in.rdbuf();
        const std::string text = buf.str();

        db.exec("BEGIN");
        drop_rows_for_file(db, pstr);
        if (!text.empty() && text.find_first_not_of(" \t\r\n") != std::string::npos) {
            Stmt ins(db,
                     "INSERT INTO mem(text, role, project, session, ts, file) "
                     "VALUES(?1,?2,?3,?4,?5,?6)");
            ins.bind(1, text).bind(2, role).bind(3, project)
               .bind(4, path.stem().string()).bind(5, iso_date(fm.mtime)).bind(6, pstr);
            if (ins.run()) ++total.rows;
        }
        upsert_file(db, pstr, fm);
        db.exec("COMMIT");
        ++total.files;
    }
    return total;
}

}  // namespace

int index_all(bool force) {
    Db db = open_db();
    if (!db) {
        std::fprintf(stderr, "cml: cannot open index\n");
        return 1;
    }

    const Counts t = index_transcripts(db, force);
    Counts m;
    std::error_code ec;
    const fs::path projects = home() / ".claude/projects";
    if (fs::is_directory(projects, ec)) {
        for (const auto& pdir : fs::directory_iterator(projects, ec)) {
            if (!pdir.is_directory()) continue;
            const std::string project = project_label(pdir.path().filename().string());
            const Counts c = index_md_dir(db, pdir.path() / "memory", "memory", project, force);
            m.files += c.files;
            m.rows += c.rows;
        }
    }
    const Counts w = index_md_dir(db, data_dir() / "wiki", "wiki", "wiki", force);

    // Incremental semantic pass. `cml index` runs from the Stop hook every turn, so
    // without this the vector table would fall permanently behind the FTS index and
    // hybrid search would quietly lose recall on everything recent. No-ops unless
    // `cml embed` has already created the table.
    const std::size_t embedded = embed_new(db);

    // Automatic curation: judge new rows when a curator key is configured, capped so
    // the Stop hook stays quick — the backlog drains over turns, or via `cml distill`.
    std::string curated;
    if (const auto key = llm_key()) {
        const auto [kept, dropped] = distill_new(db, *key, 40, false);
        if (kept + dropped > 0) {
            curated = ", curated " + std::to_string(kept) + "+" + std::to_string(dropped) +
                      "dropped";
        }
    }

    std::printf(
        "indexed %zu file(s), %zu row(s), %zu embedded%s  [transcripts %zu/%zu, memory %zu/%zu, wiki %zu/%zu]\n",
        t.files + m.files + w.files, t.rows + m.rows + w.rows, embedded, curated.c_str(),
        t.files, t.rows, m.files, m.rows, w.files, w.rows);
    return 0;
}

}  // namespace cml
