//! `cml forget` — take something out of the brain, permanently.
//!
//! Deleting the row is the easy half. The row came from a transcript still on
//! disk, so the next `cml index` would read it straight back in — which is why
//! every deletion also writes the row's [`stable_key`] into `forgotten`. That key
//! survives re-indexing; the rowid does not.

use rusqlite::Connection;

use super::{has, text};
use crate::lane::Lane;
use crate::text::{squeeze, stable_key};

/// The FTS5 query behind `--match`: whitespace-split, every token a quoted
/// phrase, all of them required.
///
/// Quoting is not decoration — an unquoted `-` or `*` is FTS5 syntax, so a user
/// typing `--match "fix --role"` would otherwise get a parse error instead of a
/// search.
fn match_query(raw: &str) -> String {
    raw.split_whitespace()
        .map(|tok| format!("\"{}\"", tok.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// Blocklist a row by its stable key, then delete it and its embedding.
/// `false` when the rowid is already gone.
pub(super) fn purge(conn: &Connection, id: i64) -> rusqlite::Result<bool> {
    // Read as Option: these are UNINDEXED FTS5 columns and a reindex leaves them
    // NULL on rows older builds wrote. `get::<String>` errors on NULL, which here
    // would mean "no such row" — `forget` would print success and delete nothing.
    let row: Option<(String, String, String, String)> = conn
        .query_row(
            "SELECT session, ts, role, substr(text,1,64) FROM mem WHERE rowid=?1",
            [id],
            |r| Ok((text(r, 0)?, text(r, 1)?, text(r, 2)?, text(r, 3)?)),
        )
        .ok();
    let Some((session, ts, role, head)) = row else {
        return Ok(false);
    };

    conn.execute(
        "INSERT OR IGNORE INTO forgotten(key) VALUES (?1)",
        [stable_key(&session, &ts, &role, &head)],
    )?;
    // The vector is dead weight the moment the row is: leaving it behind is how
    // the C++ index came to hold 5,576 embeddings for 3,826 rows. `emb.lane`
    // holds the lane's *table* name, which is what encode.rs writes.
    conn.execute(
        "DELETE FROM emb WHERE lane=?1 AND rowid_ref=?2",
        rusqlite::params![Lane::Conversation.table(), id],
    )?;
    Ok(conn.execute("DELETE FROM mem WHERE rowid=?1", [id])? > 0)
}

/// `--match` in review mode: what would go, one row per line.
fn preview(conn: &Connection, raw: &str) -> rusqlite::Result<Vec<i64>> {
    let mut st = conn.prepare(
        "SELECT rowid, role, project, ts, substr(text,1,90) FROM mem \
         WHERE mem MATCH ?1 ORDER BY rowid",
    )?;
    let rows = st.query_map([match_query(raw)], |r| {
        Ok((r.get::<_, i64>(0)?, text(r, 1)?, text(r, 2)?, text(r, 3)?, text(r, 4)?))
    })?;

    let mut ids = Vec::new();
    for (id, role, project, ts, text) in rows.flatten() {
        let day: String = ts.chars().take(10).collect();
        // 90 characters came out of SQL, so 4 bytes each is a cap that collapses
        // the whitespace without ever clipping the preview.
        println!("{id:>7} {role:<9} {project:<14} {day} | {}", squeeze(&text, 90 * 4));
        ids.push(id);
    }
    Ok(ids)
}

pub fn forget(args: &[String]) -> crate::R<i32> {
    let conn = crate::db::open()?;

    if has(args, "--clear") {
        let n = conn.execute("DELETE FROM forgotten", [])?;
        println!(
            "blocklist cleared ({n} keys) — run `cml index --all` to resurrect those rows"
        );
        return Ok(0);
    }

    let ids = match args.iter().position(|a| a == "--match") {
        Some(at) => {
            let Some(raw) = args.get(at + 1) else {
                eprintln!("cml: usage: cml forget --match \"<query>\" [--yes]");
                return Ok(1);
            };
            let ids = preview(&conn, raw)?;
            if ids.is_empty() {
                println!("no rows match: {raw}");
                return Ok(0);
            }
            if !has(args, "--yes") {
                println!("---\n{} row(s) matched — re-run with --yes to forget them", ids.len());
                return Ok(0);
            }
            ids
        }
        // Bare rowids. Anything unparsable is a flag or junk and is skipped, so
        // `cml forget 41 --yes` means the same thing as `cml forget 41`.
        None => args.iter().filter_map(|a| a.parse::<i64>().ok()).collect(),
    };

    if ids.is_empty() {
        eprintln!(
            "cml: usage: cml forget <rowid...> | --match \"<query>\" [--yes] | --clear"
        );
        return Ok(1);
    }

    let mut n = 0;
    for id in ids {
        if purge(&conn, id)? {
            n += 1;
        }
    }
    println!(
        "forgot {n} row(s) — blocklisted so they never come back (undo: cml forget --clear)"
    );
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_query_quotes_every_token() {
        assert_eq!(match_query("fix the --role"), r#""fix" AND "the" AND "--role""#);
        assert_eq!(match_query(r#"say "hi""#), r#""say" AND """hi""""#);
    }

    /// The round trip that matters: the key `forget` writes must be the key the
    /// indexer computes for that same row, or the transcript walks it back in.
    #[test]
    fn purge_blocklists_the_indexers_key_and_drops_the_vector() {
        let (dir, conn) = super::super::scratch_db("forget");
        let text = "café ".repeat(40); // multibyte, and longer than the 64-char head
        conn.execute(
            "INSERT INTO mem(text, asks, role, project, session, ts, file) \
             VALUES (?1, '', 'user', 'p', 'sess-1', '2026-08-10 10:00', 'f')",
            [&text],
        )
        .unwrap();
        let id: i64 = conn.query_row("SELECT rowid FROM mem", [], |r| r.get(0)).unwrap();
        conn.execute(
            "INSERT INTO emb(lane, rowid_ref, dim, v) VALUES ('mem', ?1, 2, x'0000')",
            [id],
        )
        .unwrap();

        assert!(purge(&conn, id).unwrap());
        assert!(!purge(&conn, id).unwrap(), "a second purge has nothing to do");

        let key: String =
            conn.query_row("SELECT key FROM forgotten", [], |r| r.get(0)).unwrap();
        assert_eq!(key, stable_key("sess-1", "2026-08-10 10:00", "user", &text));
        assert_eq!(crate::db::count(&conn, "mem"), 0);
        assert_eq!(crate::db::count(&conn, "emb"), 0, "the vector goes with the row");
        assert_eq!(crate::db::count(&conn, "forgotten"), 1);

        let _ = std::fs::remove_dir_all(dir);
    }
}
