//! Opening the index, and the schema it guarantees.
//!
//! The C++ tree carried ~200 lines of RAII wrapper here — a `Stmt` class, bind
//! chaining, a scoped transaction guard. None of that moves: rusqlite already is
//! that wrapper, and `?` already is the error path. What survives is the part with
//! actual decisions in it: where the database lives, and what shape it must be in.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

/// Where the index and its sidecar files live.
///
/// `CML_HOME` wins so a test run can point at a scratch copy instead of the real
/// 148 MB index.
pub fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("CML_HOME") {
        return PathBuf::from(h);
    }
    let base = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let mut h = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
            h.push(".claude");
            h
        });
    base.join("claude-memory-light")
}

/// Claude Code's own transcript directory.
pub fn transcripts_dir() -> PathBuf {
    let base = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let mut h = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
            h.push(".claude");
            h
        });
    base.join("projects")
}

pub fn db_path() -> PathBuf {
    home().join("index.db")
}

/// Open the index, creating the schema if this is a fresh machine.
///
/// The schema below is byte-compatible with the one the C++ tree writes, on
/// purpose: this binary must open the user's existing 148 MB index and see every
/// row already in it. A rewrite that orphaned three years of history would be a
/// regression no benchmark shows.
pub fn open() -> rusqlite::Result<Connection> {
    open_at(&db_path())
}

pub fn open_at(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let conn = Connection::open(path)?;
    // WAL keeps a long index run from blocking a concurrent `search`, which
    // happens constantly: the SessionStart hook reads while indexing writes.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    ensure_schema(&conn)?;
    Ok(conn)
}

/// Open read-only. Used by the hot path (recall, search) so a corrupt or
/// mid-write index can never be made worse by a reader.
pub fn open_ro() -> rusqlite::Result<Connection> {
    use rusqlite::OpenFlags;
    let conn = Connection::open_with_flags(
        db_path(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    Ok(conn)
}

pub fn ensure_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA)
}

/// `IF NOT EXISTS` throughout: this runs against a populated production index far
/// more often than against an empty one.
const SCHEMA: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS mem USING fts5(
    text, asks, role UNINDEXED, project UNINDEXED, session UNINDEXED,
    ts UNINDEXED, file UNINDEXED, tokenize='porter unicode61');

CREATE VIRTUAL TABLE IF NOT EXISTS work USING fts5(
    text, role UNINDEXED, project UNINDEXED, session UNINDEXED,
    ts UNINDEXED, file UNINDEXED, tokenize='porter unicode61');

CREATE VIRTUAL TABLE IF NOT EXISTS scene USING fts5(
    title, summary, outcome, session UNINDEXED, project UNINDEXED,
    ts_start UNINDEXED, ts_end UNINDEXED, n_rows UNINDEXED,
    tokenize='porter unicode61');

CREATE TABLE IF NOT EXISTS files(path TEXT PRIMARY KEY, size INTEGER, mtime INTEGER);
CREATE TABLE IF NOT EXISTS distilled(key TEXT PRIMARY KEY, gist TEXT);
CREATE TABLE IF NOT EXISTS forgotten(key TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS hints(session TEXT, category TEXT, PRIMARY KEY(session, category));
CREATE TABLE IF NOT EXISTS recalled(session TEXT, key TEXT, PRIMARY KEY(session, key));

-- Where a row came from: which harness wrote it, and which person, if not you.
--
-- A sidecar table rather than a column, because `mem` and `work` are FTS5
-- virtual tables and those do not take ALTER TABLE ADD COLUMN. Keying it by
-- `file` rather than by rowid is what makes it survive the delete-then-insert
-- that re-indexing performs: FTS5 hands rowids back out after a delete, so a
-- rowid-keyed origin would eventually attribute a stranger's row to you.
--
-- Absent from this table means what the index has always meant: a Claude Code
-- transcript of your own. That is why the 208 MB index already on this machine
-- needs no migration at all.
CREATE TABLE IF NOT EXISTS origin(
    file TEXT PRIMARY KEY, harness TEXT NOT NULL, peer TEXT);

-- What running a command has cost, tallied per normalized signature.
--
-- A plain table, not FTS5: the lookup is an exact match on a signature the caller
-- computes, never a search. Rebuilt wholesale by `cml outcomes` rather than kept
-- incrementally — the whole pass over 1,214 transcripts is one read of files that
-- are already on disk, and a wrong tally is worse than a stale one.
CREATE TABLE IF NOT EXISTS outcome(
    sig TEXT PRIMARY KEY, tries INTEGER NOT NULL, fails INTEGER NOT NULL, sample TEXT);

-- Embeddings as a plain BLOB of little-endian f32, not a vec0 virtual table.
-- sqlite-vec was 324K of vendored C and an unsafe extension load; 5,576 x 256
-- floats is 5.7 MB, and a rayon cosine sweep over it is faster than the query
-- parse that precedes it. The extension, its vendored source, and the only
-- `unsafe` in the tree all leave together.
CREATE TABLE IF NOT EXISTS emb(
    lane TEXT NOT NULL, rowid_ref INTEGER NOT NULL, dim INTEGER NOT NULL,
    v BLOB NOT NULL, PRIMARY KEY(lane, rowid_ref));
"#;

/// Count rows in a table, or 0 if it does not exist yet.
pub fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_idempotent_and_opens_twice() {
        let dir = std::env::temp_dir().join(format!("cml-schema-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("index.db");

        let c1 = open_at(&path).unwrap();
        assert_eq!(count(&c1, "mem"), 0);
        drop(c1);

        // Re-opening a populated index must not fail or wipe it.
        let c2 = open_at(&path).unwrap();
        c2.execute(
            "INSERT INTO mem(text, role, project, session, ts, file) \
             VALUES ('hello', 'user', 'p', 's', 't', 'f')",
            [],
        )
        .unwrap();
        drop(c2);

        let c3 = open_at(&path).unwrap();
        assert_eq!(count(&c3, "mem"), 1, "reopening must preserve rows");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
