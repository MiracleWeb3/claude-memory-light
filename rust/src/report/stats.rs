//! `cml stats` — the one line that says whether there is anything in there.
//!
//! Rows and knowledge are printed as two numbers on purpose. They were once read
//! as one ("1152 brain points") when the second was a small fraction of the
//! first: rows are searchable substrate, knowledge is the durable subset a
//! curator has actually judged worth keeping.

use std::path::Path;

use rusqlite::Connection;

use super::{scalar, text};

/// What `stats` prints, before it is a string.
///
/// Separating the counting from the formatting is what makes this testable at
/// all: the C++ version was one function that opened the real 148 MB index,
/// counted, and printed, so the only way to check the arithmetic was to read it.
pub(super) struct Counts {
    rows: i64,
    by_role: Vec<(String, i64)>,
    knowledge: i64,
    scenes: i64,
    sessions: i64,
    files: i64,
}

pub(super) fn collect(conn: &Connection) -> Counts {
    Counts {
        rows: crate::db::count(conn, "mem"),
        by_role: by_role(conn),
        // Two sources, one idea: a gist the curator wrote, and the notes and wiki
        // pages that arrived already curated by a human.
        knowledge: scalar(conn, "SELECT count(*) FROM distilled WHERE gist != ''", [])
            + scalar(conn, "SELECT count(*) FROM mem WHERE role IN ('memory','wiki')", []),
        // Counted apart from both: a scene is neither a row nor a gist, and it is
        // the only number that says whether `cml distill --scenes` has ever run.
        scenes: crate::db::count(conn, "scene"),
        sessions: scalar(
            conn,
            "SELECT count(DISTINCT session) FROM mem \
             WHERE role IN ('user','assistant','summary')",
            [],
        ),
        files: crate::db::count(conn, "files"),
    }
}

fn by_role(conn: &Connection) -> Vec<(String, i64)> {
    let Ok(mut st) = conn.prepare("SELECT role, count(*) FROM mem GROUP BY role ORDER BY 2 DESC")
    else {
        return Vec::new();
    };
    let Ok(rows) = st.query_map([], |r| Ok((text(r, 0)?, r.get(1)?))) else {
        return Vec::new();
    };
    rows.flatten().collect()
}

impl Counts {
    fn line(&self, db: &Path, bytes: u64) -> String {
        let roles = self
            .by_role
            .iter()
            .map(|(role, n)| format!("{role}={n}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{} rows ({roles}) | {} knowledge | {} scenes | {} sessions | {} files | \
             {:.1} MB at {}",
            self.rows,
            self.knowledge,
            self.scenes,
            self.sessions,
            self.files,
            bytes as f64 / 1e6,
            db.display(),
        )
    }
}

pub fn stats(_args: &[String]) -> crate::R<i32> {
    let path = crate::db::db_path();
    let conn = crate::db::open()?;
    let bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
    println!("{}", collect(&conn).line(&path, bytes));
    Ok(0)
}

/// Used by `doctor`, which ends with the same line rather than a second, subtly
/// different one — that drift is how two counts of "the same" thing appear.
pub(super) fn line(conn: &Connection, db: &Path) -> String {
    let bytes = std::fs::metadata(db).map_or(0, |m| m.len());
    collect(conn).line(db, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert(conn: &Connection, role: &str, session: &str, text: &str) {
        conn.execute(
            "INSERT INTO mem(text, asks, role, project, session, ts, file) \
             VALUES (?1, '', ?2, 'p', ?3, '2026-08-10 10:00', 'f')",
            (text, role, session),
        )
        .unwrap();
    }

    #[test]
    fn counts_rows_roles_knowledge_and_sessions() {
        let (dir, conn) = super::super::scratch_db("stats");

        insert(&conn, "user", "s1", "a question");
        insert(&conn, "assistant", "s1", "an answer");
        insert(&conn, "user", "s2", "another question");
        insert(&conn, "memory", "s3", "a curated note");
        insert(&conn, "wiki", "s4", "a wiki page");
        conn.execute("INSERT INTO distilled(key, gist) VALUES ('k1', 'a gist')", [])
            .unwrap();
        // A judged-and-dropped row carries no gist and is not knowledge.
        conn.execute("INSERT INTO distilled(key, gist) VALUES ('k2', '')", []).unwrap();
        conn.execute(
            "INSERT INTO scene(title, summary, outcome, session, project, ts_start, \
             ts_end, n_rows) VALUES ('t', 's', 'solved', 's1', 'p', 'a', 'b', 2)",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO files(path, size, mtime) VALUES ('/f', 1, 2)", [])
            .unwrap();

        let c = collect(&conn);
        assert_eq!(c.rows, 5);
        assert_eq!(c.scenes, 1);
        assert_eq!(c.files, 1);
        // memory + wiki are knowledge; the empty gist is not.
        assert_eq!(c.knowledge, 3);
        // memory/wiki rows are not conversations, so they are not sessions.
        assert_eq!(c.sessions, 2);
        assert_eq!(c.by_role.iter().find(|(r, _)| r == "user").unwrap().1, 2);

        let out = c.line(std::path::Path::new("/tmp/index.db"), 148_700_000);
        assert!(out.starts_with("5 rows (user=2, "), "{out}");
        assert!(out.ends_with("| 148.7 MB at /tmp/index.db"), "{out}");

        let _ = std::fs::remove_dir_all(dir);
    }
}
