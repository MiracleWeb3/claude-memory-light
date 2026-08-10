//! `cml state` — standing context for a project: what is still open, and what recently
//! got done.
//!
//! Not an answer to anything, which is why it says so in its own header. It is also the
//! one section of the SessionStart briefing that goes in on *every* start, resume and
//! compact included: compaction is the precise moment the context was destroyed, which
//! makes it the moment state most needs restating, not least.
//!
//! What this deliberately does not do is re-surface the newest hand-written memory
//! descriptions. The harness already injects MEMORY.md and the wiki index natively, so
//! that only duplicated them — and a newest-first dump of memory descriptions puts
//! whatever was last worked on into every prompt. cml's unique standing signal is what
//! MEMORY.md does not track: cross-session recurrence, and recent outcomes.

use std::collections::HashMap;

use rusqlite::Connection;

use crate::text::{gist_lookup, squeeze, stable_key};

use crate::paths;
use crate::report::loops;

const LINE: usize = 140; // one row, one line
const PER_SECTION: usize = 3; // a briefing, not a report
const DEFAULT_BUDGET: usize = 1400;

/// Rows that are durable enough to be worth restating: the model's and the user's own
/// words, newest first. Over-fetched, because most of them have no curated gist.
const DURABLE: &str = "SELECT ts, role, session, substr(text,1,400), substr(text,1,64) FROM mem \
                       WHERE role IN ('assistant','user') AND project=?1 ORDER BY ts DESC LIMIT ?2";

/// A memory file leads with YAML frontmatter, so the first 140 characters of one are
/// "--- name: ... description:" — the header, not the fact. The description field IS the
/// one-line summary the file was written with, so use it and skip the rest.
fn essence(text: &str) -> &str {
    if !text.starts_with("---") {
        return text;
    }
    let Some(d) = text.find("description:") else {
        return text;
    };
    let rest = &text[d + "description:".len()..];
    let start = rest.find(|c: char| !matches!(c, ' ' | '\t' | '"')).unwrap_or(rest.len());
    let line = &rest[start..];
    let end = line.find('\n').unwrap_or(line.len());
    line[..end].trim_end_matches(['"', ' '])
}

/// The learning inbox is a scratch pad the Stop hook appends to, indexed like any other
/// markdown under the memory dir. It is raw signal awaiting consolidation, never a
/// standing rule, and it would otherwise win a slot every session by being newest.
fn is_scratch(text: &str) -> bool {
    text.starts_with("# Learning inbox")
}

/// Prefer the curated gist over the raw row: it is the reusable essence in 120 chars,
/// which is exactly what a standing brief wants.
fn pick(
    conn: &Connection,
    project: &str,
    n: usize,
    gists: &HashMap<String, String>,
    require_gist: bool,
) -> Vec<String> {
    let over_fetch = (n * if require_gist { 12 } else { 1 }) as i64;
    let Ok(mut st) = conn.prepare(DURABLE) else {
        return Vec::new();
    };
    let Ok(rows) = st.query_map(rusqlite::params![project, over_fetch], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
        ))
    }) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for (ts, role, session, text, head) in rows.flatten() {
        if out.len() >= n {
            break;
        }
        if is_scratch(&text) {
            continue;
        }
        let gist = gists.get(&stable_key(&session, &ts, &role, &head));
        if require_gist && gist.is_none() {
            continue;
        }
        let body = gist.map_or_else(|| essence(&text), String::as_str);
        if body.len() < 20 {
            continue;
        }
        let day: String = ts.chars().take(10).collect();
        out.push(format!(
            "{} {}",
            if day.len() >= 10 { day } else { "?".to_string() },
            squeeze(body, LINE)
        ));
    }
    out
}

fn section(out: &mut String, label: &str, lines: &[String], budget: usize) {
    for l in lines {
        if out.len() + l.len() + 16 > budget {
            return;
        }
        out.push('\n');
        out.push_str(label);
        out.push_str(" \u{b7} ");
        out.push_str(l);
    }
}

/// The standing brief for one project, or nothing at all when there is nothing on file.
/// A header with no sections under it is noise, so it is not emitted either.
pub fn project_state(conn: &Connection, project: &str, budget: usize) -> String {
    if project.is_empty() {
        return String::new();
    }
    let counted: Option<(i64, String)> = conn
        .query_row(
            "SELECT count(DISTINCT session), coalesce(max(ts),'') FROM mem WHERE project=?1",
            [project],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let Some((sessions, last)) = counted else {
        return String::new();
    };
    if sessions == 0 {
        return String::new(); // nothing on file: say nothing rather than a stub
    }

    let when = if last.len() >= 10 {
        format!(", last {}", last.chars().take(10).collect::<String>())
    } else {
        String::new()
    };
    let mut out = format!(
        "[cml state] {project} — {sessions} session(s) on file{when}. \
         Standing context, not an answer to anything:"
    );
    let head = out.len();

    // Recurrence is measured across sessions, so it is the one signal that says "this is
    // still not fixed" without anyone having to mark it as such.
    // A failed query drops this section rather than the whole brief.
    let open: Vec<String> = loops::loop_lines(conn, 45, PER_SECTION, Some(project))
        .unwrap_or_default()
        .iter()
        .map(|l| squeeze(l, LINE))
        .collect();
    section(&mut out, "open", &open, budget);
    let gists = gist_lookup(conn);
    section(&mut out, "done", &pick(conn, project, PER_SECTION, &gists, true), budget);

    if out.len() == head {
        return String::new();
    }
    out
}

/// The project a bare `cml state` is about: the one the shell is standing in.
pub fn current_project() -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    paths::label_for_cwd(&cwd.to_string_lossy())
}

pub fn state(args: &[String]) -> crate::R<i32> {
    let mut project = String::new();
    let mut budget = DEFAULT_BUDGET;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--project" => project = it.next().cloned().unwrap_or_default(),
            "--budget" => {
                if let Some(v) = it.next().and_then(|v| v.parse::<usize>().ok()).filter(|v| *v > 0) {
                    budget = v;
                }
            }
            _ => {}
        }
    }
    if project.is_empty() {
        project = current_project();
    }
    let conn = crate::db::open().map_err(|e| format!("cannot open index: {e}"))?;
    let s = project_state(&conn, &project, budget);
    if s.is_empty() {
        println!("no state on file for project '{project}'");
    } else {
        println!("{s}");
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::ensure_schema(&conn).unwrap();
        conn
    }

    fn add(conn: &Connection, text: &str, role: &str, session: &str, ts: &str) {
        conn.execute(
            "INSERT INTO mem(text, role, project, session, ts, file) VALUES (?1, ?2, 'cml', ?3, ?4, 'f')",
            rusqlite::params![text, role, session, ts],
        )
        .unwrap();
    }

    #[test]
    fn nothing_on_file_says_nothing() {
        let conn = seeded();
        assert!(project_state(&conn, "cml", 1400).is_empty());
        assert!(project_state(&conn, "", 1400).is_empty());
    }

    #[test]
    fn a_header_with_no_sections_is_not_emitted() {
        let conn = seeded();
        // One ungisted row in one session: nothing recurs, nothing is curated.
        add(&conn, "a single passing remark", "user", "s1", "2026-08-01T10:00:00Z");
        assert!(project_state(&conn, "cml", 1400).is_empty(), "a bare header is noise");
    }

    #[test]
    fn open_loops_and_done_gists_get_their_prefixes() {
        let conn = seeded();
        let today = paths::iso_date(paths::now_secs());
        let ask = "why is equalizing windows in kitty still not working at all";
        add(&conn, ask, "user", "s1", &format!("{today}T09:00:00Z"));
        add(&conn, ask, "user", "s2", &format!("{today}T10:00:00Z"));

        let text = "the fix was to rebuild the kitty config from the packaged default";
        add(&conn, text, "assistant", "s2", &format!("{today}T11:00:00Z"));
        let head: String = text.chars().take(64).collect();
        conn.execute(
            "INSERT INTO distilled(key, gist) VALUES(?1, ?2)",
            rusqlite::params![
                stable_key("s2", &format!("{today}T11:00:00Z"), "assistant", &head),
                "kitty equalize needs the packaged config, not the hand-edited one"
            ],
        )
        .unwrap();

        let s = project_state(&conn, "cml", 1400);
        assert!(s.starts_with("[cml state] cml — 2 session(s) on file, last "), "{s}");
        assert!(s.contains("Standing context, not an answer to anything:"));
        assert!(s.contains("\nopen \u{b7} carried across 2 sessions since "), "{s}");
        assert!(s.contains("\ndone \u{b7} "), "{s}");
        assert!(s.contains("kitty equalize needs the packaged config"), "{s}");
    }

    #[test]
    fn the_budget_stops_the_brief_growing() {
        let conn = seeded();
        let today = paths::iso_date(paths::now_secs());
        add(&conn, "a much longer standing remark that would be printed", "user", "s1", &format!("{today}T09:00:00Z"));
        add(&conn, "a much longer standing remark that would be printed", "user", "s2", &format!("{today}T10:00:00Z"));
        // A budget below the header leaves no room for any line at all.
        assert!(project_state(&conn, "cml", 10).is_empty());
    }

    #[test]
    fn frontmatter_yields_its_description_not_its_header() {
        let text = "---\nname: touchpad-fix\ndescription: tap-drag needs xinput, not synaptics\nmetadata:\n---\nbody";
        assert_eq!(essence(text), "tap-drag needs xinput, not synaptics");
        assert_eq!(essence("plain row text"), "plain row text");
        assert_eq!(essence("---\nno description here\n"), "---\nno description here\n");
    }
}
