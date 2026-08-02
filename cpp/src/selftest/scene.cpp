// The L2 scene table. This is the first suite that opens a database, so it is also the
// first that must not open the REAL one: every case here points $CML_HOME at a temp dir
// before calling open_db(), because data_dir() reads that variable on every call
// (paths.cpp:14-19). A suite that skipped it would rewrite the user's own memory index.
#include <cstdlib>
#include <filesystem>
#include <string>

#include "db.hpp"
#include "harness.hpp"

namespace cml_test {
namespace {

std::filesystem::path sandbox() {
    return std::filesystem::temp_directory_path() / "cml-selftest-scene";
}

// Adding a table must not disturb what is already in the index. The only destructive
// path in open_db() keys on `mem`'s column list (db.cpp:126) — this asserts that a
// second open finds the rows the first one wrote, still there.
void test_scene_schema_is_additive() {
    const auto tmp = sandbox();
    std::filesystem::remove_all(tmp);
    std::filesystem::create_directories(tmp);
    setenv("CML_HOME", tmp.c_str(), 1);
    {
        cml::Db db = cml::open_db();
        ok(static_cast<bool>(db), "index opens");
        ok(db.scalar("SELECT count(*) FROM sqlite_master WHERE name='scene'") == 1,
           "scene table created");
        ok(db.exec("INSERT INTO mem(text,asks,role,project,session,ts,file)"
                   " VALUES('hello','','user','p','s','2026-01-01','f')"),
           "mem still writable alongside it");
    }
    {
        cml::Db db = cml::open_db();  // reopening must not rebuild
        ok(db.scalar("SELECT count(*) FROM mem") == 1, "additive means additive");
    }
    unsetenv("CML_HOME");
    std::filesystem::remove_all(tmp);
}

// FTS5 stores its column list and tokenizer inside the table definition, so drift here
// is not a warning — it is a silent rebuild of everything the table holds. The MATCH
// pair is the load-bearing half: it proves the UNINDEXED metadata really is unindexed,
// which no reading of the CREATE statement can establish.
void test_scene_indexes_prose_and_not_metadata() {
    const auto tmp = sandbox();
    std::filesystem::remove_all(tmp);
    std::filesystem::create_directories(tmp);
    setenv("CML_HOME", tmp.c_str(), 1);
    {
        cml::Db db = cml::open_db();
        ok(db.exec("INSERT INTO scene(title,summary,outcome,session,project,"
                   "ts_start,ts_end,n_rows) VALUES('DAITA vs 1280 path MTU',"
                   "'constant-size padding cannot fit','solved','zzsession','p',"
                   "'2026-07-31','2026-07-31',42)"),
           "a scene writes with all eight columns");
        ok(db.scalar("SELECT n_rows FROM scene") == 42, "metadata reads back");
        ok(db.scalar("SELECT count(*) FROM scene WHERE scene MATCH 'padding'") == 1,
           "the summary is searchable");
        ok(db.scalar("SELECT count(*) FROM scene WHERE scene MATCH 'zzsession'") == 0,
           "the session id is not — metadata stays UNINDEXED");
    }
    unsetenv("CML_HOME");
    std::filesystem::remove_all(tmp);
}

}  // namespace

void suite_scene() {
    test_scene_schema_is_additive();
    test_scene_indexes_prose_and_not_metadata();
}

}  // namespace cml_test
