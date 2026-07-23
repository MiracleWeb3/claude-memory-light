// forget: purge junk from the brain, permanently.
//
// Deleting the row is the easy half. The row came from a transcript that is still on
// disk, so the next `cml index` would re-read it and put it right back — which is why
// every deletion also writes the row's stable_key into `forgotten`. That key survives
// re-indexing; the rowid does not.

#include <cctype>
#include <cstdio>
#include <string>
#include <vector>

#include "curate.hpp"
#include "db.hpp"
#include "utf8.hpp"

namespace cml {
namespace {

std::string pad(std::string s, std::size_t width) {
    if (s.size() < width) s.append(width - s.size(), ' ');
    return s;
}

std::string collapse_ws(std::string_view s) {
    std::string out;
    bool gap = true;
    for (const char c : s) {
        if (std::isspace(static_cast<unsigned char>(c))) {
            gap = true;
            continue;
        }
        if (gap && !out.empty()) out.push_back(' ');
        gap = false;
        out.push_back(c);
    }
    return out;
}

}  // namespace

std::string match_query(const std::string& raw) {
    std::string q;
    std::string tok;
    const auto flush = [&] {
        if (tok.empty()) return;
        if (!q.empty()) q += " AND ";
        q += '"';
        for (const char c : tok) {
            q.push_back(c);
            if (c == '"') q.push_back('"');  // FTS5 doubles the quote
        }
        q += '"';
        tok.clear();
    };
    for (const char c : raw) {
        if (std::isspace(static_cast<unsigned char>(c))) {
            flush();
        } else {
            tok.push_back(c);
        }
    }
    flush();
    return q;
}

bool purge_rowid(Db& db, std::int64_t id) {
    std::string session, ts, role, head;
    {
        Stmt s(db, "SELECT session, ts, role, substr(text,1,64) FROM mem WHERE rowid=?1");
        s.bind(1, id);
        if (!s.step()) return false;
        session = s.text(0);
        ts = s.text(1);
        role = s.text(2);
        head = s.text(3);
    }
    Stmt block(db, "INSERT OR IGNORE INTO forgotten(key) VALUES (?1)");
    block.bind(1, stable_key(session, ts, role, head));
    block.run();
    // vec_mem only exists once embeddings have been built; missing table is fine.
    Stmt unvec(db, "DELETE FROM vec_mem WHERE rowid=?1");
    unvec.bind(1, id);
    unvec.run();
    Stmt del(db, "DELETE FROM mem WHERE rowid=?1");
    del.bind(1, id);
    return del.run();
}

int forget(const std::vector<std::string>& args) {
    Db db = open_db();
    if (!db) {
        std::fprintf(stderr, "cml: cannot open index\n");
        return 1;
    }

    const auto has = [&args](std::string_view flag) {
        for (const auto& a : args) {
            if (a == flag) return true;
        }
        return false;
    };

    if (has("--clear")) {
        if (!db.exec("DELETE FROM forgotten")) {
            std::fprintf(stderr, "cml: %s\n", db.error().c_str());
            return 1;
        }
        std::printf("blocklist cleared (%d keys) — run `cml index --all` to resurrect those rows\n",
                    sqlite3_changes(db.raw()));
        return 0;
    }

    const bool yes = has("--yes");
    std::vector<std::int64_t> rowids;

    std::size_t mi = args.size();
    for (std::size_t i = 0; i < args.size(); ++i) {
        if (args[i] == "--match") {
            mi = i;
            break;
        }
    }

    if (mi != args.size()) {
        if (mi + 1 >= args.size()) {
            std::fprintf(stderr, "cml: usage: cml forget --match \"<query>\" [--yes]\n");
            return 1;
        }
        const std::string& raw = args[mi + 1];
        Stmt s(db,
               "SELECT rowid, role, project, ts, substr(text,1,90) FROM mem "
               "WHERE mem MATCH ?1 ORDER BY rowid");
        if (!s) {
            std::fprintf(stderr, "cml: %s\n", db.error().c_str());
            return 1;
        }
        s.bind(1, match_query(raw));
        std::string listing;
        while (s.step()) {
            rowids.push_back(s.i64(0));
            char id[24];
            std::snprintf(id, sizeof id, "%7lld", static_cast<long long>(s.i64(0)));
            listing += id;
            listing += ' ' + pad(s.text(1), 9) + ' ' + pad(s.text(2), 14) + ' ' +
                       utf8_take(s.text(3), 10) + " | " + collapse_ws(s.text(4)) + '\n';
        }
        if (rowids.empty()) {
            std::printf("no rows match: %s\n", raw.c_str());
            return 0;
        }
        std::fputs(listing.c_str(), stdout);
        if (!yes) {
            std::printf("---\n%zu row(s) matched — re-run with --yes to forget them\n",
                        rowids.size());
            return 0;
        }
    } else {
        for (const auto& a : args) {
            try {
                std::size_t used = 0;
                const long long id = std::stoll(a, &used);
                if (used == a.size()) rowids.push_back(id);
            } catch (const std::exception&) {
                // not a rowid (a flag, or junk) — Rust's parse() skips these too
            }
        }
        if (rowids.empty()) {
            std::fprintf(stderr,
                         "cml: usage: cml forget <rowid...> | --match \"<query>\" [--yes] "
                         "| --clear\n");
            return 1;
        }
    }

    std::size_t n = 0;
    for (const std::int64_t id : rowids) {
        if (purge_rowid(db, id)) ++n;
    }
    std::printf("forgot %zu row(s) — blocklisted so they never come back "
                "(undo: cml forget --clear)\n",
                n);
    return 0;
}

}  // namespace cml
