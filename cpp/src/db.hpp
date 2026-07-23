// SQLite access. C++ talks to sqlite3 directly — the Rust build needed rusqlite to
// reach what is already a C library.
//
// The schema here must stay byte-identical to the Rust binary's: both write the same
// index.db, and a divergence silently splits the memory in two.
#pragma once

#include <sqlite3.h>

#include <cstdint>
#include <string>
#include <string_view>
#include <unordered_map>
#include <unordered_set>

namespace cml {

class Db {
public:
    Db() = default;
    explicit Db(const std::string& path);
    ~Db();
    Db(const Db&) = delete;
    Db& operator=(const Db&) = delete;
    Db(Db&& other) noexcept;
    Db& operator=(Db&& other) noexcept;

    explicit operator bool() const { return db_ != nullptr; }
    sqlite3* raw() const { return db_; }
    bool exec(const char* sql);
    std::int64_t scalar(const char* sql, std::int64_t fallback = 0);
    std::string error() const;

private:
    sqlite3* db_ = nullptr;
};

class Stmt {
public:
    Stmt(Db& db, std::string_view sql);
    ~Stmt();
    Stmt(const Stmt&) = delete;
    Stmt& operator=(const Stmt&) = delete;

    explicit operator bool() const { return st_ != nullptr; }
    Stmt& bind(int i, std::string_view v);
    Stmt& bind(int i, std::int64_t v);
    // Raw bytes — vectors are float arrays, not text.
    Stmt& bind_blob(int i, const void* data, std::size_t bytes);
    bool step();  // true while a row is available
    bool run();   // step once, for INSERT/DELETE; true on success
    void reset();

    std::string text(int col) const;
    std::int64_t i64(int col) const;

private:
    sqlite3_stmt* st_ = nullptr;
};

// Opens (and creates) $CML_HOME/index.db with the schema applied.
// Returns a closed Db on failure; check with operator bool.
Db open_db();

// Content-stable identity for a message, so forgetting survives re-indexing:
// rowids change when a transcript is re-parsed, this key does not.
std::string stable_key(std::string_view session, std::string_view ts,
                       std::string_view role, std::string_view text);

std::unordered_set<std::string> forgotten_set(Db& db);

// stable_key -> distilled gist: what search DISPLAYS for a curated row.
std::unordered_map<std::string, std::string> gist_lookup(Db& db);

bool vec_table_exists(Db& db);

}  // namespace cml
