//! `cml share` and `cml import` — one conversation, moved between two people.
//!
//! The user story this exists for: a friend finishes a piece of work, runs one
//! command, sends you a file, you run one command, and that conversation is
//! searchable in your own history and visibly *theirs*.
//!
//! # The bundle is a SQLite file
//!
//! Not a tar of JSONL. The exporter already has SQLite linked, `ATTACH` makes
//! the import a single `INSERT ... SELECT`, and the receiving side can inspect
//! what it was sent with tools it already has before trusting it. A tar would
//! have bought a second serialisation format and a dependency, and paid for
//! neither.
//!
//! # Two things this must never do
//!
//! Leak a secret, and pollute your recall. The first is [`redact`]'s job and is
//! on by default. The second is why every imported row carries its peer in
//! `origin` and is marked in search output: a stranger's rows in your index are
//! only an improvement if you can always tell whose they are, and remove them
//! again in one command.

pub mod redact;

use rusqlite::Connection;

use crate::db;

/// The manifest lives in the bundle as a one-row table. A version, because the
/// first thing a format needs is the ability to be a later version.
const FORMAT: i64 = 1;

/// cml share [--chat N] [--session ID] [--project P] [--to NAME] [--out FILE]
///           [--dry-run] [--no-redact]
pub fn share(args: &[String]) -> crate::R<i32> {
    let mut session = flag(args, "--session");
    let project = flag(args, "--project");
    let dry = args.iter().any(|a| a == "--dry-run");
    let no_redact = args.iter().any(|a| a == "--no-redact");
    // `--to` reads better at a call site than `--peer` ("share this to sam"),
    // and `--peer` still works because it was already documented.
    let peer = flag(args, "--to")
        .or_else(|| flag(args, "--peer"))
        .unwrap_or_else(|| "me".to_string());

    // `--chat N` resolves against the list `cml chats` prints, and says out loud
    // which chat it landed on: a positional number the user cannot verify is a
    // number that eventually shares the wrong conversation.
    let mut label = String::new();
    if let Some(n) = flag(args, "--chat").and_then(|v| v.parse::<usize>().ok()) {
        let conn = db::open_ro()?;
        let chat = crate::chats::nth(&conn, n, project.as_deref())?;
        println!(
            "chat #{n}: {} · {} · {} rows\n  {}\n",
            chat.last.get(..10).unwrap_or("-"),
            chat.project,
            chat.rows,
            chat.opener
        );
        label = chat.project.clone();
        session = Some(chat.session);
    }

    if session.is_none() && project.is_none() {
        return Err("pick something to share: `cml chats` then `cml share --chat <#>`, \
                    or --session <id> / --project <name>"
            .into());
    }
    let out = flag(args, "--out").unwrap_or_else(|| {
        // A filename someone can recognise in a chat window, not a UUID: the
        // project and the date are what the receiver actually reads.
        let tag = if label.is_empty() {
            project.clone().unwrap_or_else(|| "chat".to_string())
        } else {
            label
        };
        let day = crate::paths::iso_date(crate::paths::now_secs());
        format!("{}-{day}.cmlpack", tag.replace(['/', ' ', '.'], "-"))
    });

    let conn = db::open_ro()?;
    let rows = collect(&conn, session.as_deref(), project.as_deref())?;
    if rows.is_empty() {
        return Err("nothing matched: no rows to share".into());
    }

    // Redaction is applied to the copy that leaves, never to the local index.
    let mut report = redact::Report::default();
    let mut cleaned = rows;
    if !no_redact {
        for r in &mut cleaned {
            redact::scrub(&mut r.text, &mut report);
        }
    }
    // The dedup key is minted AFTER redaction, deliberately. `stable_key`
    // embeds the first 64 characters of the row verbatim, so a key computed
    // from the original text would carry the very secret the scrub just
    // removed, in a column nobody thinks to look at. That was a real leak: the
    // end-to-end test found four synthetic secrets in a bundle whose visible
    // text was clean.
    for r in &mut cleaned {
        r.key = crate::text::stable_key(&r.session, &r.ts, &r.role, &r.text);
    }

    if dry {
        println!("would share {} row(s) as '{peer}' -> {out}", cleaned.len());
        let mut lanes = (0, 0);
        for r in &cleaned {
            if r.lane == "mem" {
                lanes.0 += 1;
            } else {
                lanes.1 += 1;
            }
        }
        println!("  lanes: {} conversation, {} tool", lanes.0, lanes.1);
        if no_redact {
            println!("  redaction: OFF (--no-redact)");
        } else {
            report.print();
        }
        println!("\nnothing was written. drop --dry-run to make the file.");
        return Ok(0);
    }

    write_bundle(&out, &cleaned, &peer)?;
    let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or_default();
    println!("shared {} row(s) -> {out}  ({})", cleaned.len(), human(size));
    if !no_redact {
        report.print();
    }
    println!("\nsend them that one file. they run:");
    println!("  cml import {out}");
    Ok(0)
}

/// Bytes as something a person reads before deciding to send it.
fn human(n: u64) -> String {
    const K: u64 = 1024;
    match n {
        n if n < K => format!("{n} B"),
        n if n < K * K => format!("{} KB", n / K),
        n => format!("{:.1} MB", n as f64 / (K * K) as f64),
    }
}

/// cml import FILE [--as NAME] [--dry-run]
pub fn import(args: &[String]) -> crate::R<i32> {
    let Some(path) = args.iter().find(|a| !a.starts_with("--")) else {
        return Err("usage: cml import <file.cmlpack> [--as NAME]".into());
    };
    if !std::path::Path::new(path).is_file() {
        return Err(format!("no such bundle: {path}").into());
    }
    let mut conn = db::open()?;
    // ATTACH and DETACH are connection-level, not transaction-level: doing them
    // inside a transaction leaves the pack locked and the DETACH fails. So the
    // bundle is attached around the transaction, not inside it.
    conn.execute("ATTACH DATABASE ?1 AS pack", [path])?;
    let result = import_attached(&mut conn, path, args);
    // Detach even when the import failed, or the next run in the same process
    // inherits a half-open bundle.
    let _ = conn.execute("DETACH DATABASE pack", []);
    result
}

fn import_attached(conn: &mut Connection, path: &str, args: &[String]) -> crate::R<i32> {
    let dry = args.iter().any(|a| a == "--dry-run");
    let (version, author, made): (i64, String, String) = conn.query_row(
        "SELECT format, peer, created FROM pack.manifest LIMIT 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if version > FORMAT {
        return Err(format!(
            "bundle format {version} is newer than this build understands ({FORMAT})"
        )
        .into());
    }
    let peer = flag(args, "--as").unwrap_or(author);
    let total: i64 = conn.query_row("SELECT count(*) FROM pack.rows", [], |r| r.get(0))?;

    if dry {
        println!("{path}: {total} row(s) from '{peer}', created {made}");
        return Ok(0);
    }

    let tx = conn.transaction()?;
    // The import is idempotent by file: re-importing the same bundle replaces
    // what it wrote last time rather than adding a second copy.
    let file = format!("cmlpack:{peer}:{}", stem(path));
    crate::index::files::drop_rows_for_file(&tx, &file)?;
    tx.execute("DELETE FROM origin WHERE file = ?1", [&file])?;

    let mut written = 0usize;
    for lane in ["mem", "work"] {
        written += tx.execute(
            &format!(
                "INSERT INTO {lane}(text, role, project, session, ts, file) \
                 SELECT text, role, project, session, ts, ?1 FROM pack.rows \
                 WHERE lane = ?2 AND key NOT IN (SELECT key FROM forgotten)"
            ),
            (&file, lane),
        )?;
    }
    tx.execute(
        "INSERT INTO origin(file, harness, peer) VALUES(?1, 'import', ?2) \
         ON CONFLICT(file) DO UPDATE SET peer = ?2",
        (&file, &peer),
    )?;
    tx.commit()?;

    println!("imported {written} row(s) from '{peer}'");
    println!("  they are searchable now, and marked [{peer}] in results");
    println!("  to undo: cml forget --from {peer}");
    Ok(0)
}

/// One row on its way out.
pub struct Row {
    pub lane: &'static str,
    pub text: String,
    pub role: String,
    pub project: String,
    pub session: String,
    pub ts: String,
    pub key: String,
}

fn collect(conn: &Connection, session: Option<&str>, project: Option<&str>) -> crate::R<Vec<Row>> {
    let mut out = Vec::new();
    for lane in ["mem", "work"] {
        let (clause, arg) = match (session, project) {
            (Some(s), _) => ("session = ?1", s),
            (_, Some(p)) => ("project = ?1", p),
            _ => unreachable!("share() rejects an unfiltered export"),
        };
        let sql = format!(
            "SELECT text, role, project, session, ts FROM {lane} WHERE {clause}"
        );
        let mut st = conn.prepare(&sql)?;
        let rows = st.query_map([arg], |r| {
            Ok(Row {
                lane: if lane == "mem" { "mem" } else { "work" },
                text: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                role: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                project: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                session: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                ts: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                // Minted by the caller, after redaction. See `share`.
                key: String::new(),
            })
        })?;
        out.extend(rows.flatten());
    }
    Ok(out)
}

fn write_bundle(path: &str, rows: &[Row], peer: &str) -> crate::R {
    let _ = std::fs::remove_file(path);
    let pack = Connection::open(path)?;
    // A bundle is a file that gets mailed. WAL would make it three files, two of
    // which the receiver would not be sent — and an ATTACH of a WAL database
    // whose writer is still open fails with "database is locked".
    pack.pragma_update(None, "journal_mode", "DELETE")?;
    pack.execute_batch(
        "CREATE TABLE manifest(format INTEGER, peer TEXT, created TEXT, n INTEGER);
         CREATE TABLE rows(lane TEXT, text TEXT, role TEXT, project TEXT,
                           session TEXT, ts TEXT, key TEXT);",
    )?;
    pack.execute(
        "INSERT INTO manifest(format, peer, created, n) VALUES(?1, ?2, ?3, ?4)",
        (
            FORMAT,
            peer,
            crate::harness::ts_from_secs(crate::paths::now_secs()),
            i64::try_from(rows.len()).unwrap_or(i64::MAX),
        ),
    )?;
    {
        let mut st = pack.prepare(
            "INSERT INTO rows(lane, text, role, project, session, ts, key) \
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
        )?;
        for r in rows {
            st.execute((
                r.lane, &r.text, &r.role, &r.project, &r.session, &r.ts, &r.key,
            ))?;
        }
    }
    // Closed explicitly, not at end of scope: the very next thing the caller
    // does is hand this path to someone who will ATTACH it.
    pack.close().map_err(|(_, e)| e)?;
    Ok(())
}

fn stem(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "pack".to_string())
}

pub fn flag(args: &[String], name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).filter(|v| !v.starts_with("--")).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> (Connection, tempdir::Dir) {
        let dir = tempdir::Dir::new("share");
        let conn = Connection::open(dir.path().join("index.db")).unwrap();
        db::ensure_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO mem(text, role, project, session, ts, file) \
             VALUES('the deploy failed because the token expired','assistant','app','s1','t','f')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO work(text, role, project, session, ts, file) \
             VALUES('bash kubectl rollout status','tool','app','s1','t','f')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mem(text, role, project, session, ts, file) \
             VALUES('unrelated other session','user','other','s2','t','f')",
            [],
        )
        .unwrap();
        (conn, dir)
    }

    #[test]
    fn a_bundle_round_trips_and_is_deduped_on_reimport() {
        let (conn, dir) = seeded();
        let mut rows = collect(&conn, Some("s1"), None).unwrap();
        assert_eq!(rows.len(), 2, "both lanes, and only session s1");
        for r in &mut rows {
            r.key = crate::text::stable_key(&r.session, &r.ts, &r.role, &r.text);
        }
        let pack = dir.path().join("a.cmlpack");
        let pack_s = pack.to_string_lossy().into_owned();
        write_bundle(&pack_s, &rows, "alice").unwrap();
        drop(conn);

        // A fresh index plays the receiver.
        let bob_dir = tempdir::Dir::new("share-bob");
        let mut bob = Connection::open(bob_dir.path().join("index.db")).unwrap();
        db::ensure_schema(&bob).unwrap();

        // Twice, through the real code path, to prove the second is a replace.
        for _ in 0..2 {
            bob.execute("ATTACH DATABASE ?1 AS pack", [&pack_s]).unwrap();
            let args = vec![pack_s.clone()];
            import_attached(&mut bob, &pack_s, &args).unwrap();
            bob.execute("DETACH DATABASE pack", []).unwrap();
        }

        assert_eq!(db::count(&bob, "mem"), 1, "a second import must not double");
        assert_eq!(db::count(&bob, "work"), 1);

        // And it is attributed, and reversible.
        let peer: String = bob
            .query_row("SELECT peer FROM origin LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(peer, "alice");
    }

    #[test]
    fn a_manifest_carries_the_author_and_the_count() {
        let (conn, dir) = seeded();
        let rows = collect(&conn, None, Some("app")).unwrap();
        let pack = dir.path().join("b.cmlpack").to_string_lossy().into_owned();
        write_bundle(&pack, &rows, "alice").unwrap();

        let p = Connection::open(&pack).unwrap();
        let (fmt, peer, n): (i64, String, i64) = p
            .query_row("SELECT format, peer, n FROM manifest", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!(fmt, FORMAT);
        assert_eq!(peer, "alice");
        assert_eq!(n, 2);
    }

    #[test]
    fn flags_do_not_swallow_the_next_flag() {
        let args: Vec<String> =
            ["--session", "--dry-run"].iter().map(ToString::to_string).collect();
        assert_eq!(flag(&args, "--session"), None, "a flag is not a value");
        let ok: Vec<String> = ["--session", "s1"].iter().map(ToString::to_string).collect();
        assert_eq!(flag(&ok, "--session"), Some("s1".into()));
    }
}

/// A scratch directory that removes itself. Three lines, versus a dependency.
#[cfg(test)]
pub mod tempdir {
    use std::path::{Path, PathBuf};

    pub struct Dir(PathBuf);

    impl Dir {
        pub fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "cml-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
