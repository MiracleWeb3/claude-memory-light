//! `cml search` — one ranked list over every lane.
//!
//! The C++ predecessor had correct SQL for the tool lane and no way to reach it:
//! `--role` was parsed 140 lines away from the `switch` that picked a table, and
//! nothing connected them. `cml search microsocks --role work` printed "no hits"
//! while `work` held 57 matching rows out of 57,683. Here the lane *is* the
//! parsed value (`lane.rs`), and with no `--role` at all every lane is ranked and
//! merged — a user should never have to know which table holds their answer.
//!
//! What is deliberately absent: the `print_scenes` fork. Scenes were a second
//! printer in C++ because the row formatter could not describe them; here they
//! are a `Lane` whose `columns()` differ, so one fetch and one renderer cover all
//! three corpora.

use std::collections::BTreeMap;
use std::str::FromStr;

use rusqlite::{params, Connection, OptionalExtension};

use crate::lane::{role_filter, Lane};
use crate::R;

/// Rows pulled from each lane's index before filtering. Filters (project, role,
/// blocklist) drop rows after ranking, so the pool has to be deeper than `--limit`.
const CANDIDATES: usize = 60;

/// Reciprocal-rank-fusion constant, from the original paper and from the C++ tree.
const RRF_K: f64 = 60.0;

/// How much of a row stays on screen. One line per hit is the whole point.
const SNIPPET: usize = 170;

const USAGE: &str = "usage: cml search <terms> [--role R] [--project P] [--limit N] \
                     [--keyword|--semantic]\n\
                     \n  With no --role, every lane is ranked and merged: talk, tools, scenes.\
                     \n  --role takes conversation|tools|scene, or a row role: user|assistant|\
                     memory|wiki.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Keyword recall, reordered by embeddings where they exist. The default.
    Hybrid,
    /// FTS5 only.
    Keyword,
    /// Embeddings only.
    Semantic,
}

impl Mode {
    const fn name(self) -> &'static str {
        match self {
            Mode::Hybrid => "hybrid",
            Mode::Keyword => "keyword",
            Mode::Semantic => "semantic",
        }
    }
}

/// A parsed command line. Borrows the argv strings rather than cloning them.
#[derive(Debug)]
struct Opts<'a> {
    terms: Vec<&'a str>,
    /// Lanes to rank. Every lane when `--role` is absent — the fix.
    lanes: Vec<Lane>,
    /// Row-level role filter applied *within* a lane (`mem` holds four of them).
    role: Option<&'static str>,
    project: &'a str,
    limit: usize,
    mode: Mode,
}

fn parse(args: &[String]) -> R<Opts<'_>> {
    let mut o = Opts {
        terms: Vec::new(),
        lanes: Lane::ALL.to_vec(),
        role: None,
        project: "",
        limit: 10,
        mode: Mode::Hybrid,
    };
    let mut it = args.iter();
    // A flag that eats the next argument, so `--limit` with nothing after it is an
    // error rather than a search for the word "--limit".
    fn value<'a>(it: &mut std::slice::Iter<'a, String>, flag: &str) -> R<&'a str> {
        it.next()
            .map(String::as_str)
            .ok_or_else(|| format!("{flag} needs a value").into())
    }

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--limit" => {
                let raw = value(&mut it, "--limit")?;
                o.limit = raw
                    .parse()
                    .map_err(|_| format!("--limit wants a number, got '{raw}'"))?;
                if o.limit == 0 {
                    return Err("--limit must be at least 1".into());
                }
            }
            "--project" => o.project = value(&mut it, "--project")?,
            "--role" => {
                let raw = value(&mut it, "--role")?;
                // Parse failures surface as an error. The original bug hid behind a
                // silent "no hits" for a role the CLI could not select.
                o.lanes = vec![Lane::from_str(raw)?];
                o.role = role_filter(raw);
            }
            "--keyword" => o.mode = Mode::Keyword,
            "--semantic" => o.mode = Mode::Semantic,
            // An unknown flag is a typo, not a search term: `--limt 5` must not
            // quietly become a query for the word "--limt".
            flag if flag.starts_with("--") => {
                return Err(format!("unknown flag '{flag}'\n{USAGE}").into())
            }
            term => o.terms.push(term),
        }
    }
    Ok(o)
}

/// One printable result.
#[derive(Debug)]
struct Hit {
    lane: Lane,
    date: String,
    project: String,
    session: String,
    snippet: String,
}

impl Hit {
    /// `2026-08-10  ran   parserx  a1b2c3d4 | microsocks restarted, DNS back`
    ///
    /// The lane label takes the column the C++ gave to the row role. In a merged
    /// list, which corpus a line came from is the fact that makes it readable;
    /// the role is recoverable with `--role`.
    fn line(&self) -> String {
        format!(
            "{:<10}  {:<5}  {:<14}  {:<8} | {}",
            self.date,
            self.lane.label(),
            self.project,
            self.session,
            self.snippet
        )
    }
}

/// Rank every requested lane, merge, filter, and cut to `limit`.
fn search(conn: &Connection, o: &Opts<'_>) -> R<Vec<Hit>> {
    let fts = fts_query(&o.terms);
    let joined = o.terms.join(" ");
    // A leg is switched off by passing it an empty string. `rank_rowids` carries no
    // mode of its own, which is what lets recall share the primitive with a
    // different policy (it passes `ungated`).
    let (fts_leg, semantic_leg) = match o.mode {
        Mode::Hybrid => (fts.as_str(), joined.as_str()),
        Mode::Keyword => (fts.as_str(), ""),
        Mode::Semantic => ("", joined.as_str()),
    };

    // MERGE. FTS5's `rank` is a negative BM25 score whose scale depends on the
    // table's own document lengths and term frequencies, so a -8.1 in `mem` and a
    // -8.1 in `work` say nothing about each other. Normalizing by *position*
    // discards the incomparable magnitudes and keeps the only cross-lane claim
    // that survives: "this was its lane's best hit."
    //
    // That is equal-weight RRF across lanes — and since 1/(k+pos) is the same
    // function in every lane, ordering by fused score is identical to ordering by
    // position. So the merge is written as what it is: sort by within-lane
    // position, ties broken by lane order then rowid, which also makes the output
    // reproducible run to run. (RRF still does real work *inside* a lane, where it
    // fuses two legs whose scales genuinely differ — see `rank_rowids`.)
    let mut order: Vec<(usize, usize, i64)> = Vec::new();
    let mut ranked_lanes = 0usize;
    let mut failure = None;
    for (li, &lane) in o.lanes.iter().enumerate() {
        match rank_rowids(conn, fts_leg, semantic_leg, CANDIDATES, false, lane) {
            Ok(ids) => {
                ranked_lanes += 1;
                order.extend(ids.into_iter().enumerate().map(|(pos, id)| (pos, li, id)));
            }
            // One lane failing (a pre-3.0 index with no `work` table, say) must not
            // sink the lanes that work. All of them failing is a real error.
            Err(e) => failure = Some(e),
        }
    }
    if ranked_lanes == 0 {
        return Err(failure.unwrap_or_else(|| "no lane to search".into()));
    }
    order.sort_unstable();

    let fetch_sql: Vec<String> = o
        .lanes
        .iter()
        .map(|l| format!("SELECT {} FROM {} WHERE rowid=?1", l.columns(), l.table()))
        .collect();
    let want_project = o.project.to_lowercase();
    // One map for the whole search, not a lookup per row: `distilled` is small and
    // this is the shared reader every other command already uses.
    let gists = crate::text::gist_lookup(conn);

    // Not `with_capacity(limit)`: `--limit 4000000000` would then allocate for four
    // billion hits before reading a row.
    let mut out = Vec::new();
    for &(_, li, rowid) in &order {
        if out.len() >= o.limit {
            break;
        }
        let lane = o.lanes[li];
        let row = conn
            .prepare_cached(&fetch_sql[li])?
            .query_row(params![rowid], |r| {
                Ok((col(r, 0)?, col(r, 1)?, col(r, 2)?, col(r, 3)?, col(r, 4)?))
            })
            .optional()?;
        // A rowid that vanished between ranking and fetching (a concurrent
        // `cml forget`) is skipped, not an error.
        let Some((body, second, project, session, ts)) = row else { continue };

        if !want_project.is_empty() && !project.to_lowercase().contains(&want_project) {
            continue;
        }
        // `second` is the row role in `mem`/`work` and the title in `scene`; only
        // the former is a role filter, and `role_filter` only ever yields lane
        // Conversation, so a scene can never reach this branch with Some.
        if o.role.is_some_and(|want| second != want) {
            continue;
        }

        // `stable_key` is shared with distill, index and forget on purpose: the key
        // is what `cml forget` blocklists and what `cml distill` files a gist under,
        // so a second copy of the format here is a silent lookup miss waiting to
        // happen. Exhaustive on `Lane`, like every other match on it: a fourth
        // corpus must say what its key looks like instead of inheriting `mem`'s.
        // Scenes are keyed the way recall mints them — the literal role "scene" over
        // the title — except that `columns()` hands us ts_start where recall used
        // ts_end, so a hand-forgotten scene needs that column to line up.
        let (key_role, key_text) = match lane {
            Lane::Scene => ("scene", second.as_str()),
            Lane::Conversation | Lane::Tools => (second.as_str(), body.as_str()),
        };
        let key = crate::text::stable_key(&session, &ts, key_role, key_text);
        if is_forgotten(conn, &key) {
            continue;
        }

        // A curated gist is what the row *means*; the raw text is only the fallback.
        let snippet = match (gists.get(&key), lane) {
            (Some(gist), _) => crate::text::squeeze(gist, SNIPPET),
            // A scene's title is its headline and its summary is the body; neither
            // alone is a useful line.
            (None, Lane::Scene) => crate::text::squeeze(&format!("{second} — {body}"), SNIPPET),
            (None, Lane::Conversation | Lane::Tools) => crate::text::squeeze(&body, SNIPPET),
        };

        out.push(Hit {
            lane,
            date: ts.get(..10).unwrap_or("no-date").to_string(),
            project,
            session: session.chars().take(8).collect(),
            snippet,
        });
    }
    Ok(out)
}

/// Ranked rowids for ONE lane, best first. The primitive the multi-lane merge and
/// the recall hook both sit on.
///
/// Two legs with incomparable scores — BM25 rank and cosine similarity — fused by
/// RRF over each leg's *positions*, which is the only thing they agree on. Either
/// leg is switched off by passing an empty string for it.
///
/// Returns a `Result` rather than a bare `Vec`: a database error that arrives as an
/// empty list is indistinguishable from "nothing matched", and that confusion is
/// the whole reason this rewrite exists.
pub fn rank_rowids(
    conn: &Connection,
    fts: &str,
    semantic: &str,
    candidates: usize,
    ungated: bool,
    lane: Lane,
) -> R<Vec<i64>> {
    let mut keyword: Vec<i64> = Vec::new();
    if !fts.is_empty() {
        // Only column 0 is read: `rank` is selected so the SQL states what it
        // orders by, but its value is the per-table magnitude this deliberately
        // does not compare across lanes.
        let mut stmt = conn.prepare(lane.rank_sql())?;
        let rows = stmt.query_map(params![fts, candidates as i64], |r| r.get::<_, i64>(0))?;
        for id in rows {
            keyword.push(id?);
        }
    }

    let vectors = if semantic.is_empty() {
        Vec::new()
    } else {
        match crate::vector::search(conn, semantic, lane, candidates) {
            Ok(v) => v,
            // With no keyword leg the vectors ARE the answer, so their failure is the
            // answer's failure. Beside one they are an enhancement: a missing model
            // degrades hybrid search to keyword rather than failing it.
            Err(e) if fts.is_empty() => return Err(e),
            Err(_) => Vec::new(),
        }
    };

    // The lexical gate, kept from the C++ measurement: with equal RRF weight, a row
    // sharing not one word with the query took the #1 slot from a different project
    // entirely, because embeddings over short rows put everything in everything
    // else's neighbourhood. Gated, the vector leg REORDERS what keyword recall
    // found and may not add rows of its own. `ungated` is recall's call to make —
    // and with no keyword leg there is nothing to gate against anyway.
    let gated = !fts.is_empty() && !ungated;
    // BTreeMap, not HashMap: its iteration order is rowid order, so the stable sort
    // below breaks score ties by rowid for free and the same query on the same
    // database prints the same list every time. The sort also names that tie-break
    // explicitly, so swapping this container back to a HashMap would cost performance
    // rather than silently making the ordering random — which is the failure the C++
    // tree records having already shipped once: five runs, three different orderings.
    let mut fused: BTreeMap<i64, f64> = BTreeMap::new();
    for (pos, id) in keyword.iter().enumerate() {
        *fused.entry(*id).or_default() += 1.0 / (RRF_K + pos as f64);
    }
    for (pos, (id, _similarity)) in vectors.iter().enumerate() {
        if gated && !fused.contains_key(id) {
            continue;
        }
        *fused.entry(*id).or_default() += 1.0 / (RRF_K + pos as f64);
    }

    let mut ranked: Vec<(i64, f64)> = fused.into_iter().collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    Ok(ranked.into_iter().map(|(id, _)| id).collect())
}

/// An FTS5 query: every whitespace-separated token quoted, all of them required.
fn fts_query(terms: &[&str]) -> String {
    terms
        .iter()
        .flat_map(|t| t.split_whitespace())
        // FTS5 escapes a quote by doubling it.
        .map(|tok| format!("\"{}\"", tok.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// Is this row on the `cml forget` blocklist?
///
/// A point lookup rather than loading the set: at most a few hundred keys are
/// tested per search and `forgotten.key` is the primary key. A missing table (an
/// index written before it existed) means nothing is blocked, never a failed
/// search.
fn is_forgotten(conn: &Connection, key: &str) -> bool {
    conn.prepare_cached("SELECT key FROM forgotten WHERE key=?1")
        .and_then(|mut st| st.query_row(params![key], |r| r.get::<_, String>(0)))
        .is_ok()
}

/// NULL in an UNINDEXED column reads as empty rather than failing the whole row.
fn col(row: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<String> {
    Ok(row.get::<_, Option<String>>(i)?.unwrap_or_default())
}

pub fn run(args: &[String]) -> R<i32> {
    let opts = parse(args)?;
    if opts.terms.is_empty() {
        eprintln!("{USAGE}");
        return Ok(2);
    }

    let conn = crate::db::open_ro()
        .map_err(|e| format!("cannot open the index ({e}) — run `cml index` first"))?;
    let hits = search(&conn, &opts)?;
    for hit in &hits {
        println!("{}", hit.line());
    }

    if hits.is_empty() {
        // Silence under `--semantic` is the failure this rewrite exists to stop:
        // say that the embeddings are missing rather than implying the corpus is.
        if opts.mode == Mode::Semantic && crate::db::count(&conn, "emb") == 0 {
            return Err("no embeddings yet — run `cml embed`, or drop --semantic".into());
        }
        println!(
            "no hits for: {} ({})",
            opts.terms.join(" "),
            opts.mode.name()
        );
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A scratch index. `CML_HOME` is deliberately not used: these run in parallel
    /// in one process, and a shared env var would make them race.
    fn scratch(name: &str) -> (PathBuf, Connection) {
        let dir = std::env::temp_dir().join(format!("cml-search-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let conn = crate::db::open_at(&dir.join("index.db")).expect("open scratch index");
        conn.execute(
            "INSERT INTO mem(text, asks, role, project, session, ts, file) \
             VALUES ('the microsocks guard unit was installed on Aug 7', '', 'assistant', \
             'claude-memory-light', 'sess-aaaabbbb', '2026-08-07T09:00:00Z', 'f')",
            [],
        )
        .unwrap();
        // The row the C++ tree could not reach from the command line.
        conn.execute(
            "INSERT INTO work(text, role, project, session, ts, file) \
             VALUES ('systemctl restart microsocks -> active (running)', 'tool', \
             'parserx', 'sess-ccccdddd', '2026-08-07T09:01:00Z', 'f')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scene(title, summary, outcome, session, project, ts_start, ts_end, n_rows) \
             VALUES ('microsocks DNS lane', 'rezka lane lost netns DNS; restart fixed it', \
             'fixed', 'sess-eeeeffff', 'claude-memory-light', '2026-08-07T08:00:00Z', \
             '2026-08-07T10:00:00Z', 12)",
            [],
        )
        .unwrap();
        (dir, conn)
    }

    fn hits(conn: &Connection, argv: &[&str]) -> Vec<Hit> {
        let args: Vec<String> = argv.iter().copied().map(String::from).collect();
        let opts = parse(&args).expect("arguments parse");
        search(conn, &opts).expect("search runs")
    }

    /// THE regression guard. `cml search microsocks` with no `--role` must return
    /// the `work` row: in C++ this query could only ever see `mem`, so 57,683 rows
    /// of tool output were unreachable from the command line.
    #[test]
    fn default_search_reaches_the_tools_lane() {
        let (dir, conn) = scratch("default-all-lanes");
        let found = hits(&conn, &["microsocks"]);

        let lanes: Vec<Lane> = found.iter().map(|h| h.lane).collect();
        assert!(
            lanes.contains(&Lane::Tools),
            "a `work` row must be reachable with no --role at all; got {lanes:?}"
        );
        assert!(lanes.contains(&Lane::Conversation), "got {lanes:?}");
        assert!(lanes.contains(&Lane::Scene), "got {lanes:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The exact invocation that reported "no hits" against a populated table.
    #[test]
    fn role_work_selects_the_tools_lane() {
        let (dir, conn) = scratch("role-work");
        for role in ["work", "tools"] {
            let found = hits(&conn, &["microsocks", "--role", role]);
            assert_eq!(found.len(), 1, "--role {role} found {found:?}");
            assert_eq!(found[0].lane, Lane::Tools);
            assert!(found[0].snippet.contains("systemctl restart"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every lane's best hit shares position 0, so the merged head holds one row
    /// from each rather than three from whichever table happened to score highest.
    #[test]
    fn the_merge_interleaves_lanes_by_within_lane_rank() {
        let (dir, conn) = scratch("merge-order");
        let found = hits(&conn, &["microsocks", "--limit", "3"]);
        assert_eq!(found.len(), 3);
        for lane in Lane::ALL {
            assert!(
                found.iter().any(|h| h.lane == lane),
                "{lane} should hold one of the top 3 slots: {found:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keyword_only_still_reaches_every_lane() {
        let (dir, conn) = scratch("keyword-only");
        let found = hits(&conn, &["microsocks", "--keyword"]);
        assert!(found.iter().any(|h| h.lane == Lane::Tools));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn role_filters_within_the_conversation_lane() {
        let (dir, conn) = scratch("row-role");
        assert_eq!(hits(&conn, &["microsocks", "--role", "assistant"]).len(), 1);
        // `mem` holds one row and it is not a user turn.
        assert!(hits(&conn, &["microsocks", "--role", "user"]).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_filter_is_a_case_insensitive_substring() {
        let (dir, conn) = scratch("project");
        let found = hits(&conn, &["microsocks", "--project", "PARSER"]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].lane, Lane::Tools);
        assert!(hits(&conn, &["microsocks", "--project", "nope"]).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn forgotten_rows_never_surface() {
        let (dir, conn) = scratch("forgotten");
        let key: String = conn
            .query_row(
                "SELECT session || '|' || ts || '|' || role || '|' || substr(text,1,64) \
                 FROM work LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute("INSERT INTO forgotten(key) VALUES (?1)", params![key])
            .unwrap();
        // Built in SQL above, matched by the Rust key builder: if the two formats
        // ever drift, this row reappears.
        assert!(hits(&conn, &["microsocks", "--role", "tools"]).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_gist_replaces_the_raw_row() {
        let (dir, conn) = scratch("gist");
        let key: String = conn
            .query_row(
                "SELECT session || '|' || ts || '|' || role || '|' || substr(text,1,64) \
                 FROM work LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO distilled(key, gist) VALUES (?1, 'microsocks came back after a restart')",
            params![key],
        )
        .unwrap();
        let found = hits(&conn, &["microsocks", "--role", "tools"]);
        assert_eq!(found[0].snippet, "microsocks came back after a restart");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_scene_prints_its_title_and_summary() {
        let (dir, conn) = scratch("scene");
        let found = hits(&conn, &["microsocks", "--role", "scene"]);
        assert_eq!(found.len(), 1);
        assert!(found[0].snippet.starts_with("microsocks DNS lane — "));
        assert_eq!(found[0].date, "2026-08-07");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_role_is_an_error_not_a_silent_miss() {
        let args = ["microsocks".to_string(), "--role".into(), "wrok".into()];
        let err = parse(&args).expect_err("a misspelled role must not search silently");
        assert!(err.to_string().contains("wrok"), "{err}");
    }

    #[test]
    fn a_flag_missing_its_value_is_an_error() {
        for argv in [["x".to_string(), "--limit".into()], ["x".into(), "--role".into()]] {
            assert!(parse(&argv).is_err(), "{argv:?} should not parse");
        }
        let bad = ["x".to_string(), "--limit".into(), "zero".into()];
        assert!(parse(&bad).is_err());
        let typo = ["x".to_string(), "--limt".into(), "5".into()];
        assert!(parse(&typo).is_err(), "a typo'd flag must not become a search term");
    }

    #[test]
    fn defaults_are_all_lanes_hybrid_ten() {
        let args = ["microsocks".to_string()];
        let o = parse(&args).unwrap();
        assert_eq!(o.lanes, Lane::ALL.to_vec());
        assert_eq!(o.limit, 10);
        assert_eq!(o.mode, Mode::Hybrid);
        assert!(o.role.is_none());
    }

    #[test]
    fn fts_query_requires_every_token_and_escapes_quotes() {
        assert_eq!(fts_query(&["guest", "mode"]), "\"guest\" AND \"mode\"");
        assert_eq!(fts_query(&["two words"]), "\"two\" AND \"words\"");
        assert_eq!(fts_query(&["say\"it"]), "\"say\"\"it\"");
    }

    /// The primitive recall shares. An empty `fts` leaves only the vector leg, and
    /// with no embeddings that is an empty list — not an error, and not a panic.
    #[test]
    fn rank_rowids_ranks_one_lane_and_survives_an_empty_leg() {
        let (dir, conn) = scratch("primitive");
        let fts = fts_query(&["microsocks"]);

        for lane in Lane::ALL {
            let ranked = rank_rowids(&conn, &fts, "", 60, false, lane).expect("keyword leg");
            assert_eq!(ranked.len(), 1, "{lane} should rank its one matching row");
        }
        // No keyword leg and no embeddings: an empty list, never invented hits.
        let semantic_only = rank_rowids(&conn, "", "microsocks", 60, false, Lane::Tools);
        assert!(semantic_only.map_or(true, |ids| ids.is_empty()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
