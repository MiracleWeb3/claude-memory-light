//! The retrieval core: every gate except per-session dedupe.
//!
//! Measured across 564 transcripts before this existed: the write side, which is
//! hooked, ran 625 times. The read side, which was not, ran 20 times — `cml search` in
//! 12 sessions, 2%. Storage was never the gap. Retrieval being a decision the model had
//! to remember to make was the gap, and a decision made 2% of the time is the same as
//! no feature at all. So this fires on every prompt and the model is not consulted.
//!
//! What it must not become is wallpaper. Four gates keep it quiet: a prompt with fewer
//! than two content words asks nothing; a term the index is saturated with is not
//! evidence; a row sharing fewer than two terms is a coincidence; and a row that merely
//! echoes the prompt back says nothing that is not already on screen. The fifth gate —
//! a row already injected this session — belongs to the hook, in `recall.rs`.
//!
//! BM25 only, deliberately. `cml eval` measured the embedding leg making *this path*
//! worse in every configuration: -5 and -9 rows with potion-base-8M, -2 and -3 with a
//! real bge-small transformer, and unchanged when ungated — because the two-shared-word
//! floor below is itself a lexical requirement, so a purely semantic match cannot
//! survive the pipeline however the fusion is arranged. The C++ tree kept a
//! `with_vectors` switch here and a fusion path that the hook never took; it does not
//! move. Vectors still serve `cml search --semantic`, where they do something BM25
//! cannot: "trackpad dragging" finds touchpad rows.

use std::collections::HashMap;

use rusqlite::Connection;

use crate::lane::Lane;
use crate::text::{content_terms, gist_lookup, squeeze, stable_key, term_overlap};

const SNIPPET: usize = 160; // ~3 lines of context total
const MIN_TERMS: usize = 2; // "yes" and "do it" recall nothing
const MIN_OVERLAP: usize = 2; // one shared word is a coincidence, two is a topic
const CANDIDATES: i64 = 40;
const ECHO_MIN: usize = 25; // shorter than this, containment means nothing

// Rarity gate. Measured before it existed: 76% of real prompts fired, most of them on
// words like "code", "problem" or "same" — content words by any stopword list, and
// worthless in an index where every row is about code. BM25 discounts them when it
// ranks; the overlap gate does not, so two such words were enough to drag in a row from
// an unrelated project.
const MIN_CORPUS: i64 = 200; // below this, rarity cannot be judged at all
const DF_PERCENT: i64 = 6;
const DF_FLOOR: i64 = 5; // ...but never call a 5-row term common

/// One line, ready to inject.
pub struct Hit {
    pub rowid: i64,
    /// "2026-07-08 memory/project: text…", or for a scene "title (outcome, date)".
    pub line: String,
    /// `stable_key`, so per-session dedupe survives a reindex changing rowids.
    pub key: String,
}

/// Ablation switches for `cml eval`. The defaults are what the hook runs.
#[derive(Default, Clone, Copy)]
pub struct Opts {
    /// Search `work` — commands and their output — instead of conversation.
    pub tools: bool,
    /// Confine the FTS5 match to the text column, which is how the index behaves with
    /// no doc2query expansions, without having to throw them away to find out.
    pub text_only: bool,
    /// Drop the precision gates (overlap, echo) and report what BM25 alone ranked.
    ///
    /// The gates are what make the hook quiet enough to run on every prompt, so nothing
    /// in production sets this. `cml eval` does, because "how much do the gates cost in
    /// recall" is only answerable by running without them.
    pub ungated: bool,
    /// Fuse an embedding ranking into the BM25 one.
    ///
    /// The hook never sets this — the module header records why, and the header's claim
    /// is exactly what `cml eval --vectors` re-measures. Keeping the switch real is the
    /// difference between a measurement that can be repeated and a comment that has to
    /// be believed; an ablation flag wired to nothing would report a comparison it never
    /// performed.
    pub with_vectors: bool,
}

/// FTS5 OR query. A prompt is not a conjunction: requiring every word (which is what
/// `cml search` does, correctly, for a hand-typed query) matches nothing when the query
/// is a whole sentence. BM25 then ranks by how many rare terms a row actually hit.
fn or_query(terms: &[String]) -> String {
    terms
        .iter()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\""))) // FTS5 doubles the quote
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// Keep only terms rare enough to mean something. A term matching nothing is dropped
/// with them: it cannot rank, and it cannot count toward the overlap floor either.
///
/// Rarity is judged by each table's *own* statistics. Using conversation counts for the
/// work lane silently deleted the only terms that lane exists for: "reply.cpp" occurs
/// zero times in anything anyone said, so it was dropped as matching nothing, and the
/// question came back empty with 29,703 tool rows sitting there holding the answer.
fn discriminative(conn: &Connection, terms: &[String], lane: Lane) -> Vec<String> {
    let table = lane.table();
    let total = crate::db::count(conn, table);
    if total < MIN_CORPUS {
        return terms.to_vec(); // a young index: let everything through
    }
    let ceiling = DF_FLOOR.max(total * DF_PERCENT / 100);
    let sql = format!("SELECT count(*) FROM {table} WHERE {table} MATCH ?1");
    let Ok(mut st) = conn.prepare(&sql) else {
        return terms.to_vec();
    };
    terms
        .iter()
        .filter(|t| {
            let n: i64 = st
                .query_row([format!("\"{t}\"")], |r| r.get(0))
                .unwrap_or(0);
            n > 0 && n <= ceiling
        })
        .cloned()
        .collect()
}

/// BM25 candidates for `fts`, best first. A malformed match expression or a missing
/// table yields no candidates rather than an error: this runs on a hook.
fn rank(conn: &Connection, lane: Lane, fts: &str) -> Vec<i64> {
    let Ok(mut st) = conn.prepare(lane.rank_sql()) else {
        return Vec::new();
    };
    let Ok(rows) = st.query_map(rusqlite::params![fts, CANDIDATES], |r| r.get::<_, i64>(0)) else {
        return Vec::new();
    };
    rows.flatten().collect()
}

/// A row that is the prompt itself, asked once before, tells the model nothing that is
/// not already on screen. The answer to it might — the echo never does.
fn echoes(row: &str, prompt_flat: &str) -> bool {
    if prompt_flat.len() < ECHO_MIN {
        return false;
    }
    let a = squeeze(&row.to_ascii_lowercase(), 400);
    a.contains(prompt_flat) || prompt_flat.contains(a.as_str())
}

/// The terms a prompt is worth querying on, or `None` if it asks nothing.
fn query_terms(conn: &Connection, prompt: &str, lane: Lane) -> Option<Vec<String>> {
    // A slash-command envelope or a hook injection is not a question to the index.
    if crate::text::is_noise(prompt) {
        return None;
    }
    let words = content_terms(prompt);
    if words.len() < MIN_TERMS {
        return None;
    }
    let terms = discriminative(conn, &words, lane);
    (terms.len() >= MIN_TERMS).then_some(terms)
}

/// Rows matching `prompt`, best first, with every gate applied except dedupe.
///
/// `exclude_session` drops rows from one session: the live hook excludes the session
/// being typed in (already on screen), and `cml eval` excludes the session a question
/// was asked in (the point is whether the answer is findable from somewhere else).
/// Reciprocal-rank fusion of the BM25 order with an embedding order.
///
/// RRF rather than a weighted sum of scores, because BM25 scores and cosine similarities
/// are on different and non-comparable scales — the same category error as comparing raw
/// FTS5 `rank` values across two tables. Positions are comparable; the numbers are not.
///
/// Degrades to the BM25 order whenever the semantic leg has nothing to say (no model, no
/// embeddings computed yet), so enabling this can reorder results but can never empty
/// them.
fn fuse_vectors(conn: &Connection, prompt: &str, lane: Lane, bm25: Vec<i64>) -> Vec<i64> {
    let want = bm25.len().max(CANDIDATES as usize);
    let Ok(sem) = crate::vector::search(conn, prompt, lane, want) else {
        return bm25;
    };
    if sem.is_empty() {
        return bm25;
    }

    const K: f32 = 60.0; // conventional RRF damping constant
    let mut score: HashMap<i64, f32> = HashMap::new();
    for (i, id) in bm25.iter().enumerate() {
        *score.entry(*id).or_default() += 1.0 / (K + i as f32 + 1.0);
    }
    for (i, (id, _sim)) in sem.iter().enumerate() {
        *score.entry(*id).or_default() += 1.0 / (K + i as f32 + 1.0);
    }

    let mut ids: Vec<i64> = score.keys().copied().collect();
    // Ties break on rowid so the ranking is deterministic run to run; an eval whose
    // ordering wobbles cannot attribute a score change to the thing being ablated.
    ids.sort_by(|a, b| {
        score[b]
            .partial_cmp(&score[a])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(b))
    });
    ids
}

pub fn retrieve(
    conn: &Connection,
    prompt: &str,
    exclude_session: &str,
    limit: usize,
    opts: Opts,
) -> Vec<Hit> {
    let mut out = Vec::new();
    let lane = if opts.tools { Lane::Tools } else { Lane::Conversation };
    let Some(terms) = query_terms(conn, prompt, lane) else {
        return out;
    };

    let mut fts = or_query(&terms);
    if opts.text_only {
        // {text} : (...) restricts the match to one column. FTS5 searches every indexed
        // column by default, which is exactly what makes doc2query work — and what has
        // to be switched off to measure whether it works.
        fts = format!("{{text}} : ({fts})");
    }
    let mut ranked = rank(conn, lane, &fts);
    if ranked.is_empty() {
        return out;
    }
    if opts.with_vectors {
        ranked = fuse_vectors(conn, prompt, lane, ranked);
    }

    let prompt_flat = squeeze(&prompt.to_ascii_lowercase(), 400);
    let gists = gist_lookup(conn);
    // Fetch from the SAME table the ranking came from. This read was pinned to `mem`
    // while the work lane ranked against `work`, so every tool rowid was looked up in
    // the wrong table and silently produced nothing — the lane was wired end to end and
    // returned empty for every query it existed to answer.
    let sql = format!(
        "SELECT ts, role, project, session, substr(text,1,400), substr(text,1,64) \
         FROM {} WHERE rowid=?1",
        lane.table()
    );
    let Ok(mut fetch) = conn.prepare(&sql) else {
        return out;
    };

    for rowid in ranked {
        if out.len() >= limit {
            break;
        }
        let Ok(row) = fetch.query_row([rowid], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        }) else {
            continue;
        };
        let (ts, role, project, session, text, head) = row;

        if !exclude_session.is_empty() && session == exclude_session {
            continue;
        }
        if !opts.ungated && term_overlap(&text, &terms) < MIN_OVERLAP {
            continue;
        }
        if !opts.ungated && echoes(&text, &prompt_flat) {
            continue;
        }

        let key = stable_key(&session, &ts, &role, &head);
        let body = gists.get(&key).unwrap_or(&text);
        let where_ = if project.is_empty() {
            String::new()
        } else {
            format!("/{project}")
        };
        out.push(Hit {
            rowid,
            line: format!("{} {role}{where_}: {}", day(&ts), squeeze(body, SNIPPET)),
            key,
        });
    }
    out
}

/// The best-matching L2 scene for this prompt, or nothing.
///
/// The terms come from `mem`'s rarity statistics, not from `scene`'s, and that is
/// deliberate: there are 72 scenes against `MIN_CORPUS` = 200, so a scene lane judging
/// its own table would take the "young index, let everything through" branch and fire
/// on exactly the prompts the row lanes correctly stay silent on.
pub fn scene_hit(conn: &Connection, prompt: &str, exclude_session: &str) -> Option<Hit> {
    let terms = query_terms(conn, prompt, Lane::Conversation)?;
    let ranked = rank(conn, Lane::Scene, &or_query(&terms));
    let mut fetch = conn
        .prepare("SELECT title, summary, outcome, session, ts_end FROM scene WHERE rowid=?1")
        .ok()?;

    for rowid in ranked {
        let Ok((title, summary, outcome, session, ts)) = fetch.query_row([rowid], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        }) else {
            continue;
        };
        // The live session has no scene yet, but a resumed one does — and its summary
        // is the conversation already on screen.
        if !exclude_session.is_empty() && session == exclude_session {
            continue;
        }
        if term_overlap(&format!("{title} {summary}"), &terms) < MIN_OVERLAP {
            continue;
        }
        return Some(Hit {
            rowid,
            line: format!("{title} ({outcome}, {})", day(&ts)),
            key: stable_key(&session, &ts, "scene", &title),
        });
    }
    None
}

/// The date half of an ISO timestamp, or a marker that says so.
fn day(ts: &str) -> &str {
    if ts.len() >= 10 && ts.is_char_boundary(10) {
        &ts[..10]
    } else {
        "no-date"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::ensure_schema(&conn).unwrap();
        conn
    }

    fn add(conn: &Connection, text: &str, role: &str, session: &str) {
        conn.execute(
            "INSERT INTO mem(text, role, project, session, ts, file) \
             VALUES (?1, ?2, 'proj', ?3, '2026-08-09T10:00:00Z', 'f')",
            rusqlite::params![text, role, session],
        )
        .unwrap();
    }

    #[test]
    fn a_prompt_asking_nothing_retrieves_nothing() {
        let conn = seeded();
        add(&conn, "the touchpad tap-drag fix lives in xinput", "assistant", "s0");
        for empty in ["do it", "ok", "<system-reminder>touchpad tap drag</system-reminder>"] {
            assert!(retrieve(&conn, empty, "", 3, Opts::default()).is_empty());
        }
    }

    #[test]
    fn one_shared_word_is_a_coincidence_two_is_a_topic() {
        let conn = seeded();
        add(&conn, "the touchpad tap-drag fix lives in xinput", "assistant", "s0");
        add(&conn, "touchpad brightness has nothing to do with it", "assistant", "s0");

        let hits = retrieve(&conn, "where is the touchpad tapdrag xinput fix", "", 5, Opts::default());
        assert_eq!(hits.len(), 1, "only the two-term row clears the overlap floor");
        assert!(hits[0].line.contains("xinput"));
        // Shape: "<date> <role>/<project>: <text>"
        assert!(hits[0].line.starts_with("2026-08-09 assistant/proj: "), "{}", hits[0].line);
    }

    #[test]
    fn the_live_session_is_excluded_and_an_echo_is_dropped() {
        let conn = seeded();
        add(&conn, "the touchpad tap-drag fix lives in xinput", "assistant", "live");
        assert!(retrieve(&conn, "touchpad tapdrag xinput fix", "live", 5, Opts::default()).is_empty());

        let conn2 = seeded();
        let prompt = "the touchpad tap-drag fix lives in xinput settings somewhere";
        add(&conn2, prompt, "user", "s0");
        assert!(
            retrieve(&conn2, prompt, "", 5, Opts::default()).is_empty(),
            "a row that is the prompt itself says nothing new"
        );
    }

    #[test]
    fn the_work_lane_is_reachable_and_reads_its_own_table() {
        let conn = seeded();
        conn.execute(
            "INSERT INTO work(text, role, project, session, ts, file) VALUES \
             ('cargo build failed: undefined reference in reply.cpp', 'tool', 'proj', 's0', \
              '2026-08-09T10:00:00Z', 'f')",
            [],
        )
        .unwrap();
        let opts = Opts { tools: true, ..Opts::default() };
        let hits = retrieve(&conn, "what was that undefined reference in reply.cpp", "", 2, opts);
        assert_eq!(hits.len(), 1, "the lane the C++ tree stranded must answer");
        // ...and the conversation lane must not answer it, since the row is not there.
        assert!(retrieve(&conn, "what was that undefined reference in reply.cpp", "", 2, Opts::default()).is_empty());
    }

    #[test]
    fn a_missing_index_yields_no_hits_rather_than_an_error() {
        let conn = Connection::open_in_memory().unwrap(); // no schema at all
        assert!(retrieve(&conn, "touchpad tapdrag xinput fix", "", 3, Opts::default()).is_empty());
        assert!(scene_hit(&conn, "touchpad tapdrag xinput fix", "").is_none());
    }

    #[test]
    fn day_never_slices_a_multibyte_timestamp() {
        assert_eq!(day("2026-08-09T10:00:00Z"), "2026-08-09");
        assert_eq!(day("short"), "no-date");
        assert_eq!(day(""), "no-date");
        // 12 bytes, but byte 10 falls inside a character: better a marker than a panic.
        assert_eq!(day(&"\u{2026}".repeat(4)), "no-date");
    }
}
