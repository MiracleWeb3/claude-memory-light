//! Session transcripts -> searchable rows.
//!
//! The half of indexing with judgement in it: which entries deserve a row at all,
//! and which of the two lanes they land in. Notes are whole-file and trivial by
//! comparison, so they live next door.
//!
//! # Subagent transcripts
//!
//! The C++ scanner read `<project>/*.jsonl` and nothing else, with the comment
//! "top-level session files only; subagent transcripts are skipped". On this machine
//! that skipped 786 of 999 files: every parallel agent's work was absent from memory,
//! which is precisely the work nobody can reconstruct from their own head later.
//!
//! The walk is now recursive, so `<session>/subagents/*.jsonl` and
//! `<session>/subagents/workflows/<wf>/*.jsonl` are indexed too. Two things make
//! those rows usable rather than merely present:
//!
//! * **Attribution.** A subagent entry's own `sessionId` *is* the parent session id,
//!   so the `session` column already traces home with no bookkeeping; `file` names
//!   the agent that produced it. When an entry has no `sessionId`, the fallback is
//!   the session *directory* holding the subagent file — the same value, never the
//!   `agent-a74eda25051eef5ba` filename, which would trace to nothing.
//! * **Volume.** The 40-byte floor on tool rows is what stops several hundred agent
//!   transcripts from burying the conversation they belong to.
//!
//! Deleting the directory restriction alone would have changed nothing: every line
//! of a subagent transcript carries `isSidechain: true`, which the parser dropped.
//! See `entry.rs`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use rusqlite::Connection;

use super::entry::{self, Kind};
use super::files::{self, Counts, Known, Meta};
use super::Budget;

/// Signal floors. Short assistant rows are mode-acks; short user rows ("please",
/// "ok done", "522556") are never how anyone finds anything, and the reply that
/// follows carries the answer and is kept anyway.
/// ponytail: word count, not semantics — drop to 2 if real messages start vanishing.
///
/// `pub(super)` so `foreign.rs` judges a jcode or opencode row by the identical
/// floors. Two copies of these numbers is how one lane silently starts keeping
/// text the other throws away.
pub(super) const USER_MIN_WORDS: usize = 4;
pub(super) const ASSISTANT_MIN_CHARS: usize = 80;
pub(super) const MIN_CHARS: usize = 4;
/// Bytes, matching the C++ `std::string::size()` this floor was calibrated against.
pub(super) const TOOL_MIN: usize = 40;

/// Files per transaction. Parsing is parallel and writing is not, so this is the
/// unit of resumability: a budget that runs out costs at most this many files of
/// re-parsing next turn.
/// ponytail: 8 keeps peak memory near one batch of parsed rows; raise it if the
/// commit rate ever shows up in a profile.
const BATCH: usize = 8;

struct Job {
    path: PathBuf,
    /// `path` as the string the `files` table and the `file` column store.
    file: String,
    project: String,
    /// Session id to fall back on when an entry does not carry its own.
    session: String,
    meta: Meta,
}

/// A row on its way into one of the two lanes. `key` is `Some` for `mem` rows: it is
/// the dedup identity, and `work` rows do not have one (they are cleared per file).
struct Row {
    text: String,
    role: &'static str,
    session: String,
    ts: String,
    key: Option<String>,
}

pub fn index(
    conn: &mut Connection,
    root: &Path,
    known: &Known,
    force: bool,
    budget: &Budget,
) -> crate::R<Counts> {
    let jobs = jobs(root, known, force);
    let mut total = Counts::default();
    if jobs.is_empty() {
        return Ok(total);
    }

    // Continued sessions leave overlapping rows across two transcript files, so new
    // rows are deduped against everything already indexed — minus the files about to
    // be deleted and rewritten, whose own keys would otherwise block their re-insert.
    let stale: HashSet<&str> = jobs.iter().map(|j| j.file.as_str()).collect();
    let mut seen = indexed_keys(conn, &stale);
    let blocked = blocklist(conn);

    for batch in jobs.chunks(BATCH) {
        if budget.spent() {
            break;
        }
        let parsed: Vec<Vec<Row>> = batch.par_iter().map(rows_of).collect();

        let tx = conn.transaction()?;
        {
            let mut mem = tx.prepare(
                "INSERT INTO mem(text, asks, role, project, session, ts, file) \
                 VALUES(?1,?2,?3,?4,?5,?6,?7)",
            )?;
            let mut work = tx.prepare(
                "INSERT INTO work(text, role, project, session, ts, file) VALUES(?1,?2,?3,?4,?5,?6)",
            )?;
            for (job, rows) in batch.iter().zip(parsed) {
                // Read before the delete: the re-insert is the only moment the
                // curated expansions can be put back on the rows they describe.
                let curated = curated_asks(&tx, &job.file);
                files::drop_rows_for_file(&tx, &job.file)?;
                for row in rows {
                    match row.key {
                        Some(key) => {
                            if blocked.contains(&key) || seen.contains(&key) {
                                continue;
                            }
                            mem.execute((
                                &row.text,
                                curated.get(&key),
                                row.role,
                                &job.project,
                                &row.session,
                                &row.ts,
                                &job.file,
                            ))?;
                            seen.insert(key);
                        }
                        None => {
                            work.execute((
                                &row.text,
                                row.role,
                                &job.project,
                                &row.session,
                                &row.ts,
                                &job.file,
                            ))?;
                        }
                    }
                    total.rows += 1;
                }
                files::upsert_file(&tx, &job.file, &job.meta)?;
                total.files += 1;
            }
        }
        tx.commit()?;
    }
    Ok(total)
}

/// Every transcript that needs (re)reading, deepest included.
fn jobs(root: &Path, known: &Known, force: bool) -> Vec<Job> {
    let mut out = Vec::new();
    for (project_dir, project) in super::project_dirs(root) {
        // A `.jsonl` sitting in the project directory is a whole session and names
        // itself. Anything deeper belongs to the session directory above it, however
        // many levels down `subagents/workflows/<wf-id>/` goes.
        let mut stack = vec![(project_dir, None::<String>)];
        while let Some((dir, session)) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                let path = e.path();
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    // The session directory names itself; everything below inherits it.
                    let inherited = session.clone().unwrap_or(name);
                    stack.push((path, Some(inherited)));
                    continue;
                }
                if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                    continue;
                }
                let Some(meta) = files::meta_of(&path) else {
                    continue;
                };
                let file = path.to_string_lossy().into_owned();
                if !force && !files::is_stale(known, &file, &meta) {
                    continue;
                }
                let session = session.clone().unwrap_or_else(|| {
                    path.file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default()
                });
                out.push(Job { path, file, project: project.clone(), session, meta });
            }
        }
    }
    out
}

/// The pure half: read a file, judge its entries, hand back rows. No database, no
/// shared state, which is what lets `BATCH` files run through it at once.
fn rows_of(job: &Job) -> Vec<Row> {
    let entries = entry::parse(&job.path, &job.session);
    let mut out = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let role = match e.kind {
            Kind::Summary => "summary",
            Kind::UserHuman => "user",
            Kind::AssistantText if entry::turn_final(&entries, i) => "assistant",
            Kind::AssistantText => continue,
            // The work itself: commands and what they printed. Short results are
            // acknowledgements ("OK", "1 file changed") carrying no search surface,
            // so they go the way empty text does.
            Kind::AssistantTool | Kind::UserTool => {
                if e.text.len() >= TOOL_MIN {
                    out.push(Row {
                        text: e.text.clone(),
                        role: "tool",
                        session: e.session.clone(),
                        ts: e.ts.clone(),
                        key: None,
                    });
                }
                continue;
            }
        };

        if crate::text::is_noise(&e.text) {
            continue;
        }
        // Characters, not bytes: the dedup key is built from SQLite's substr(), which
        // counts characters, and a floor that disagreed would judge a different text.
        let len = e.text.trim().chars().count();
        if len < MIN_CHARS || (e.kind == Kind::AssistantText && len < ASSISTANT_MIN_CHARS) {
            continue;
        }
        if e.kind == Kind::UserHuman && e.text.split_whitespace().count() < USER_MIN_WORDS {
            continue;
        }

        out.push(Row {
            key: Some(crate::text::stable_key(&e.session, &e.ts, role, &e.text)),
            text: e.text.clone(),
            role,
            session: e.session.clone(),
            ts: e.ts.clone(),
        });
    }
    out
}

/// NULL reads as "" rather than an error: these UNINDEXED FTS5 columns are older
/// than any guarantee about them, and `stable_key` has to see the same "" the C++
/// tree saw when it wrote the keys now sitting in `distilled` and `forgotten`.
fn col(r: &rusqlite::Row, i: usize) -> rusqlite::Result<String> {
    Ok(r.get::<_, Option<String>>(i)?.unwrap_or_default())
}

/// `substr(text, 1, 64)` counts characters in SQLite, and `crate::text::stable_key`
/// clips to 64 characters internally — so handing it the clipped head gives the same
/// key as handing it the full text.
fn key_of(r: &rusqlite::Row) -> rusqlite::Result<String> {
    Ok(crate::text::stable_key(
        &col(r, 0)?,
        &col(r, 1)?,
        &col(r, 2)?,
        &col(r, 3)?,
    ))
}

fn indexed_keys(conn: &Connection, stale: &HashSet<&str>) -> HashSet<String> {
    let mut out = HashSet::new();
    let Ok(mut st) = conn.prepare("SELECT session, ts, role, substr(text, 1, 64), file FROM mem")
    else {
        return out;
    };
    let rows = st.query_map([], |r| Ok((key_of(r)?, col(r, 4)?)));
    if let Ok(rows) = rows {
        for (key, file) in rows.flatten() {
            if !stale.contains(file.as_str()) {
                out.insert(key);
            }
        }
    }
    out
}

/// The doc2query expansions a curator was paid to write, keyed so they survive the
/// row they are attached to.
///
/// Without this, one extra turn in a live session destroys every `asks` on its
/// transcript: the file looks changed, its rows are deleted, and the re-insert never
/// mentioned the column. `distilled` still holds the gist under the same key, so the
/// curator's worklist skips those rows forever and nothing ever regenerates them.
/// Measured on the live index before the fix: 267 of 297 curated rows, 90%, already
/// stripped. The column reads back NULL rather than "", which is why it went
/// unnoticed — an `asks != ''` probe cannot see it.
fn curated_asks(conn: &Connection, file: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(mut st) = conn.prepare(
        "SELECT session, ts, role, substr(text, 1, 64), asks FROM mem \
         WHERE file = ?1 AND asks IS NOT NULL AND asks != ''",
    ) else {
        return out;
    };
    let rows = st.query_map([file], |r| Ok((key_of(r)?, col(r, 4)?)));
    if let Ok(rows) = rows {
        out.extend(rows.flatten());
    }
    out
}

/// Keys `forget` and `distill` have retired. Re-indexing must not walk them back in.
fn blocklist(conn: &Connection) -> HashSet<String> {
    let Ok(mut st) = conn.prepare("SELECT key FROM forgotten") else {
        return HashSet::new();
    };
    let Ok(rows) = st.query_map([], |r| r.get::<_, String>(0)) else {
        return HashSet::new();
    };
    rows.flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    const PARENT: &str = "11111111-2222-3333-4444-555555555555";
    const ANSWER: &str = "the walk is recursive now, so subagent transcripts land in the same \
                          two lanes as the session that spawned them";

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cml-idx-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A project with one session and one subagent transcript under it, plus the
    /// `.omc` telemetry directory that shares the tree but is not conversation.
    fn fixture(root: &Path) {
        let project = root.join("-home-x-proj");
        let subagents = project.join(PARENT).join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();

        let session = [
            format!(
                r#"{{"type":"user","sessionId":"{PARENT}","timestamp":"2026-08-10T10:00:00Z","message":{{"content":"port the indexer to rust and keep every subagent transcript"}}}}"#
            ),
            format!(
                r#"{{"type":"assistant","sessionId":"{PARENT}","timestamp":"2026-08-10T10:00:01Z","message":{{"content":[{{"type":"tool_use","name":"Bash","input":{{"command":"find ~/.claude/projects -name '*.jsonl' | wc -l"}}}}]}}}}"#
            ),
            // 3 bytes of output: an acknowledgement, no search surface.
            format!(
                r#"{{"type":"user","sessionId":"{PARENT}","timestamp":"2026-08-10T10:00:02Z","message":{{"content":[{{"type":"tool_result","content":"999"}}]}}}}"#
            ),
            format!(
                r#"{{"type":"user","sessionId":"{PARENT}","timestamp":"2026-08-10T10:00:03Z","message":{{"content":[{{"type":"tool_result","content":"999 transcript files, 786 of them under subagents/ and invisible to memory"}}]}}}}"#
            ),
            format!(
                r#"{{"type":"assistant","sessionId":"{PARENT}","timestamp":"2026-08-10T10:00:04Z","message":{{"content":[{{"type":"text","text":"{ANSWER}"}}]}}}}"#
            ),
        ];
        std::fs::write(project.join(format!("{PARENT}.jsonl")), session.join("\n") + "\n").unwrap();

        // Every line of a subagent transcript carries isSidechain, which is what the
        // C++ parser dropped on the floor.
        let agent = [
            format!(
                r#"{{"type":"user","isSidechain":true,"sessionId":"{PARENT}","timestamp":"2026-08-10T10:01:00Z","message":{{"content":"walk the subagent directories recursively and report what you find"}}}}"#
            ),
            format!(
                r#"{{"type":"assistant","isSidechain":true,"sessionId":"{PARENT}","timestamp":"2026-08-10T10:01:01Z","message":{{"content":[{{"type":"text","text":"{ANSWER}"}}]}}}}"#
            ),
        ];
        std::fs::write(subagents.join("agent-abc123.jsonl"), agent.join("\n") + "\n").unwrap();

        std::fs::create_dir_all(root.join(".omc/state")).unwrap();
        std::fs::write(
            root.join(".omc/state/agent-replay-1.jsonl"),
            "{\"event\":\"agent_stop\",\"success\":true}\n",
        )
        .unwrap();
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn subagent_transcripts_are_indexed_and_trace_to_their_parent_session() {
        let dir = scratch("subagent");
        let root = dir.join("projects");
        fixture(&root);
        let mut conn = db::open_at(&dir.join("index.db")).unwrap();

        let known = files::known(&conn);
        let c = index(&mut conn, &root, &known, false, &Budget::new(0)).unwrap();
        assert_eq!(c.files, 2, "both the session and its subagent file must be read");

        let sub = count(&conn, "SELECT count(*) FROM mem WHERE file LIKE '%subagents%'");
        assert!(sub >= 2, "subagent rows missing entirely, got {sub}");

        let stray: i64 = count(
            &conn,
            &format!("SELECT count(*) FROM mem WHERE file LIKE '%subagents%' AND session != '{PARENT}'"),
        );
        assert_eq!(stray, 0, "a subagent row must trace back to the parent session");

        // The agent file still has to be distinguishable from the parent's own rows.
        assert!(count(&conn, "SELECT count(*) FROM mem WHERE file LIKE '%agent-abc123%'") >= 2);
        assert_eq!(
            count(&conn, "SELECT count(*) FROM files WHERE path LIKE '%.omc%'"),
            0,
            "telemetry under a dot-directory is not a transcript"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reindexing_adds_no_duplicate_rows() {
        let dir = scratch("dedup");
        let root = dir.join("projects");
        fixture(&root);
        let mut conn = db::open_at(&dir.join("index.db")).unwrap();
        let budget = Budget::new(0);

        let known = files::known(&conn);
        index(&mut conn, &root, &known, false, &budget).unwrap();
        let (mem, work) = (
            count(&conn, "SELECT count(*) FROM mem"),
            count(&conn, "SELECT count(*) FROM work"),
        );
        assert!(mem > 0 && work > 0);

        // Unchanged files are skipped outright.
        let known = files::known(&conn);
        let again = index(&mut conn, &root, &known, false, &budget).unwrap();
        assert_eq!(again.files, 0);
        assert_eq!(count(&conn, "SELECT count(*) FROM mem"), mem);

        // And a forced re-read replaces its rows rather than appending them, which is
        // the failure that would silently double a 148 MB index.
        let known = files::known(&conn);
        let forced = index(&mut conn, &root, &known, true, &budget).unwrap();
        assert_eq!(forced.files, 2);
        assert_eq!(count(&conn, "SELECT count(*) FROM mem"), mem, "mem rows duplicated");
        assert_eq!(count(&conn, "SELECT count(*) FROM work"), work, "work rows duplicated");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The property `forget`, `distill` and this module must all agree on. SQLite's
    /// `substr()` counts CHARACTERS, so a key clipped by BYTES would miss every key
    /// already in `distilled` whose row contains non-ASCII — and the miss is silent:
    /// blocklisting stops working and curated asks quietly fail to re-attach.
    #[test]
    fn stable_key_agrees_with_sqlite_substr() {
        let dir = scratch("key");
        let conn = db::open_at(&dir.join("index.db")).unwrap();
        let text = "\u{e9}".repeat(100);
        conn.execute(
            "INSERT INTO mem(text, role, project, session, ts, file) \
             VALUES(?1, 'user', 'p', 's', 't', 'f')",
            [&text],
        )
        .unwrap();
        let head: String = conn
            .query_row("SELECT substr(text, 1, 64) FROM mem", [], |r| r.get(0))
            .unwrap();
        assert_eq!(head.chars().count(), 64);
        assert_eq!(
            crate::text::stable_key("s", "t", "user", &text),
            format!("s|t|user|{head}")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A live session grows every turn, so its transcript is re-read constantly.
    /// Each of those re-reads used to wipe the curator's paid output.
    #[test]
    fn reindexing_preserves_curated_asks() {
        let dir = scratch("asks");
        let root = dir.join("projects");
        fixture(&root);
        let mut conn = db::open_at(&dir.join("index.db")).unwrap();
        let budget = Budget::new(0);

        let known = files::known(&conn);
        index(&mut conn, &root, &known, false, &budget).unwrap();
        conn.execute(
            "UPDATE mem SET asks = 'how do I index subagent transcripts' WHERE role = 'assistant'",
            [],
        )
        .unwrap();
        let curated = count(&conn, "SELECT count(*) FROM mem WHERE asks IS NOT NULL AND asks != ''");
        assert!(curated > 0, "fixture produced no curatable rows");

        let known = files::known(&conn);
        index(&mut conn, &root, &known, true, &budget).unwrap();
        assert_eq!(
            count(&conn, "SELECT count(*) FROM mem WHERE asks IS NOT NULL AND asks != ''"),
            curated,
            "a re-index destroyed curated asks"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn short_tool_output_is_dropped() {
        let dir = scratch("toolfloor");
        let root = dir.join("projects");
        fixture(&root);
        let mut conn = db::open_at(&dir.join("index.db")).unwrap();

        let known = files::known(&conn);
        index(&mut conn, &root, &known, false, &Budget::new(0)).unwrap();

        let mut st = conn.prepare("SELECT text FROM work").unwrap();
        let texts: Vec<String> = st
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .flatten()
            .collect();
        drop(st);
        assert_eq!(texts.len(), 2, "expected the command and its long output: {texts:?}");
        assert!(texts.iter().all(|t| t.len() >= TOOL_MIN));
        assert!(!texts.iter().any(|t| t == "999"), "a 3-byte ack was indexed");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
