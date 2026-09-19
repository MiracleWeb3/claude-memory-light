//! Foreign sessions -> rows in the same two lanes.
//!
//! `transcripts.rs` is the Claude Code path and stays exactly as it was. This is
//! the same judgement applied to a [`crate::harness::Session`]: the identical
//! signal floors, the identical dedup key, the identical two lanes — because a
//! search result must not be able to tell which harness it came from except by
//! the `origin` column that says so on purpose.
//!
//! One difference, and it is deliberate: there is no `turn_final` narration
//! filter here. That rule exists because Claude Code writes each content block
//! as its own line, so an assistant "doing X now" is a separate entry. The other
//! harnesses group a turn into one message, so the equivalent filter would drop
//! real answers.

use rusqlite::Connection;

use super::files::{self, Counts, Known};
use crate::harness::{Harness, Role, Session};

/// Same floors as `transcripts.rs`. Duplicated as `use`, not as literals, so the
/// two lanes cannot drift into judging the same text differently.
use super::transcripts::{ASSISTANT_MIN_CHARS, MIN_CHARS, TOOL_MIN, USER_MIN_WORDS};

/// Index every detected harness, or just the named one.
pub fn index(
    conn: &mut Connection,
    known: &Known,
    force: bool,
    budget: &super::Budget,
    only: Option<Harness>,
) -> crate::R<Counts> {
    let mut total = Counts::default();
    let list: Vec<Harness> = match only {
        Some(h) => vec![h],
        None => crate::harness::detected(),
    };
    for h in list {
        if budget.spent() {
            break;
        }
        for session in h.sessions() {
            if budget.spent() {
                break;
            }
            let meta = files::Meta { size: session.size, mtime: session.mtime };
            if !force && !files::is_stale(known, &session.file, &meta) {
                continue;
            }
            total += write(conn, &session, &meta)?;
        }
    }
    Ok(total)
}

/// One session, in one transaction: drop what this file wrote before, write what
/// it says now. The same delete-then-insert `transcripts.rs` relies on, and for
/// the same reason — without it a second run doubles every row.
fn write(conn: &mut Connection, s: &Session, meta: &files::Meta) -> crate::R<Counts> {
    let project = s.project();
    let mut count = Counts::default();
    let tx = conn.transaction()?;
    files::drop_rows_for_file(&tx, &s.file)?;
    tx.execute("DELETE FROM origin WHERE file = ?1", [&s.file])?;

    {
        let mut mem = tx.prepare(
            "INSERT INTO mem(text, role, project, session, ts, file) VALUES(?1,?2,?3,?4,?5,?6)",
        )?;
        let mut work = tx.prepare(
            "INSERT INTO work(text, role, project, session, ts, file) VALUES(?1,?2,?3,?4,?5,?6)",
        )?;
        let mut blocked = tx.prepare("SELECT 1 FROM forgotten WHERE key = ?1")?;

        for t in &s.turns {
            let role = t.role.as_str();
            if t.role == Role::Tool {
                if t.text.len() >= TOOL_MIN {
                    work.execute((&t.text, role, &project, &s.id, &t.ts, &s.file))?;
                    count.rows += 1;
                }
                continue;
            }
            if crate::text::is_noise(&t.text) {
                continue;
            }
            let len = t.text.trim().chars().count();
            if len < MIN_CHARS || (t.role == Role::Assistant && len < ASSISTANT_MIN_CHARS) {
                continue;
            }
            if t.role == Role::User && t.text.split_whitespace().count() < USER_MIN_WORDS {
                continue;
            }
            // `forget` is cross-harness: a row the user buried in one harness
            // must not walk back in through another.
            let key = crate::text::stable_key(&s.id, &t.ts, role, &t.text);
            if blocked.exists([&key])? {
                continue;
            }
            mem.execute((&t.text, role, &project, &s.id, &t.ts, &s.file))?;
            count.rows += 1;
        }
    }

    tx.execute(
        "INSERT INTO origin(file, harness, peer) VALUES(?1, ?2, NULL) \
         ON CONFLICT(file) DO UPDATE SET harness = ?2",
        (&s.file, s.harness.id()),
    )?;
    files::upsert_file(&tx, &s.file, meta)?;
    tx.commit()?;
    count.files += 1;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::Turn;

    fn session(id: &str, turns: Vec<Turn>) -> Session {
        Session {
            harness: Harness::Jcode,
            id: id.into(),
            cwd: "/home/u/dev/app".into(),
            file: format!("/sessions/{id}.json"),
            size: 1,
            mtime: 2,
            turns,
        }
    }

    fn turn(role: Role, text: &str) -> Turn {
        Turn { role, text: text.into(), ts: "2026-08-19T15:03:34Z".into() }
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::db::ensure_schema(&c).unwrap();
        c
    }

    #[test]
    fn rows_land_in_both_lanes_and_carry_their_origin() {
        let mut c = db();
        let s = session(
            "s1",
            vec![
                turn(Role::User, "why does the parser drop the last row"),
                turn(Role::Assistant, &"because the budget expired mid-batch and the file stayed stale".repeat(2)),
                turn(Role::Tool, "Bash cargo test --release -- --nocapture parser::tests"),
                turn(Role::User, "ok"), // under the word floor
            ],
        );
        let n = write(&mut c, &s, &files::Meta { size: 1, mtime: 2 }).unwrap();
        assert_eq!(n.files, 1);
        assert_eq!(crate::db::count(&c, "mem"), 2, "the bare ack must not be a memory");
        assert_eq!(crate::db::count(&c, "work"), 1);
        assert_eq!(n.rows, 3);

        let h: String = c
            .query_row("SELECT harness FROM origin WHERE file = ?1", [&s.file], |r| r.get(0))
            .unwrap();
        assert_eq!(h, "jcode");
    }

    /// The label must be the one `--project` already filters on, computed by the
    /// same function Claude rows go through — not a second spelling of it.
    #[test]
    fn the_project_label_is_the_one_claude_rows_use() {
        let home = crate::paths::home_dir();
        let cwd = home.join("dev").join("app");
        let mut s = session("s1", vec![turn(Role::User, "why does the parser drop the last row")]);
        s.cwd = cwd.to_string_lossy().into_owned();

        let mut c = db();
        write(&mut c, &s, &files::Meta { size: 1, mtime: 2 }).unwrap();
        let project: String = c
            .query_row("SELECT project FROM mem LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(project, "dev-app");
        assert_eq!(project, crate::paths::label_for_cwd(&s.cwd));
    }

    /// The bug that doubles a 208 MB index: re-indexing a changed session must
    /// replace its rows, not add a second copy.
    #[test]
    fn reindexing_a_session_replaces_rather_than_doubles() {
        let mut c = db();
        let s = session("s1", vec![turn(Role::User, "why does the parser drop the last row")]);
        let meta = files::Meta { size: 1, mtime: 2 };
        write(&mut c, &s, &meta).unwrap();
        write(&mut c, &s, &meta).unwrap();
        assert_eq!(crate::db::count(&c, "mem"), 1);
        assert_eq!(crate::db::count(&c, "origin"), 1);
    }

    #[test]
    fn a_forgotten_row_stays_forgotten_across_harnesses() {
        let mut c = db();
        let text = "why does the parser drop the last row";
        let key = crate::text::stable_key("s1", "2026-08-19T15:03:34Z", "user", text);
        c.execute("INSERT INTO forgotten(key) VALUES(?1)", [&key]).unwrap();

        write(&mut c, &session("s1", vec![turn(Role::User, text)]), &files::Meta { size: 1, mtime: 2 })
            .unwrap();
        assert_eq!(crate::db::count(&c, "mem"), 0, "forget must survive the harness it was used in");
    }
}
