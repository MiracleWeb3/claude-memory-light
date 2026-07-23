#include "db.hpp"

#include <filesystem>
#include <utility>

#include "paths.hpp"
#include "utf8.hpp"

namespace cml {

Db::Db(const std::string& path) {
    if (sqlite3_open(path.c_str(), &db_) != SQLITE_OK) {
        sqlite3_close(db_);
        db_ = nullptr;
    }
}

Db::~Db() {
    if (db_) sqlite3_close(db_);
}

Db::Db(Db&& other) noexcept : db_(std::exchange(other.db_, nullptr)) {}

Db& Db::operator=(Db&& other) noexcept {
    if (this != &other) {
        if (db_) sqlite3_close(db_);
        db_ = std::exchange(other.db_, nullptr);
    }
    return *this;
}

bool Db::exec(const char* sql) {
    if (!db_) return false;
    return sqlite3_exec(db_, sql, nullptr, nullptr, nullptr) == SQLITE_OK;
}

std::int64_t Db::scalar(const char* sql, std::int64_t fallback) {
    Stmt s(*this, sql);
    if (s && s.step()) return s.i64(0);
    return fallback;
}

std::string Db::error() const { return db_ ? sqlite3_errmsg(db_) : "db not open"; }

Stmt::Stmt(Db& db, std::string_view sql) {
    if (!db) return;
    if (sqlite3_prepare_v2(db.raw(), sql.data(), static_cast<int>(sql.size()), &st_,
                           nullptr) != SQLITE_OK) {
        sqlite3_finalize(st_);
        st_ = nullptr;
    }
}

Stmt::~Stmt() {
    if (st_) sqlite3_finalize(st_);
}

Stmt& Stmt::bind(int i, std::string_view v) {
    if (st_) sqlite3_bind_text(st_, i, v.data(), static_cast<int>(v.size()), SQLITE_TRANSIENT);
    return *this;
}

Stmt& Stmt::bind(int i, std::int64_t v) {
    if (st_) sqlite3_bind_int64(st_, i, v);
    return *this;
}

Stmt& Stmt::bind_blob(int i, const void* data, std::size_t bytes) {
    if (st_) sqlite3_bind_blob(st_, i, data, static_cast<int>(bytes), SQLITE_TRANSIENT);
    return *this;
}

bool Stmt::step() { return st_ && sqlite3_step(st_) == SQLITE_ROW; }

bool Stmt::run() { return st_ && sqlite3_step(st_) == SQLITE_DONE; }

void Stmt::reset() {
    if (st_) {
        sqlite3_reset(st_);
        sqlite3_clear_bindings(st_);
    }
}

std::string Stmt::text(int col) const {
    if (!st_) return {};
    const auto* p = sqlite3_column_text(st_, col);
    if (!p) return {};
    return std::string(reinterpret_cast<const char*>(p),
                       static_cast<std::size_t>(sqlite3_column_bytes(st_, col)));
}

std::int64_t Stmt::i64(int col) const { return st_ ? sqlite3_column_int64(st_, col) : 0; }

// Defined in vec.cpp. Declared here rather than including vec.hpp, which includes
// this header — the dependency runs one way only.
void register_vec_extension();

Db open_db() {
    // sqlite-vec must be registered before any connection opens, or vec0 tables are
    // invisible and `vec_mem` looks like a missing table instead of a missing extension.
    register_vec_extension();
    const auto dir = data_dir();
    std::error_code ec;
    std::filesystem::create_directories(dir, ec);
    std::filesystem::create_directories(dir / "wiki", ec);
    std::filesystem::create_directories(dir / "inbox", ec);

    Db db((dir / "index.db").string());
    if (!db) return db;

    // The FTS5 definition must never drift, tokenizer included — FTS5 stores the
    // tokenizer in the table definition, so a mismatch would rebuild the index.
    // `hints` is additive (2.5.0, per-session prompt-hint dedupe); older binaries
    // simply ignore it.
    const bool ok = db.exec(
        "PRAGMA journal_mode=WAL;"
        "PRAGMA synchronous=NORMAL;"
        "CREATE TABLE IF NOT EXISTS files(path TEXT PRIMARY KEY, size INTEGER, mtime INTEGER);"
        "CREATE TABLE IF NOT EXISTS forgotten(key TEXT PRIMARY KEY);"
        "CREATE TABLE IF NOT EXISTS distilled(key TEXT PRIMARY KEY, gist TEXT);"
        "CREATE TABLE IF NOT EXISTS hints(session TEXT, category TEXT,"
        "    PRIMARY KEY(session, category));"
        "CREATE VIRTUAL TABLE IF NOT EXISTS mem USING fts5("
        "    text, role UNINDEXED, project UNINDEXED, session UNINDEXED,"
        "    ts UNINDEXED, file UNINDEXED, tokenize='porter unicode61');");
    if (!ok) return Db{};
    return db;
}

std::string stable_key(std::string_view session, std::string_view ts,
                       std::string_view role, std::string_view text) {
    return std::string(session) + "|" + std::string(ts) + "|" + std::string(role) + "|" +
           utf8_take(text, 64);
}

std::unordered_set<std::string> forgotten_set(Db& db) {
    std::unordered_set<std::string> out;
    Stmt s(db, "SELECT key FROM forgotten");
    while (s.step()) out.insert(s.text(0));
    return out;
}

std::unordered_map<std::string, std::string> gist_lookup(Db& db) {
    std::unordered_map<std::string, std::string> out;
    Stmt s(db, "SELECT key, gist FROM distilled WHERE gist != ''");
    while (s.step()) out.emplace(s.text(0), s.text(1));
    return out;
}

bool vec_table_exists(Db& db) {
    Stmt s(db, "SELECT 1 FROM sqlite_master WHERE type='table' AND name='vec_mem'");
    return s.step();
}

}  // namespace cml
