//! `cml chats` — the list you pick from, and the numbers you pick by.
//!
//! Sharing was gated behind knowing a session id, which is a 36-character UUID
//! nobody has ever known by heart. The honest unit of choice is "the chat where
//! I asked about X", so this prints exactly that: what you opened with, when,
//! which project, how big. Then `cml share --chat 3` shares the third one.
//!
//! # Why a number and not a name
//!
//! A name would need a naming scheme, a uniqueness rule, and a way to fix a
//! collision. A number needs a list, which is on the screen. The number is
//! positional and only valid against the list just printed, which is why
//! `share --chat N` prints the chat it resolved to before doing anything.

use rusqlite::Connection;

use crate::db;

/// One row of the picker.
#[derive(Debug)]
pub struct Chat {
    pub session: String,
    pub project: String,
    pub last: String,
    pub rows: i64,
    /// The first thing the human said, which is what makes a chat recognisable.
    pub opener: String,
    pub peer: Option<String>,
}

/// cml chats [--project P] [--limit N] [--all] [--json]
pub fn run(args: &[String]) -> crate::R<i32> {
    let conn = db::open_ro()
        .map_err(|e| format!("cannot open the index ({e}) — run `cml index` first"))?;
    let project = crate::share::flag(args, "--project");
    let limit = crate::share::flag(args, "--limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(20);
    let all = args.iter().any(|a| a == "--all");

    let chats = list(&conn, project.as_deref(), limit, all)?;

    // `--json` exists for the GUI, which needs the session id and the exact
    // ordering rather than a column layout. Built with serde_json rather than
    // string concatenation because an opener containing a quote is not an edge
    // case, it is Tuesday.
    if args.iter().any(|a| a == "--json") {
        let out: Vec<serde_json::Value> = chats
            .iter()
            .enumerate()
            .map(|(i, c)| {
                serde_json::json!({
                    "n": i + 1,
                    "session": c.session,
                    "project": c.project,
                    "last": c.last,
                    "rows": c.rows,
                    "opener": c.opener,
                    "peer": c.peer,
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(out));
        return Ok(0);
    }

    if chats.is_empty() {
        println!("no chats indexed yet — run `cml index --all`");
        return Ok(0);
    }

    println!("{:>3}  {:<10}  {:<22}  {:>5}  opened with", "#", "when", "project", "rows");
    for (i, c) in chats.iter().enumerate() {
        let who = c.peer.as_ref().map_or(String::new(), |p| format!("[{p}] "));
        println!(
            "{:>3}  {:<10}  {:<22}  {:>5}  {}{}",
            i + 1,
            c.last.get(..10).unwrap_or("-"),
            trim(&c.project, 22),
            c.rows,
            who,
            trim(&c.opener, 46)
        );
    }
    println!("\nshare one:  cml share --chat <#> --to <name>");
    println!("look first: cml share --chat <#> --dry-run");
    Ok(0)
}

/// The picker query. One row per session, newest first.
///
/// The opener is the first *user* message: an assistant's first line is usually
/// "I'll help you with that", which distinguishes nothing. Sessions whose whole
/// content is a one-line probe are dropped unless `--all`, because a list where
/// the top five entries are `Reply with exactly: OK` is a list nobody reads.
pub fn list(
    conn: &Connection,
    project: Option<&str>,
    limit: usize,
    all: bool,
) -> crate::R<Vec<Chat>> {
    // A chat with a single row is a one-line probe ("Reply with exactly: OK"),
    // not a conversation. Two rows is a real question and a real answer, and
    // dropping those was an off-by-one that hid genuine chats from the picker.
    let floor = if all { 1 } else { 2 };
    // The `origin` subquery is written only when the table exists. An index
    // built by an older cml has no `origin`, and a picker that refuses to list
    // anything because a column it invented is missing is worse than a picker
    // that shows no peer labels.
    let has_origin = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='origin'",
            [],
            |_| Ok(()),
        )
        .is_ok();
    let peer_col = if has_origin {
        "(SELECT o.peer FROM origin o WHERE o.file = m1.file)"
    } else {
        "NULL"
    };
    let mut sql = format!(
        "SELECT m1.session, m1.project, MAX(m1.ts), COUNT(*), \
         (SELECT m2.text FROM mem m2 WHERE m2.session = m1.session AND m2.role = 'user' \
          ORDER BY m2.ts LIMIT 1), \
         {peer_col} \
         FROM mem m1 WHERE m1.session != ''"
    );
    if project.is_some() {
        sql.push_str(" AND lower(m1.project) LIKE '%' || lower(?1) || '%'");
    }
    sql.push_str(" GROUP BY m1.session HAVING COUNT(*) >= ?2 ORDER BY MAX(m1.ts) DESC LIMIT ?3");

    let mut st = conn.prepare(&sql)?;
    let map = |r: &rusqlite::Row| -> rusqlite::Result<Chat> {
        Ok(Chat {
            session: text(r, 0)?,
            project: text(r, 1)?,
            last: text(r, 2)?,
            rows: r.get::<_, Option<i64>>(3)?.unwrap_or_default(),
            opener: crate::text::squeeze(&text(r, 4)?, 200),
            peer: r.get::<_, Option<String>>(5)?,
        })
    };
    let lim = i64::try_from(limit).unwrap_or(20);
    let rows = match project {
        Some(p) => st.query_map(rusqlite::params![p, floor, lim], map)?.collect(),
        None => st
            .query_map(rusqlite::params![rusqlite::types::Null, floor, lim], map)?
            .collect(),
    };
    let rows: rusqlite::Result<Vec<Chat>> = rows;
    Ok(rows?)
}

/// Resolve `--chat N` against the same list `cml chats` just printed.
///
/// The list must be built identically or the number means something else, which
/// is why both callers go through [`list`] rather than writing the query twice.
pub fn nth(conn: &Connection, n: usize, project: Option<&str>) -> crate::R<Chat> {
    // A generous ceiling: the number the user typed came off a printed list, and
    // that list may have been longer than the default.
    let chats = list(conn, project, n.max(20), false)?;
    if n == 0 || n > chats.len() {
        return Err(format!(
            "no chat #{n} — run `cml chats` to see the list ({} available)",
            chats.len()
        )
        .into());
    }
    Ok(chats.into_iter().nth(n - 1).expect("bounds checked above"))
}

/// NULL is "" here, as everywhere else in this crate: these are UNINDEXED FTS5
/// columns older than any guarantee about them.
fn text(r: &rusqlite::Row, i: usize) -> rusqlite::Result<String> {
    Ok(r.get::<_, Option<String>>(i)?.unwrap_or_default())
}

fn trim(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let head: String = s.chars().take(n.saturating_sub(1)).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        db::ensure_schema(&c).unwrap();
        let add = |s: &str, role: &str, ts: &str, text: &str, project: &str| {
            c.execute(
                "INSERT INTO mem(text, role, project, session, ts, file) VALUES(?1,?2,?3,?4,?5,?6)",
                (text, role, project, s, ts, format!("/{s}.jsonl")),
            )
            .unwrap();
        };
        add("old", "user", "2026-08-01T10:00:00Z", "why does the parser drop rows", "app");
        add("old", "assistant", "2026-08-01T10:01:00Z", "because the budget expired", "app");
        add("new", "assistant", "2026-08-09T09:00:00Z", "I'll help you with that", "web");
        add("new", "user", "2026-08-09T09:01:00Z", "the deploy keeps timing out", "web");
        add("new", "user", "2026-08-09T09:02:00Z", "still broken", "web");
        // A one-line probe: noise in a picker.
        add("probe", "user", "2026-08-10T09:00:00Z", "Reply with exactly: OK", "tmp");
        c
    }

    #[test]
    fn the_list_is_newest_first_and_opens_with_what_the_human_said() {
        let c = seeded();
        let chats = list(&c, None, 20, false).unwrap();
        assert_eq!(chats.len(), 2, "the one-line probe must not be offered");
        assert_eq!(chats[0].session, "new", "newest first");
        assert_eq!(
            chats[0].opener, "the deploy keeps timing out",
            "the opener must be the user's first line, not the assistant's"
        );
        assert_eq!(chats[0].rows, 3);
        assert_eq!(chats[1].session, "old");
    }

    #[test]
    fn all_includes_the_probes() {
        let c = seeded();
        assert_eq!(list(&c, None, 20, true).unwrap().len(), 3);
    }

    #[test]
    fn a_project_filter_narrows_the_list() {
        let c = seeded();
        let chats = list(&c, Some("app"), 20, false).unwrap();
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].session, "old");
    }

    /// The number is only meaningful against the printed list, so it must
    /// resolve to the same row the user was looking at.
    #[test]
    fn nth_matches_the_printed_order() {
        let c = seeded();
        assert_eq!(nth(&c, 1, None).unwrap().session, "new");
        assert_eq!(nth(&c, 2, None).unwrap().session, "old");
    }

    #[test]
    fn an_out_of_range_number_explains_itself() {
        let c = seeded();
        let e = nth(&c, 9, None).unwrap_err().to_string();
        assert!(e.contains("no chat #9"), "{e}");
        assert!(e.contains("cml chats"), "the error must name the fix: {e}");
        assert!(nth(&c, 0, None).is_err(), "the list is 1-based");
    }

    /// An index written by an older cml has no `origin` table, and the picker
    /// must still list every chat in it rather than failing on a missing
    /// column. This was a real crash on the 208 MB index on this machine.
    #[test]
    fn an_index_without_the_origin_table_still_lists() {
        let c = seeded();
        c.execute("DROP TABLE origin", []).unwrap();
        let chats = list(&c, None, 20, false).unwrap();
        assert_eq!(chats.len(), 2);
        assert!(chats.iter().all(|c| c.peer.is_none()));
    }
}
