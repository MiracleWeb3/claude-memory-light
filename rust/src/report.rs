//! The commands that report on the index instead of feeding it.
//!
//! Five commands live here because they answer one question in five sizes:
//! *is this thing actually working?* `stats` is the one-liner, `doctor` the full
//! examination, `loops` and `eval` the two measurements that can contradict the
//! other three, and `forget` the one lever a human pulls after reading them.
//!
//! The C++ tree spread these over report.cpp, forget.cpp, loops.cpp and eval.cpp,
//! and each file re-derived its own scalar-query call and its own `--flag N`
//! parser. Those are the four functions below, written once. The text
//! predicates they also each re-derived — noise, squeeze, stable_key — belong to
//! [`crate::text`], and the commands here call that rather than keeping a second
//! copy of the tables to drift against.

use rusqlite::{Connection, Params};

mod doctor;
mod eval;
mod forget;
mod stats;
/// Public because the SessionStart briefing shares the recurrence heuristic with
/// `cml loops` — one implementation, two front ends. See [`loops::loop_lines`].
pub mod loops;

pub use doctor::doctor;
pub use eval::eval;
pub use forget::forget;
pub use loops::{loop_lines, loops};
pub use stats::stats;

/// One number out of the database, or 0.
///
/// Every count in this module is "a number, or nothing to report": a `doctor`
/// that aborts because `recalled` does not exist yet fails exactly the person
/// who ran it to find out what does not exist yet.
pub(crate) fn scalar<P: Params>(conn: &Connection, sql: &str, params: P) -> i64 {
    conn.query_row(sql, params, |r| r.get(0)).unwrap_or(0)
}

/// One text column, NULL read as empty.
///
/// Never `get::<String>` on a column out of `mem`: the UNINDEXED FTS5 columns
/// come back NULL on every row an older build wrote — 3,580 of 3,833 rows carry
/// a NULL `asks` right now — and `get::<String>` returns `Err` on NULL. Inside a
/// `query_map` that error silently drops the whole row; inside `forget` it reads
/// as "no such row", so the command reports success and deletes nothing.
pub(crate) fn text(row: &rusqlite::Row, col: usize) -> rusqlite::Result<String> {
    Ok(row.get::<_, Option<String>>(col)?.unwrap_or_default())
}

/// `--flag N`. Missing, unparsable or non-positive all read as absent, so
/// `--days banana` keeps the default instead of silently meaning zero days.
pub(crate) fn num_arg(args: &[String], flag: &str) -> Option<usize> {
    let at = args.iter().position(|a| a == flag)?;
    args.get(at + 1)?.parse::<usize>().ok().filter(|n| *n > 0)
}

pub(crate) fn has(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

/// One labelled line of a report. `doctor` and `eval` share the column so their
/// output reads as one program's.
pub(crate) fn row(label: &str, value: impl AsRef<str>) {
    println!("{label:<16}: {}", value.as_ref());
}

#[cfg(test)]
pub(crate) fn scratch_db(tag: &str) -> (std::path::PathBuf, Connection) {
    let dir = std::env::temp_dir().join(format!("cml-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("index.db");
    let conn = crate::db::open_at(&path).unwrap();
    (dir, conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn num_arg_ignores_junk_and_keeps_the_default() {
        let args: Vec<String> = ["--days", "7", "--limit", "x", "--k"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        assert_eq!(num_arg(&args, "--days"), Some(7));
        assert_eq!(num_arg(&args, "--limit"), None, "unparsable must not mean 0");
        assert_eq!(num_arg(&args, "--k"), None, "trailing flag has no value");
    }
}
