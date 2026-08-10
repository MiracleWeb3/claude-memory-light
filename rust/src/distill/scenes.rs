//! L2: one summary per real working session, written under `Rubric::Scene`.
//!
//! Shares nothing with row distillation but the wire: nothing is kept or dropped
//! here, the unit is a session rather than a row, and the output is a new `scene` row
//! rather than a verdict against an existing one.

use std::collections::{HashMap, HashSet};

use rusqlite::{params, Connection};

use super::prompts::Rubric;
use super::wire::{self, Conf};
use super::StandDown;
use crate::text::{gist_lookup, squeeze, stable_key};

/// Four sessions per call, not twenty. `post` runs `curl --max-time 90` and row
/// distillation's batch of 20 is sized for 500-char rows; twenty digests of this size
/// is a ~120 KB request asking for twenty summaries, which does not come back inside
/// the timeout — and a timeout costs the whole call.
const BATCH: usize = 4;
const DIGEST: usize = 6000; // bytes of digest per session
const ROW: usize = 300; // chars of a row with no gist of its own

/// The conversation roles, matching `stats()`. Without this filter the `memory` and
/// `wiki` rows — hand-written note files, not conversation — group into one more
/// "session", and the curator is paid to summarise fragments of MEMORY.md.
const ELIGIBLE: &str = "session != '' AND role IN ('user','assistant','summary')";

struct Session {
    id: String,
    project: String,
    ts_start: String,
    ts_end: String,
    rows: i64,
    digest: String,
}

/// Sessions with no scene yet, newest first.
///
/// Newest first because the recent sessions are both the likeliest to be recalled and
/// the hardest case for the rubric: a `--limit` run then calibrates against dense
/// build work rather than whatever happens to sort first.
fn pending(conn: &Connection, cap: Option<usize>) -> rusqlite::Result<Vec<Session>> {
    let mut seen = conn.prepare("SELECT session FROM scene")?;
    let done: HashSet<String> = seen.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;

    let mut stmt = conn.prepare(&format!(
        "SELECT session, project, min(ts), max(ts), count(*) FROM mem WHERE {ELIGIBLE} \
         GROUP BY session HAVING count(*) >= 5 ORDER BY max(ts) DESC"
    ))?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        let id: String = r.get(0)?;
        if done.contains(&id) {
            continue;
        }
        out.push(Session {
            id,
            project: r.get(1)?,
            ts_start: r.get(2)?,
            ts_end: r.get(3)?,
            rows: r.get(4)?,
            digest: String::new(),
        });
        if cap.is_some_and(|c| out.len() >= c) {
            break;
        }
    }
    Ok(out)
}

/// One session as the curator reads it: a line per turn, the curated gist where the
/// row earned one, otherwise the row clipped.
pub fn session_digest(
    conn: &Connection,
    session: &str,
    gists: &HashMap<String, String>,
) -> rusqlite::Result<String> {
    let mut stmt = conn.prepare(&format!(
        "SELECT ts, role, substr(text,1,{ROW}), substr(text,1,64) FROM mem \
         WHERE session=?1 AND {ELIGIBLE} ORDER BY ts"
    ))?;
    let mut rows = stmt.query([session])?;
    let mut lines = Vec::new();
    while let Some(r) = rows.next()? {
        let (ts, role, text, head): (String, String, String, String) =
            (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
        let body = gists.get(&stable_key(session, &ts, &role, &head)).unwrap_or(&text);
        lines.push(format!("{role}: {}", squeeze(body, ROW)));
    }
    Ok(assemble(&lines))
}

/// Over the cap this keeps the FIRST TWO TURNS AND THE ENDING, never a prefix.
///
/// 27 of 76 sessions here exceed 6,000 chars (median 4,460, p90 17,575, max 67,395),
/// and they are the long ones a scene is most for. `outcome` is decided by how a
/// session ended, so a prefix cap would judge the 282-row session on its first 9% and
/// then ask it how things turned out.
fn assemble(lines: &[String]) -> String {
    let bytes: usize = lines.iter().map(|l| l.len() + 1).sum();
    // Lines [tail, end) are the ending that survives; tail == 2 means nothing was cut.
    let mut tail = 2;
    if bytes > DIGEST && lines.len() > 2 {
        // Two turns plus one more cannot reach the cap (a turn is clipped to 300
        // chars), so the ending is never the part that gets dropped.
        let mut used = lines[0].len() + lines[1].len() + 2;
        tail = lines.len();
        while tail > 2 && used + lines[tail - 1].len() < DIGEST {
            used += lines[tail - 1].len() + 1;
            tail -= 1;
        }
    }
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i == 2 && tail > 2 {
            out.push_str(&format!("[... {} turns omitted ...]\n", tail - 2));
        }
        if i >= 2 && i < tail {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Summarises every eligible session not already in `scene`, newest first, and
/// returns how many were written.
///
/// `cap` bounds the run in SESSIONS, not rows — the point of it is calibrating on 20
/// before spending the rest of the money. Sessions whose call fails are simply left
/// out of `scene`, so the next run retries them.
pub fn build_scenes(conn: &Connection, conf: &Conf, cap: Option<usize>) -> crate::R<usize> {
    let mut todo = pending(conn, cap)?;
    if todo.is_empty() {
        return Ok(0);
    }
    let gists = gist_lookup(conn);
    for s in &mut todo {
        s.digest = session_digest(conn, &s.id, &gists)?;
    }

    let total = todo.len().div_ceil(BATCH);
    let mut written = 0;
    let mut stalls = StandDown::default();
    for (bi, chunk) in todo.chunks(BATCH).enumerate() {
        // Ids are the index WITHIN this batch, so an id the model invents cannot name
        // a session that was never sent.
        let rows: Vec<(i64, String)> =
            chunk.iter().enumerate().map(|(i, s)| (i as i64, s.digest.clone())).collect();

        let reply = wire::post(conf, &rows, Rubric::Scene).and_then(|r| wire::parse_scenes(&r));
        let scenes = match reply {
            Ok(s) => {
                stalls.worked();
                s
            }
            Err(e) => {
                eprintln!(
                    "batch {}/{total} skipped ({e}) — sessions stay, retried next run",
                    bi + 1
                );
                if stalls.failed() {
                    break;
                }
                continue;
            }
        };

        // `scene` has no PRIMARY KEY and `pending` skips whatever is already in it, so
        // a wrong id is written once and never revisited. A repeated one would give a
        // session two contradictory scenes.
        let mut seen = vec![false; chunk.len()];
        for sc in scenes {
            let Ok(i) = usize::try_from(sc.id) else { continue };
            if i >= chunk.len() || seen[i] {
                continue;
            }
            seen[i] = true;
            let s = &chunk[i];
            written += conn.execute(
                "INSERT INTO scene(title,summary,outcome,session,project,ts_start,ts_end,n_rows) \
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    sc.title,
                    sc.summary,
                    sc.outcome.to_lowercase(),
                    s.id,
                    s.project,
                    s.ts_start,
                    s.ts_end,
                    s.rows
                ],
            )?;
        }
        // Progress is the point. No explicit flush: Rust's stdout is line-buffered
        // even into a pipe, which is exactly the C `fflush` that stood here.
        println!(
            "batch {}/{total}: {written} of {} sessions summarised so far",
            bi + 1,
            todo.len()
        );
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(n: usize, len: usize) -> String {
        format!("user: {n} {}", "x".repeat(len))
    }

    #[test]
    fn a_short_session_is_left_whole() {
        let lines = vec![turn(0, 10), turn(1, 10), turn(2, 10), turn(3, 10)];
        let out = assemble(&lines);
        assert!(!out.contains("omitted"));
        assert_eq!(out.lines().count(), 4);
    }

    /// The property the whole cap exists for: `outcome` is decided by how a session
    /// ENDED, so an over-long digest must keep the ending and cut the middle.
    #[test]
    fn an_over_long_session_keeps_its_ending_not_its_prefix() {
        let lines: Vec<String> = (0..100).map(|n| turn(n, ROW)).collect();
        let out = assemble(&lines);
        assert!(out.len() <= DIGEST + 64, "digest stays near the cap: {}", out.len());
        assert!(out.starts_with(&lines[0]), "the opening survives");
        assert!(out.contains(&lines[1]), "and the second turn");
        assert!(out.trim_end().ends_with(lines[99].as_str()), "the ENDING survives");
        assert!(out.contains("turns omitted ..."), "and the cut is declared");
        assert!(!out.contains(&lines[50]), "the middle is what goes");
    }

    /// Exactly at the boundary nothing is cut, and the marker never appears without a
    /// cut behind it.
    #[test]
    fn the_marker_only_appears_when_something_was_cut() {
        let lines: Vec<String> = (0..3).map(|n| turn(n, 10)).collect();
        assert!(!assemble(&lines).contains("omitted"));
        assert_eq!(assemble(&[]), "");
        assert_eq!(assemble(&[turn(0, 5)]), format!("{}\n", turn(0, 5)));
    }

}
