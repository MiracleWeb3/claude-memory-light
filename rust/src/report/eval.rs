//! `cml eval` — recall@k measured against your own history, with no labelling.
//!
//! Every claim this project makes about retrieval was an assertion until this
//! existed. The ground truth was in the index the whole time: when you asked
//! something at turn N, the assistant answered at turn N+1, and that answer is
//! by construction the row retrieval should find. Replay the question, and ask
//! whether the answer comes back.
//!
//! The first version scored a perfect 0/282 because it excluded the question's
//! own session — and the answer lives in that session, one turn later, so the
//! ground truth was excluded by construction. Same-session exclusion is a
//! product rule (don't re-inject what is already on screen), not a property of
//! the ranker. The benchmark measures the ranker, so it does not exclude;
//! `--cross-session` puts the rule back and measures something much harder:
//! whether the same question, asked again, finds the session that solved it.

use rusqlite::Connection;

use super::{has, num_arg, row, text};
use crate::text::is_noise;

struct Pair {
    question: String,
    session: String,
    answer: i64, // the assistant reply that followed it
}

/// The ablation switches, as typed. Measurement only — all false is what the
/// hook actually runs, and that is the configuration the numbers describe
/// unless a flag says otherwise.
struct Ablations {
    vectors: bool,
    text_only: bool,
    ungated: bool,
    tools: bool,
}

/// Adjacent user → assistant turns within one session, straight out of the
/// index. The length floors drop pairs that cannot be scored: a two-word ask
/// retrieves nothing, and a one-line answer is an acknowledgement.
fn pairs(conn: &Connection) -> rusqlite::Result<Vec<Pair>> {
    let mut st = conn.prepare(
        "SELECT rowid, role, session, text FROM mem \
         WHERE role IN ('user','assistant') ORDER BY session, ts, rowid",
    )?;
    let rows =
        st.query_map([], |r| Ok((r.get::<_, i64>(0)?, text(r, 1)?, text(r, 2)?, text(r, 3)?)))?;

    let mut pairs = Vec::new();
    let mut prev: Option<(String, String, String)> = None; // role, session, body
    for (rowid, role, session, body) in rows.flatten() {
        if let Some((prev_role, prev_session, prev_body)) = &prev {
            if prev_role == "user"
                && role == "assistant"
                && *prev_session == session
                && !is_noise(prev_body)
                && prev_body.len() >= 20
                && body.len() >= 80
            {
                pairs.push(Pair {
                    question: prev_body.clone(),
                    session: session.clone(),
                    answer: rowid,
                });
            }
        }
        prev = Some((role, session, body));
    }
    Ok(pairs)
}

/// The only place this module touches the ranker: retrieved rowids, best first.
///
/// Everything the benchmark knows about retrieval passes through these four
/// lines, which is the point — `eval` scores the hook's real path, not a copy
/// of it that can drift into scoring something easier.
fn ranked(conn: &Connection, p: &Pair, cross_session: bool, k: usize, ab: &Ablations) -> Vec<i64> {
    let exclude = if cross_session { p.session.as_str() } else { "" };
    let opts = crate::recall::Opts {
        with_vectors: ab.vectors,
        text_only: ab.text_only,
        ungated: ab.ungated,
        tools: ab.tools,
    };
    crate::recall::retrieve(conn, &p.question, exclude, k, opts)
        .iter()
        .map(|hit| hit.rowid)
        .collect()
}

pub fn eval(args: &[String]) -> crate::R<i32> {
    let k = num_arg(args, "-k").unwrap_or(3);
    let limit = num_arg(args, "--limit").unwrap_or(400);
    let cross_session = has(args, "--cross-session");
    let ab = Ablations {
        vectors: has(args, "--vectors"),
        text_only: has(args, "--no-asks"),
        ungated: has(args, "--ungated"),
        tools: has(args, "--tools"),
    };

    let conn = crate::db::open()?;
    let all = pairs(&conn)?;
    if all.is_empty() {
        println!("no question/answer pairs in the index — run `cml index --all` first");
        return Ok(0);
    }

    // Evenly spaced rather than the first N, so one long project cannot be the
    // whole benchmark. Deterministic: same index, same sample, every run.
    let stride = all.len().div_ceil(limit);
    let sample: Vec<&Pair> = all.iter().step_by(stride).collect();

    let (mut fired, mut hit, mut hit_at_1) = (0usize, 0usize, 0usize);
    for p in &sample {
        let hits = ranked(&conn, p, cross_session, k, &ab);
        if hits.is_empty() {
            continue;
        }
        fired += 1;
        if let Some(at) = hits.iter().position(|id| *id == p.answer) {
            hit += 1;
            if at == 0 {
                hit_at_1 += 1;
            }
        }
    }

    let pct = |n: usize, d: usize| if d == 0 { 0.0 } else { 100.0 * n as f64 / d as f64 };
    // Every ablation that moves the numbers is named on this line. A run with
    // the gates off measures a different system than the hook, and a number
    // that does not say so next to itself gets quoted as the hook's.
    let mut legs = vec![
        if ab.vectors { "BM25 + embedding rerank" } else { "BM25 only" },
        if ab.text_only { "text column only (no expansions)" } else { "text + asks" },
    ];
    if ab.ungated {
        legs.push("gates OFF (overlap, echo, fusion) — not the hook's configuration");
    }
    if ab.tools {
        legs.push("tools lane (commands and their output)");
    }
    if cross_session {
        legs.push("cross-session (the answer's own session excluded)");
    }
    row("legs", legs.join(", "));
    row(
        "pairs",
        format!("{} sampled of {} question/answer turns in the index", sample.len(), all.len()),
    );
    row(
        "fired",
        format!(
            "{fired} ({:.0}%) — the rest were gated out as asking nothing",
            pct(fired, sample.len())
        ),
    );
    row(
        &format!("recall@{k}"),
        format!(
            "{hit} ({:.1}% of all questions, {:.1}% of the ones it answered)",
            pct(hit, sample.len()),
            pct(hit, fired)
        ),
    );
    row("recall@1", format!("{hit_at_1} ({:.1}% of all questions)", pct(hit_at_1, sample.len())));
    println!("\nthe exact answer row, found from a different session, with no labels.");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert(conn: &Connection, role: &str, session: &str, ts: &str, text: &str) {
        conn.execute(
            "INSERT INTO mem(text, asks, role, project, session, ts, file) \
             VALUES (?1, '', ?2, 'p', ?3, ?4, 'f')",
            (text, role, session, ts),
        )
        .unwrap();
    }

    /// The benchmark's ground truth is the *next* row, and only when both sides
    /// are substantial enough to score. Get this wrong and every number the
    /// benchmark prints describes the wrong rows.
    #[test]
    fn pairs_are_adjacent_question_answer_turns_only() {
        let (dir, conn) = super::super::scratch_db("eval");
        let answer = "x".repeat(80);

        insert(&conn, "user", "s1", "01", "a question long enough to score");
        insert(&conn, "assistant", "s1", "02", &answer);
        // Noise never counts as a question, however long it is.
        insert(&conn, "user", "s1", "03", "<command-message>run the thing</command-message>");
        insert(&conn, "assistant", "s1", "04", &answer);
        // A short answer is an acknowledgement, not an answer.
        insert(&conn, "user", "s1", "05", "another question long enough");
        insert(&conn, "assistant", "s1", "06", "sure");
        // A pair must not straddle a session boundary.
        insert(&conn, "user", "s2", "07", "a question that never got answered here");
        insert(&conn, "assistant", "s3", "08", &answer);

        let found = pairs(&conn).unwrap();
        assert_eq!(found.len(), 1, "{:?}", found.iter().map(|p| &p.question).collect::<Vec<_>>());
        assert_eq!(found[0].question, "a question long enough to score");
        assert_eq!(found[0].session, "s1");
        assert_eq!(found[0].answer, 2, "the answer is the row that followed");

        let _ = std::fs::remove_dir_all(dir);
    }

    /// Sampling must span the whole corpus: the bug it prevents is a benchmark
    /// that only ever measures the oldest project in the index.
    #[test]
    fn sampling_is_evenly_spaced_and_deterministic() {
        let all: Vec<usize> = (0..1000).collect();
        let stride = all.len().div_ceil(400);
        let sample: Vec<&usize> = all.iter().step_by(stride).collect();
        assert_eq!(stride, 3);
        assert_eq!(*sample[0], 0);
        assert!(*sample[sample.len() - 1] > 900, "the tail of the index must be sampled");
        assert!(sample.len() <= 400);
    }
}
