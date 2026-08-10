//! `cml doctor` — the whole install, checked end to end, in one screen.
//!
//! Every line here exists because something failed silently once. A key sat in
//! `llm.key` for a month while curation looked "off"; a hook shipped and was
//! never installed while recall reported nothing at all; a `tools` lane held
//! 57,683 rows that no `--role` could select. Doctor's job is to make that class
//! of defect *visible*, so the rule for adding a line is simple: if it can be
//! broken without anything saying so, it gets counted here.

use std::path::PathBuf;

use rusqlite::Connection;

use super::{row, scalar, stats};
use crate::lane::Lane;

pub fn doctor(_args: &[String]) -> crate::R<i32> {
    let transcripts = crate::db::transcripts_dir();
    let data = crate::db::home();
    let dbp = crate::db::db_path();
    // Stat the index before opening it: `open` creates the file, so asking
    // afterwards would report every fresh machine as already indexed.
    let indexed = dbp.is_file();

    // First line on purpose: every number below describes whichever binary is
    // speaking, and the hooks run an installed copy that nothing keeps current.
    row("binary", format!("cml {} at {}", env!("CARGO_PKG_VERSION"), self_exe().display()));
    row("transcripts dir", format!("{} ({})", transcripts.display(), mark(transcripts.is_dir())));
    row("data dir", format!("{} ({})", data.display(), mark(data.is_dir())));
    row("index db", format!("{} ({})", dbp.display(), mark(indexed)));

    let conn = crate::db::open()?;

    row("lanes", lanes(&conn));
    row(
        "graphify",
        match which("graphify") {
            Some(p) => format!("{} (optional structural-memory companion)", p.display()),
            None => "not installed (optional structural-memory companion)".to_string(),
        },
    );
    row("semantic", semantic(&conn));
    row("expansions", expansions(&conn));
    row("curation", curation(&conn));
    let last = curator_last_line();
    if !last.is_empty() {
        row("curator run", last);
    }
    row("recall", recall(&conn));
    println!(
        "hint: keep transcripts forever with \"cleanupPeriodDays\": 3650 \
         in ~/.claude/settings.json"
    );

    if indexed {
        println!("{}", stats::line(&conn, &dbp));
    } else {
        println!("run `cml index` to build the index");
    }
    Ok(0)
}

fn mark(present: bool) -> &'static str {
    if present {
        "ok"
    } else {
        "MISSING"
    }
}

fn self_exe() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("cml"))
}

/// `which`, without spawning a shell. `split_paths` already knows the platform's
/// separator, which is the whole of what the C++ version hand-rolled.
fn which(exe: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(exe))
        .find(|cand| cand.is_file())
}

/// Rows per lane, and whether a human can actually reach each one.
///
/// This is the line the rewrite exists for. The C++ tree indexed 57,683 rows of
/// tool output into a lane whose SQL worked perfectly and which no CLI path
/// could select; nothing was broken enough to report, so nothing did, for
/// months. The reachability check is not a comment about the past — `Display`
/// and `FromStr` are two separate matches in lane.rs and can still drift apart,
/// so doctor asks the same question the user's fingers would: does the name this
/// lane prints parse back into this lane?
fn lanes(conn: &Connection) -> String {
    let counts = Lane::ALL
        .iter()
        .map(|lane| format!("{lane} {}", crate::db::count(conn, lane.table())))
        .collect::<Vec<_>>()
        .join(" · ");
    let stranded = Lane::ALL
        .iter()
        .filter(|lane| !matches!(lane.to_string().parse::<Lane>(), Ok(back) if back == **lane))
        .map(Lane::to_string)
        .collect::<Vec<_>>();

    if stranded.is_empty() {
        format!("{counts} — all reachable from --role")
    } else {
        format!(
            "{counts} — UNREACHABLE from --role: {} (indexed rows no command can select)",
            stranded.join(", ")
        )
    }
}

/// One lane's embedding coverage: vectors that point at a row that still exists,
/// out of the rows in that lane.
struct LaneVecs {
    lane: Lane,
    live: i64,
    rows: i64,
}

/// Embeddings counted against the rows they actually point at, plus the ones
/// that point at nothing.
///
/// The C++ doctor printed `5576/3826 rows embedded` — more vectors than rows —
/// because it counted two tables separately and neither count knew about
/// deletions. A coverage figure that exceeds its own denominator is not
/// coverage, it is a leak, and it read as "over-embedded, fine" for as long as
/// it was printed.
fn vectors(conn: &Connection) -> (Vec<LaneVecs>, i64) {
    let mut per_lane = Vec::new();
    let mut live_total = 0;
    for lane in Lane::ALL {
        // `emb.lane` holds the lane's table name — that is what encode.rs writes.
        let held = scalar(conn, "SELECT count(*) FROM emb WHERE lane=?1", [lane.table()]);
        if held == 0 {
            continue;
        }
        // `lane.table()` is a const fn over an enum, not user input.
        let live = scalar(
            conn,
            &format!(
                "SELECT count(*) FROM emb WHERE lane=?1 \
                 AND rowid_ref IN (SELECT rowid FROM {})",
                lane.table()
            ),
            [lane.table()],
        );
        live_total += live;
        per_lane.push(LaneVecs { lane, live, rows: crate::db::count(conn, lane.table()) });
    }
    (per_lane, crate::db::count(conn, "emb") - live_total)
}

fn semantic(conn: &Connection) -> String {
    let held = crate::db::count(conn, "emb");
    if held == 0 {
        return "off — run `cml embed` once to enable hybrid search".to_string();
    }
    let (per_lane, stale) = vectors(conn);
    let mut line = if per_lane.is_empty() {
        // Every vector is filed under a lane name this build does not know:
        // schema drift between the encoder and the searcher, which would
        // otherwise show up as semantic search quietly returning nothing.
        format!("{held} vectors, none filed under a lane this build knows")
    } else {
        per_lane
            .iter()
            .map(|l| format!("{} {}/{} embedded", l.lane, l.live, l.rows))
            .collect::<Vec<_>>()
            .join(" · ")
    };
    if stale > 0 {
        line += &format!(
            " · {stale} stale (vectors for deleted rows; `cml embed --all` clears them)"
        );
    }
    // Two separate facts, not one guess: nothing records which model wrote the
    // stored vectors, so naming one over them was a lie the moment a second
    // backend existed. This is the id the encoder would use *now* — asked of the
    // encoder itself, so doctor cannot report a model that is not the one an
    // `cml embed` would load.
    line + &format!("; backend now: {}", crate::encode::model::model_id())
}

/// doc2query coverage. Same silent-failure shape as the recall counter: the
/// expansions can sit at zero for every row and nothing else would say so.
fn expansions(conn: &Connection) -> String {
    let with_asks = scalar(conn, "SELECT count(*) FROM mem WHERE asks != ''", []);
    if with_asks == 0 {
        return "none — `cml distill` writes them, needs a curator key".to_string();
    }
    format!("{with_asks}/{} rows carry search phrasings", crate::db::count(conn, "mem"))
}

/// Curation status. Omitting this is how a key that had been sitting in
/// `llm.key` since July read as a missing feature for a month.
fn curation(conn: &Connection) -> String {
    // `distilled` records a verdict either way — a drop demotes rather than
    // deletes — so this is rows JUDGED, not rows kept, and the gist count is the
    // one that says how much of the judging produced something. `forgotten` is a
    // different mechanism: only `cml forget` writes it, so a curator run never
    // moves that number.
    let judged = crate::db::count(conn, "distilled");
    let gists = scalar(conn, "SELECT count(*) FROM distilled WHERE gist != ''", []);
    let forgot = crate::db::count(conn, "forgotten");

    // The curator's own config object, not a second reading of the same
    // environment: a doctor that resolves the key or the model differently from
    // the code that runs describes a curator nobody has.
    let Some(conf) = crate::distill::wire::Conf::load() else {
        return "off — put a key in ~/.claude/claude-memory-light/llm.key (any \
                OpenAI-compatible provider; CML_LLM_URL / CML_LLM_MODEL to configure)"
            .to_string();
    };
    let host = conf
        .url
        .split_once("//")
        .map_or(conf.url.as_str(), |(_, rest)| rest)
        .split('/')
        .next()
        .unwrap_or("?");
    format!(
        "on ({} @ {host}) — {judged} rows judged, {gists} carry a gist, \
         {forgot} hand-forgotten",
        conf.model
    )
}

/// What the curator's last detached run amounted to, in one line.
///
/// A failing run still ends with a cheerful "distilled: kept 0, dropped 0", so
/// the last line is the wrong one to show — it reports the non-event and
/// swallows the cause. A complaint outranks the summary; with no complaint, the
/// summary is the whole story.
fn curator_last_line() -> String {
    let log = std::fs::read_to_string(crate::db::home().join("curator.log")).unwrap_or_default();
    let mut last = "";
    let mut complaint = "";
    for line in log.lines().map(str::trim).filter(|l| !l.is_empty()) {
        last = line;
        if complaint.is_empty() && (line.contains("skipped (") || line.contains("error")) {
            complaint = line;
        }
    }
    let pick = if complaint.is_empty() { last } else { complaint };
    if pick.chars().count() > 160 {
        return pick.chars().take(157).chain("...".chars()).collect();
    }
    pick.to_string()
}

/// Retrieval is the half that fails silently: an index can be perfect, embedded
/// and curated while nothing ever reads it. `cml search` ran in 2% of sessions
/// before recall was hooked and nobody noticed for months, because no number
/// reported it. Zero here means check the hook, not the ranker.
///
/// No ratio on purpose — `mem` counts sessions already on disk, `recalled`
/// counts sessions the hook has run in since it was installed. One over the
/// other looks like a percentage and is not one.
fn recall(conn: &Connection) -> String {
    let sessions = scalar(conn, "SELECT count(DISTINCT session) FROM recalled", []);
    if sessions == 0 {
        return "never fired — is the UserPromptSubmit hook installed? \
                (`/plugin update claude-memory-light`)"
            .to_string();
    }
    format!(
        "fired in {sessions} sessions, {} memories injected",
        crate::db::count(conn, "recalled")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The regression this rewrite is named after: `5576/3826 rows embedded`.
    /// A vector whose row is gone must count as stale, never as coverage.
    #[test]
    fn stale_vectors_are_not_counted_as_coverage() {
        let (dir, conn) = super::super::scratch_db("doctor");
        for i in 1..=3 {
            conn.execute(
                "INSERT INTO mem(text, asks, role, project, session, ts, file) \
                 VALUES (?1, '', 'user', 'p', 's', 't', 'f')",
                [format!("row {i}")],
            )
            .unwrap();
        }
        // Two vectors for live rows, two for rows that no longer exist.
        for rowid in [1_i64, 2, 900, 901] {
            conn.execute(
                "INSERT INTO emb(lane, rowid_ref, dim, v) VALUES ('mem', ?1, 2, x'0000')",
                [rowid],
            )
            .unwrap();
        }

        let (per_lane, stale) = vectors(&conn);
        assert_eq!(per_lane.len(), 1, "only the conversation lane holds vectors");
        assert_eq!(per_lane[0].live, 2, "coverage counts vectors with a row behind them");
        assert_eq!(per_lane[0].rows, 3);
        assert_eq!(stale, 2, "vectors for deleted rows are stale, not coverage");
        assert!(per_lane[0].live <= per_lane[0].rows, "coverage cannot exceed its denominator");

        let line = semantic(&conn);
        assert!(line.starts_with("conversation 2/3 embedded · 2 stale"), "{line}");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn empty_index_reports_off_rather_than_zero_over_zero() {
        let (dir, conn) = super::super::scratch_db("doctor-empty");
        assert!(semantic(&conn).starts_with("off — run `cml embed`"));
        assert!(recall(&conn).starts_with("never fired"));
        assert!(expansions(&conn).starts_with("none —"));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Doctor must report the lane defect, not reproduce it.
    #[test]
    fn every_lane_is_counted_and_reported_reachable() {
        let (dir, conn) = super::super::scratch_db("doctor-lanes");
        let line = lanes(&conn);
        for lane in Lane::ALL {
            assert!(line.contains(&format!("{lane} 0")), "{lane} missing from: {line}");
        }
        assert!(line.ends_with("— all reachable from --role"), "{line}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
