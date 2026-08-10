//! What both scanners agree on: is this file new, and how do its old rows go away.
//!
//! `files(path, size, mtime)` is what makes re-indexing incremental. It is also what
//! makes it *correct*: a changed file's old rows must be deleted before the new ones
//! land, or the second run doubles every row in a 148 MB index.

use std::collections::HashMap;
use std::path::Path;
use std::time::UNIX_EPOCH;

use rusqlite::Connection;

use crate::lane::Lane;

/// Size and mtime, the two cheap facts that answer "did this change".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    pub size: i64,
    pub mtime: i64,
}

/// Everything the `files` table already knows, read in one query.
pub type Known = HashMap<String, Meta>;

#[derive(Debug, Default, Clone, Copy)]
pub struct Counts {
    pub files: usize,
    pub rows: usize,
}

impl std::ops::AddAssign for Counts {
    fn add_assign(&mut self, o: Self) {
        self.files += o.files;
        self.rows += o.rows;
    }
}

pub fn meta_of(path: &Path) -> Option<Meta> {
    let md = std::fs::metadata(path).ok()?;
    if !md.is_file() {
        return None;
    }
    let mtime = md.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(Meta {
        size: i64::try_from(md.len()).unwrap_or(i64::MAX),
        mtime: i64::try_from(mtime.as_secs()).unwrap_or(i64::MAX),
    })
}

/// One query instead of one per file. On this machine that is 503 rows read once
/// in place of 999 prepared statements.
pub fn known(conn: &Connection) -> Known {
    let mut out = Known::new();
    let Ok(mut st) = conn.prepare("SELECT path, size, mtime FROM files") else {
        return out;
    };
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            Meta {
                size: r.get::<_, Option<i64>>(1)?.unwrap_or_default(),
                mtime: r.get::<_, Option<i64>>(2)?.unwrap_or_default(),
            },
        ))
    });
    if let Ok(rows) = rows {
        out.extend(rows.flatten());
    }
    out
}

pub fn is_stale(known: &Known, path: &str, meta: &Meta) -> bool {
    known.get(path) != Some(meta)
}

/// Delete every row this file produced, embeddings included.
///
/// The embedding rows are keyed by the rowid they describe, and FTS5 hands rowids
/// back out after a delete — so leaving them behind does not merely waste space,
/// it eventually attaches a stale vector to an unrelated row.
pub fn drop_rows_for_file(conn: &Connection, path: &str) -> rusqlite::Result<()> {
    for lane in [Lane::Conversation, Lane::Tools] {
        conn.execute(
            &format!(
                "DELETE FROM emb WHERE lane = ?2 AND rowid_ref IN \
                 (SELECT rowid FROM {} WHERE file = ?1)",
                lane.table()
            ),
            (path, lane.table()),
        )?;
        conn.execute(
            &format!("DELETE FROM {} WHERE file = ?1", lane.table()),
            [path],
        )?;
    }
    Ok(())
}

pub fn upsert_file(conn: &Connection, path: &str, meta: &Meta) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO files(path, size, mtime) VALUES(?1, ?2, ?3) \
         ON CONFLICT(path) DO UPDATE SET size = ?2, mtime = ?3",
        (path, meta.size, meta.mtime),
    )?;
    Ok(())
}

// Date formatting lives in crate::paths: `iso_date` writes the `ts` of a markdown
// row, and `recall` parses that same string back.
