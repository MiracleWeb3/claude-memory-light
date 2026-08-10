//! `cml index` — what gets scanned, in what order, and for how long.
//!
//! The judgement lives one level down: `transcripts` decides which lines of a
//! session are worth a row, `notes` takes markdown whole. This file is only the
//! sequence and its clock, because the sequence is what runs on every Stop hook
//! and therefore has to be bounded.
//!
//! What the C++ tree needed and this does not: a per-file `SELECT ... FROM files`
//! (one map, read once), a per-file `SELECT ... FROM mem` to rebuild the dedup set
//! (3,827 rows read once, not once per file — that scan was O(files x rows)), and
//! a hand-rolled `Budget` header shared by two translation units.

pub mod entry;
pub mod files;
pub mod notes;
pub mod transcripts;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::db;
use files::Counts;

/// cml index [--all|--force]
pub fn run(args: &[String]) -> crate::R<i32> {
    let force = args.iter().any(|a| a == "--all" || a == "--force");
    let mut conn = db::open()?;
    let budget = Budget::from_env();
    let known = files::known(&conn);
    let root = db::transcripts_dir();

    let t = transcripts::index(&mut conn, &root, &known, force, &budget)?;

    // Memory notes live beside the transcripts, one directory per project.
    let mut m = Counts::default();
    for (dir, project) in project_dirs(&root) {
        m += notes::index_dir(&mut conn, &known, &dir.join("memory"), "memory", &project, force)?;
    }
    let w = notes::index_dir(&mut conn, &known, &db::home().join("wiki"), "wiki", "wiki", force)?;

    // Embedding and curation are separate commands with their own clocks; this
    // line reports only what indexing itself did.
    println!(
        "indexed {} file(s), {} row(s)  [transcripts {}/{}, memory {}/{}, wiki {}/{}]",
        t.files + m.files + w.files,
        t.rows + m.rows + w.rows,
        t.files,
        t.rows,
        m.files,
        m.rows,
        w.files,
        w.rows
    );
    Ok(0)
}

/// A wall-clock ceiling for work that runs from a hook.
///
/// Every unit below is resumable — files commit in batches and a skipped file is
/// simply still stale next turn — so stopping early costs latency, never data.
pub struct Budget {
    end: Option<Instant>,
}

impl Budget {
    /// `CML_INDEX_BUDGET_MS`, default 4s. `0` removes the ceiling, which is what a
    /// deliberate `cml index --all` catch-up run wants.
    pub fn from_env() -> Self {
        let ms = std::env::var("CML_INDEX_BUDGET_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok());
        Self::new(ms.unwrap_or(4_000))
    }

    pub fn new(ms: u64) -> Self {
        Self {
            end: (ms > 0).then(|| Instant::now() + Duration::from_millis(ms)),
        }
    }

    pub fn spent(&self) -> bool {
        self.end.is_some_and(|e| Instant::now() >= e)
    }
}

/// The project directories under `~/.claude/projects`, with the label that goes in
/// the `project` column.
///
/// Dot-directories are skipped: `.omc/state` holds agent-replay `.jsonl` files that
/// are telemetry, not conversation, and indexing them would spend rows on nothing.
/// Several real project directories begin with `-`, which is why this reads them
/// with `read_dir` rather than a glob.
pub fn project_dirs(root: &Path) -> Vec<(PathBuf, String)> {
    let Ok(rd) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    rd.flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            // crate::paths owns the slug -> label mapping; search, recall and state
            // all filter on the value it produces, so a second copy here would
            // silently break every --project filter the day the two drifted.
            (!name.starts_with('.')).then(|| (e.path(), crate::paths::project_label(&name)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_zero_never_expires() {
        assert!(!Budget::new(0).spent());
        assert!(Budget::new(1).end.is_some());
    }
}
