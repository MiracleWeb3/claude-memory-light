//! `cml embed [--all] [--limit N]` — give every row a vector.
//!
//! The C++ embedded `mem` only, so 57,683 rows of tool output and 72 distilled
//! scenes had no vectors and semantic search could not see them. All three lanes
//! are swept here; `crate::lane::Lane::ALL` is what makes forgetting one impossible.
//!
//! Storage moved out of sqlite-vec into a plain `emb` BLOB table (see `db.rs`).
//! **Vectors already in the old `vec_mem` tables are not migrated.** sqlite-vec
//! stores them in an internal chunked format that cannot be read without loading
//! the extension — the exact dependency this port deletes — so `cml embed --all`
//! recomputes them instead. At model2vec speed that is seconds of work, and the
//! old tables can then be dropped.

pub mod model;
pub mod wordpiece;

use std::time::Instant;

use rayon::prelude::*;
use rusqlite::types::ValueRef;
use rusqlite::{params, Connection, OptionalExtension};

use crate::lane::Lane;
use crate::R;
use model::Model;

/// Only the first 2000 characters of a row are embedded, matching what is already
/// in the index. SQLite's `substr` counts characters, not bytes — the same reason
/// the dedup keys elsewhere do.
const EMBED_CHARS: usize = 2000;

/// Rows held in memory at once. 2048 x 2000 chars is ~4 MB of text per pass, which
/// keeps a 57,683-row sweep flat instead of loading 115 MB up front the way the
/// C++ did for its (much smaller) single lane.
const BATCH: usize = 2048;

/// What text a row's vector is *of*. Lives here rather than in `Lane` because it is
/// an embedding decision, not a search one: a scene's title carries its topic and
/// belongs in the vector even though the ranker displays only the summary.
fn source_sql(lane: Lane) -> &'static str {
    match lane {
        Lane::Conversation | Lane::Tools => "coalesce(text,'')",
        Lane::Scene => "coalesce(title,'') || ' ' || coalesce(summary,'')",
    }
}

/// Embed every row of `lane` that has no vector yet, up to `budget` rows.
fn sweep(conn: &Connection, model: &Model, lane: Lane, budget: usize) -> R<usize> {
    let select = format!(
        "SELECT rowid, substr({src},1,{chars}) FROM {table} \
         WHERE rowid NOT IN (SELECT rowid_ref FROM emb WHERE lane = ?1) LIMIT ?2",
        src = source_sql(lane),
        chars = EMBED_CHARS,
        table = lane.table(),
    );
    let dim = model.dim() as i64;
    let mut done = 0usize;

    while done < budget {
        let take = BATCH.min(budget - done);
        // Collected before writing so the read cursor is closed by the time the
        // transaction opens.
        //
        // Read as bytes, not `String`: 107 of the 61,825 rows in the live index are
        // not valid UTF-8 — the transcripts they came from were cut mid-character.
        // `get::<String>` returns `Err` on those, which would abort the whole sweep
        // at the first one. `from_utf8_lossy` substitutes U+FFFD, which the
        // normalizer already drops, so a torn character costs one token and nothing
        // else. This is what the C++ did implicitly by holding bytes in a
        // `std::string` and replacing bad sequences during its own UTF-8 decode.
        let pending: Vec<(i64, String)> = {
            let mut st = conn.prepare_cached(&select)?;
            let rows = st.query_map(params![lane.table(), take as i64], |r| {
                let text = match r.get_ref(1)? {
                    ValueRef::Text(b) | ValueRef::Blob(b) => {
                        String::from_utf8_lossy(b).into_owned()
                    }
                    _ => String::new(),
                };
                Ok((r.get(0)?, text))
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        if pending.is_empty() {
            break;
        }

        // The whole reason rayon is a dependency: this map has no shared state.
        let vectors: Vec<(i64, Vec<f32>)> = pending
            .par_iter()
            .map(|(rowid, text)| (*rowid, model.encode(text)))
            .collect();

        let tx = conn.unchecked_transaction()?;
        {
            let mut ins = tx.prepare_cached(
                "INSERT OR REPLACE INTO emb(lane, rowid_ref, dim, v) VALUES (?1,?2,?3,?4)",
            )?;
            for (rowid, v) in &vectors {
                ins.execute(params![lane.table(), rowid, dim, crate::vector::to_blob(v)])?;
            }
        }
        tx.commit()?;
        done += vectors.len();
    }
    Ok(done)
}

/// The width already in the index, or `None` when nothing is embedded yet.
fn stored_dim(conn: &Connection) -> Option<usize> {
    conn.query_row("SELECT dim FROM emb LIMIT 1", [], |r| r.get::<_, i64>(0))
        .optional()
        .ok()
        .flatten()
        .map(|d| d as usize)
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == name {
            return it.next().cloned();
        }
        if let Some(v) = a.strip_prefix(&format!("{name}=")) {
            return Some(v.to_string());
        }
    }
    None
}

pub fn run(args: &[String]) -> R<i32> {
    let all = args.iter().any(|a| a == "--all");
    let budget = flag(args, "--limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(usize::MAX);

    let conn = crate::db::open()?;
    // Loud, unlike the hook path: this command was asked for by name, so a missing
    // model is an error rather than a silent no-op.
    let model = match Model::load() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("cml: {e}");
            return Ok(1);
        }
    };

    // A width change means a different model: the old vectors are not comparable to
    // the new ones, and rebuilding is the only correct answer, asked for or not.
    let width_changed = stored_dim(&conn).is_some_and(|d| d != model.dim());
    if width_changed {
        println!(
            "vector width changed -> {} ({}) — rebuilding",
            model.dim(),
            model.id()
        );
    }
    if all || width_changed {
        conn.execute("DELETE FROM emb", [])?;
    }

    let start = Instant::now();
    let mut done = 0usize;
    for lane in Lane::ALL {
        let n = sweep(&conn, &model, lane, budget.saturating_sub(done))?;
        if n > 0 {
            println!("  {lane}: {n} row(s)");
        }
        done += n;
        if done >= budget {
            break;
        }
    }

    let total = crate::db::count(&conn, "emb");
    println!(
        "embedded {done} new row(s) in {:.1}s ({total} total, {}-dim, {})",
        start.elapsed().as_secs_f64(),
        model.dim(),
        model.id()
    );
    Ok(0)
}

/// Incremental pass for `cml index`, which runs from a hook on every turn.
///
/// Silent and gated: a user who has never run `cml embed` has an empty `emb` table
/// and must not pay a 30 MB model load once per turn to keep it empty. Returns how
/// many rows were added.
pub fn embed_new(conn: &Connection) -> usize {
    let Some(dim) = stored_dim(conn) else { return 0 };
    let Ok(model) = Model::load() else { return 0 };
    if dim != model.dim() {
        return 0; // `cml embed --all` rebuilds; a hook must not decide that
    }
    Lane::ALL
        .iter()
        .map(|&lane| sweep(conn, &model, lane, usize::MAX).unwrap_or(0))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_lane_has_embeddable_source_text() {
        // The C++ defect this replaces: `work` and `scene` were never embedded at
        // all. Adding a lane without a source expression fails to compile; this
        // checks the expressions are not accidentally empty.
        for lane in Lane::ALL {
            assert!(source_sql(lane).contains("coalesce"), "{lane} has no source text");
        }
    }

    #[test]
    fn limit_is_read_in_both_spellings() {
        let split = ["--limit".to_string(), "50".to_string()];
        let joined = ["--limit=50".to_string()];
        assert_eq!(flag(&split, "--limit").as_deref(), Some("50"));
        assert_eq!(flag(&joined, "--limit").as_deref(), Some("50"));
        assert_eq!(flag(&["--all".to_string()], "--limit"), None);
    }

    #[test]
    fn an_empty_index_reports_no_stored_width() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::ensure_schema(&conn).unwrap();
        assert_eq!(stored_dim(&conn), None);
        conn.execute(
            "INSERT INTO emb(lane, rowid_ref, dim, v) VALUES ('mem', 1, 256, x'00')",
            [],
        )
        .unwrap();
        assert_eq!(stored_dim(&conn), Some(256));
    }
}
