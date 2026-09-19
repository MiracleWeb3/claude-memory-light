//! opencode: one SQLite database, three tables deep.
//!
//! Unlike every other harness here, opencode does not keep a file per session.
//! History is `session` -> `message` -> `part`, and the text lives two joins
//! down in `part.data` as JSON. Confirmed against a live 48 MB store: 28
//! sessions, 922 messages, 3,895 parts.
//!
//! # Read-only, and copied when busy
//!
//! The database is open in the user's editor while this runs. It is opened
//! `mode=ro` with `immutable=0` so WAL readers work, and never written: a memory
//! tool that takes a lock the editor wants is a memory tool that hangs the
//! editor.

use std::path::PathBuf;

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use super::{Harness, Role, Session, Turn};

/// `$OPENCODE_DB`, else the XDG data dir. macOS puts it under Application
/// Support, which `XDG_DATA_HOME` does not cover, so that path is tried too.
pub fn root() -> PathBuf {
    if let Some(p) = std::env::var_os("OPENCODE_DB") {
        return PathBuf::from(p);
    }
    let home = crate::paths::home_dir();
    let xdg = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local").join("share"));
    let linux = xdg.join("opencode").join("opencode.db");
    if linux.is_file() {
        return linux;
    }
    let mac = home
        .join("Library")
        .join("Application Support")
        .join("opencode")
        .join("opencode.db");
    if mac.is_file() {
        return mac;
    }
    linux
}

pub fn sessions() -> Vec<Session> {
    let path = root();
    let Some(meta) = crate::index::files::meta_of(&path) else {
        return Vec::new();
    };
    // URI form, read-only. A missing file is not an error worth a message: the
    // caller already asked `detect()`.
    let uri = format!("file:{}?mode=ro", path.to_string_lossy());
    let Ok(conn) = Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    ) else {
        return Vec::new();
    };
    let file = path.to_string_lossy().into_owned();
    read(&conn, &file, meta.size, meta.mtime).unwrap_or_default()
}

/// The join, in one query rather than one per session.
///
/// `ORDER BY m.time_created, p.id` is the transcript order: parts carry
/// monotonic ids, so within a message they already sort into the order they
/// were emitted.
fn read(conn: &Connection, db: &str, size: i64, mtime: i64) -> rusqlite::Result<Vec<Session>> {
    let mut st = conn.prepare(
        "SELECT m.session_id, s.directory, m.data, p.data, m.time_created \
         FROM message m \
         JOIN part p ON p.message_id = m.id \
         LEFT JOIN session s ON s.id = m.session_id \
         ORDER BY m.session_id, m.time_created, p.id",
    )?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?.unwrap_or_default(),
            r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            r.get::<_, Option<i64>>(4)?.unwrap_or_default(),
        ))
    })?;

    let mut out: Vec<Session> = Vec::new();
    for (sid, dir, mdata, pdata, created) in rows.flatten() {
        let Some(turn) = turn_of(&mdata, &pdata, created) else {
            continue;
        };
        // The query is ordered by session, so a new id can only mean a new
        // session: no map, no second pass.
        match out.last_mut() {
            Some(s) if s.id == sid => s.turns.push(turn),
            _ => out.push(Session {
                harness: Harness::Opencode,
                // One session per row-group, but they share a database file.
                // Appending the id keeps `files` staleness per session, so one
                // changed conversation does not rewrite all 28.
                file: format!("{db}#{sid}"),
                id: sid,
                cwd: dir,
                size,
                mtime,
                turns: vec![turn],
            }),
        }
    }
    Ok(out)
}

/// One `part` row -> at most one turn.
fn turn_of(mdata: &str, pdata: &str, created: i64) -> Option<Turn> {
    let p: Value = serde_json::from_str(pdata).ok()?;
    let role = match serde_json::from_str::<Value>(mdata)
        .ok()?
        .get("role")?
        .as_str()?
    {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => return None,
    };
    let ts = super::ts_from_millis(created);

    match p.get("type").and_then(Value::as_str)? {
        // `reasoning` is the model thinking to itself. It is not what anyone
        // searches for later, and it is the single largest part type in the
        // store (732 of 3,895), so it stays out of the index.
        "text" => {
            let text = p.get("text").and_then(Value::as_str)?.to_string();
            (!text.trim().is_empty()).then_some(Turn { role, text, ts })
        }
        "tool" => {
            let mut out = String::new();
            if let Some(name) = p.get("tool").and_then(Value::as_str) {
                out.push_str(name);
            }
            if let Some(state) = p.get("state") {
                for key in ["input", "output"] {
                    match state.get(key) {
                        Some(Value::String(s)) => push(&mut out, s),
                        Some(Value::Object(o)) => {
                            for v in o.values().filter_map(Value::as_str) {
                                push(&mut out, v);
                            }
                        }
                        _ => {}
                    }
                }
            }
            (!out.trim().is_empty()).then_some(Turn { role: Role::Tool, text: out, ts })
        }
        _ => None,
    }
}

/// The ceiling `index::entry` applies to a Claude tool row, applied here too.
const TOOL_MAX: usize = 1200;

fn push(out: &mut String, s: &str) {
    if out.len() >= TOOL_MAX || s.is_empty() {
        return;
    }
    if !out.is_empty() {
        out.push(' ');
    }
    let room = TOOL_MAX.saturating_sub(out.len()).min(s.len());
    let mut end = room;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    out.push_str(&s[..end]);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture with opencode's real column names and JSON shapes.
    fn fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE session(id TEXT, directory TEXT);
             CREATE TABLE message(id TEXT, session_id TEXT, time_created INTEGER, data TEXT);
             CREATE TABLE part(id TEXT, message_id TEXT, data TEXT);
             INSERT INTO session VALUES('ses_1', '/home/u/dev/app');
             INSERT INTO message VALUES('msg_1','ses_1',1754000000000,'{\"role\":\"user\"}');
             INSERT INTO message VALUES('msg_2','ses_1',1754000001000,'{\"role\":\"assistant\"}');
             INSERT INTO part VALUES('prt_1','msg_1','{\"type\":\"text\",\"text\":\"why is it slow\"}');
             INSERT INTO part VALUES('prt_2','msg_2','{\"type\":\"reasoning\",\"text\":\"hmm\"}');
             INSERT INTO part VALUES('prt_3','msg_2','{\"type\":\"text\",\"text\":\"the index was cold\"}');
             INSERT INTO part VALUES('prt_4','msg_2','{\"type\":\"tool\",\"tool\":\"bash\",\
                \"state\":{\"input\":{\"command\":\"cargo bench\"},\"output\":\"42ms\"}}');",
        )
        .unwrap();
        conn
    }

    #[test]
    fn the_join_reconstructs_a_session_in_order() {
        let s = read(&fixture(), "/db", 1, 2).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].id, "ses_1");
        assert_eq!(s[0].cwd, "/home/u/dev/app");
        assert_eq!(s[0].file, "/db#ses_1", "per-session staleness key");

        let kinds: Vec<_> = s[0].turns.iter().map(|t| (t.role, t.text.as_str())).collect();
        assert_eq!(
            kinds,
            vec![
                (Role::User, "why is it slow"),
                (Role::Assistant, "the index was cold"),
                (Role::Tool, "bash cargo bench 42ms"),
            ],
            "reasoning parts must not reach the index"
        );
        assert_eq!(s[0].turns[0].ts, "2025-07-31T22:13:20Z");
    }

    #[test]
    fn a_session_row_missing_from_the_join_still_indexes() {
        let conn = fixture();
        conn.execute("DELETE FROM session", []).unwrap();
        let s = read(&conn, "/db", 1, 2).unwrap();
        assert_eq!(s.len(), 1, "a LEFT JOIN, not an inner one");
        assert_eq!(s[0].cwd, "");
        assert_eq!(s[0].project(), "misc");
    }

    #[test]
    fn a_tool_row_is_capped_on_a_character_boundary() {
        let mut out = String::new();
        push(&mut out, &"\u{e9}".repeat(4000));
        assert!(out.len() <= TOOL_MAX);
        assert!(out.chars().all(|c| c == '\u{e9}'));
    }
}
