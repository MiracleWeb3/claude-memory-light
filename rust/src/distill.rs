//! `cml distill` — an LLM judges what deserves memory; junk gets blocklisted.
//!
//! Two independent axes, two calls. A KEEP pass asks whether a row holds content at
//! all; the rows that survive are re-asked, on their own, whether they earn a `gist`.
//! Folding the second question into the first was measured at 45-84% minted against
//! 16% for the same clause asked separately — a deliberately generous keep list drags
//! the gist decision up with it.
//!
//! Every failure here is a non-event by construction: a batch whose call fails leaves
//! its rows out of `distilled`, and the next run picks them up again. Nothing is lost
//! by an endpoint that is down, only deferred.

pub mod prompts;
pub mod scenes;
pub mod wire;

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use rusqlite::{params, Connection};

use prompts::Rubric;
use wire::{Conf, Verdict};

// `stable_key` names a row by what it IS, so a verdict survives the reindex that
// renumbers every rowid. Shared with search, index and forget — one definition in
// `text`, never a private copy: two keying rules that drift would re-judge the whole
// corpus at the user's expense.
use crate::text::stable_key;

/// Rows per keep call. Sized for the 500-char clip below: twenty of those fit inside
/// `curl --max-time 90` at ~20s a row of reasoning-tier latency.
const BATCH: usize = 20;

/// Only the first 500 characters of a row are judged. The curator decides whether a
/// row is *the kind of thing* worth keeping, which the opening of it already answers.
const CLIP: usize = 500;

/// Consecutive unanswered calls before the curator stops for this run.
///
/// Without this, a `--all` drain against a dead endpoint spends `--max-time` per batch
/// for thousands of batches — on 2026-08-02 four consecutive index runs each burned
/// their whole budget and curated nothing, because the endpoint answers but the call
/// does not land. Bounded waste is still waste when it repeats. Three failures in a
/// row is an endpoint problem, not a batch problem.
const STAND_DOWN: usize = 3;

pub fn run(args: &[String]) -> crate::R<i32> {
    // Flags are read before any of them acts: --all is destructive, and which table it
    // clears depends on whether --scenes appears later in the same argv.
    //
    // --limit exists because the background curator needs a bounded chunk: at ~20s a
    // row an unbounded run drains the whole backlog in one sitting, which is a long
    // time to hold the lock and a lot of balance spent at once.
    let (mut all, mut want_scenes, mut cap) = (false, false, None);
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--all" => all = true,
            "--scenes" => want_scenes = true,
            "--limit" => {
                cap = rest.next().and_then(|v| v.parse::<usize>().ok()).filter(|&n| n > 0).or(cap);
            }
            _ => {}
        }
    }

    // No key is a no-op, not a crash: the hook path must stay quiet on a machine that
    // never configured a curator.
    let Some(conf) = Conf::load() else {
        eprintln!(
            "cml: no curator key — put one in ~/.claude/claude-memory-light/llm.key \
             (or set CML_LLM_KEY)"
        );
        return Ok(1);
    };
    let conn = crate::db::open()?;
    // Wall clock, not CPU: nearly all of it is spent waiting on the endpoint.
    let t0 = Instant::now();

    // --scenes is L2 and runs alone: it judges nothing, drops nothing, and its --limit
    // counts SESSIONS rather than rows, because the run that matters is the first
    // twenty read by hand before the rest of the money is spent.
    if want_scenes {
        if all {
            let cleared = conn.execute("DELETE FROM scene", [])?;
            println!("re-summarising every session from scratch ({cleared} prior scenes cleared)");
        }
        let n = scenes::build_scenes(&conn, &conf, cap)?;
        println!("scenes: {n} written in {:.0}s ({})", t0.elapsed().as_secs_f64(), conf.model);
        return Ok(0);
    }

    if all {
        let cleared = conn.execute("DELETE FROM distilled", [])?;
        println!("re-judging every row from scratch ({cleared} prior verdicts cleared)");
    }
    let (kept, dropped) = judge(&conn, &conf, cap, Rubric::Assistant)?;
    let (ukept, udropped) = judge(&conn, &conf, cap, Rubric::User)?;
    // Carried over verbatim, and it is STALE: distill has never written `forgotten`
    // (only `cml forget` does), and on the live index that table holds 0 rows. A
    // dropped row is recorded so it is not re-judged, and stays fully searchable.
    // Left as-is because the wording is user-facing behaviour, not an implementation
    // detail — changing it is the user's call.
    println!(
        "distilled: kept {}, dropped {} in {:.0}s ({}); dropped rows are blocklisted \
         (undo: cml forget --clear)",
        kept + ukept,
        dropped + udropped,
        t0.elapsed().as_secs_f64(),
        conf.model
    );
    Ok(0)
}

/// One row on the worklist. `key` is computed once, in SQL, and serves both jobs it
/// has: skipping rows already judged, and naming the row in `distilled`.
struct Row {
    id: i64,
    text: String,
    key: String,
}

/// Judges every unjudged row belonging to `rubric` and returns (kept, dropped).
fn judge(
    conn: &Connection,
    conf: &Conf,
    cap: Option<usize>,
    rubric: Rubric,
) -> crate::R<(usize, usize)> {
    let todo = worklist(conn, cap, rubric)?;
    if todo.is_empty() {
        return Ok((0, 0));
    }

    let total = todo.len().div_ceil(BATCH);
    let (mut kept, mut dropped, mut minted) = (0, 0, 0);
    let mut stalls = StandDown::default();
    for (bi, batch) in todo.chunks(BATCH).enumerate() {
        let rows: Vec<(i64, String)> = batch.iter().map(|r| (r.id, r.text.clone())).collect();
        let verdicts = match call(conf, &rows, rubric) {
            Ok(v) => {
                stalls.worked();
                v
            }
            Err(e) => {
                eprintln!("batch {}/{total} skipped ({e}) — rows stay, retried next run", bi + 1);
                if stalls.failed() {
                    break;
                }
                continue;
            }
        };

        // Second pass. A failure here costs nothing: the row is stored gistless, still
        // searchable, simply not plotted on the map.
        let mut durable: HashMap<i64, String> = HashMap::new();
        let kept_rows = survivors(batch, &verdicts);
        if !kept_rows.is_empty() {
            match call(conf, &kept_rows, Rubric::Durability) {
                Ok(judged) => {
                    for d in judged {
                        if !d.gist.is_empty() {
                            durable.insert(d.id, d.gist);
                        }
                        // Expansions land straight on the row: they are search
                        // surface, not a verdict, so they are stored where FTS5 will
                        // index them.
                        if !d.asks.is_empty() {
                            conn.execute(
                                "UPDATE mem SET asks=?1 WHERE rowid=?2",
                                params![d.asks, d.id],
                            )?;
                        }
                    }
                }
                Err(e) => eprintln!("  durability pass skipped ({e}) — rows kept gistless"),
            }
        }

        for v in &verdicts {
            // A verdict naming a row that was not in this batch is a hallucination; it
            // would blocklist or bless a row nobody judged.
            let Some(row) = batch.iter().find(|r| r.id == v.id) else { continue };
            let gist = match durable.get(&v.id) {
                Some(g) if v.keep => g.as_str(),
                _ => "",
            };
            // A "drop" verdict demotes, it does not delete. Measured: deleting them
            // cost recall@3 19.7% -> 14.6%, and destroyed 547 of 821 question/answer
            // pairs outright — two thirds of the history had no answer left to find.
            // The curator is good at judging what deserves a permanent gist and has no
            // business deciding what still exists, because it judges a row before the
            // question that needs it has been asked. Recorded either way so the row is
            // not re-judged every run; `cml forget` remains the deliberate delete.
            conn.execute(
                "INSERT OR REPLACE INTO distilled(key, gist) VALUES (?1, ?2)",
                params![row.key, gist],
            )?;
            if !v.keep {
                dropped += 1;
            } else {
                kept += 1;
                minted += usize::from(!gist.is_empty());
            }
        }
        println!(
            "batch {}/{total}: kept {kept} ({minted} durable), dropped {dropped} so far",
            bi + 1
        );
    }
    Ok((kept, dropped))
}

/// Rows this rubric owns that carry no verdict yet, newest-last, capped.
fn worklist(conn: &Connection, cap: Option<usize>, rubric: Rubric) -> rusqlite::Result<Vec<Row>> {
    let mut seen = conn.prepare("SELECT key FROM distilled")?;
    let judged: HashSet<String> =
        seen.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;

    let mut stmt = conn.prepare(&format!(
        "SELECT rowid, substr(text,1,{CLIP}), session, ts, role, substr(text,1,64) \
         FROM mem WHERE {}",
        rubric.row_filter()
    ))?;
    let mut rows = stmt.query([])?;
    let mut todo = Vec::new();
    while let Some(r) = rows.next()? {
        let key = stable_key(
            &r.get::<_, String>(2)?,
            &r.get::<_, String>(3)?,
            &r.get::<_, String>(4)?,
            &r.get::<_, String>(5)?,
        );
        if judged.contains(&key) {
            continue;
        }
        todo.push(Row { id: r.get(0)?, text: r.get(1)?, key });
        if cap.is_some_and(|c| todo.len() >= c) {
            break;
        }
    }
    Ok(todo)
}

/// One judging round: POST, then read the verdicts out of the reply.
fn call(conf: &Conf, rows: &[(i64, String)], rubric: Rubric) -> Result<Vec<Verdict>, String> {
    wire::post(conf, rows, rubric).and_then(|reply| wire::parse_verdicts(&reply))
}

/// The rows a keep pass blessed, ready to send to the durability pass.
///
/// Split out because it is a join between two independent replies, which is where a
/// hallucinated id would otherwise get a gist minted against the wrong text.
fn survivors(batch: &[Row], verdicts: &[Verdict]) -> Vec<(i64, String)> {
    verdicts
        .iter()
        .filter(|v| v.keep)
        .filter_map(|v| batch.iter().find(|r| r.id == v.id))
        .map(|r| (r.id, r.text.clone()))
        .collect()
}

/// Counts consecutive failed calls so a dead endpoint costs one timeout, not one per
/// batch for the rest of the backlog.
#[derive(Default)]
pub struct StandDown(usize);

impl StandDown {
    pub fn worked(&mut self) {
        self.0 = 0;
    }

    /// True once the curator should stop for this run. Says so on stderr, where the
    /// curator log keeps it: a detached run has no other witness.
    pub fn failed(&mut self) -> bool {
        self.0 += 1;
        if self.0 < STAND_DOWN {
            return false;
        }
        eprintln!(
            "curator stood down after {STAND_DOWN} unanswered calls in a row — \
             the rest stays unjudged and is retried on the next run"
        );
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64) -> Row {
        Row { id, text: format!("text {id}"), key: format!("s|t|assistant|text {id}") }
    }

    fn verdict(id: i64, keep: bool) -> Verdict {
        Verdict { id, keep, gist: String::new(), asks: String::new() }
    }

    /// Batching is what bounds the request size and the blast radius of one failed
    /// call. Every row must land in exactly one batch, and the last one is short.
    #[test]
    fn every_row_lands_in_exactly_one_batch() {
        let todo: Vec<Row> = (0..45).map(row).collect();
        let batches: Vec<&[Row]> = todo.chunks(BATCH).collect();
        assert_eq!(batches.len(), todo.len().div_ceil(BATCH), "the printed total matches");
        assert_eq!(batches.iter().map(|b| b.len()).collect::<Vec<_>>(), vec![20, 20, 5]);

        let seen: Vec<i64> = batches.iter().flat_map(|b| b.iter().map(|r| r.id)).collect();
        assert_eq!(seen, (0..45).collect::<Vec<_>>(), "no row is lost or judged twice");
    }

    /// A batch smaller than BATCH is one batch, not zero — the off-by-one that would
    /// silently curate nothing on every small run.
    #[test]
    fn a_short_worklist_is_still_one_batch() {
        let todo: Vec<Row> = (0..3).map(row).collect();
        assert_eq!(todo.chunks(BATCH).count(), 1);
        assert_eq!(todo.len().div_ceil(BATCH), 1);
    }

    /// Only kept rows are re-asked about durability, and only rows that were actually
    /// in the batch: an id the model invents must not pull an unrelated row into the
    /// second, gist-minting call.
    #[test]
    fn survivors_are_the_kept_rows_of_this_batch_only() {
        let batch: Vec<Row> = (0..3).map(row).collect();
        let verdicts =
            vec![verdict(0, true), verdict(1, false), verdict(2, true), verdict(99, true)];
        let got = survivors(&batch, &verdicts);
        assert_eq!(
            got,
            vec![(0, "text 0".to_string()), (2, "text 2".to_string())],
            "kept only, and the invented id 99 is not resolved to anything"
        );
    }

    /// The stand-down fires on consecutive failures, and a single answered call
    /// forgives everything before it — a run must not stop because of two failures an
    /// hour apart.
    #[test]
    fn stand_down_needs_consecutive_failures() {
        let mut s = StandDown::default();
        for _ in 0..STAND_DOWN * 2 {
            assert!(!s.failed(), "one failure is not a stand-down");
            s.worked();
        }
        for i in 1..STAND_DOWN {
            assert!(!s.failed(), "failure {i} of {STAND_DOWN}");
        }
        assert!(s.failed(), "the {STAND_DOWN}th consecutive failure stands the curator down");
    }

    /// SQL already clipped the head with `substr(text,1,64)`, so `stable_key` clips a
    /// second time over the same 64 characters. That must be a no-op — it is what lets
    /// the worklist's skip-check and the `distilled` insert share one key.
    #[test]
    fn clipping_the_key_twice_is_a_no_op() {
        let long = "я".repeat(100);
        let once = stable_key("s", "t", "user", &long);
        assert_eq!(once, stable_key("s", "t", "user", &"я".repeat(64)));
    }
}
