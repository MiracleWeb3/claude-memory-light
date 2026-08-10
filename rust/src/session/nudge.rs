//! `cml nudge` — the SessionStart briefing.
//!
//! Four sections, each earning its slot or not appearing at all: the learning inbox
//! (with its consolidation report, see below), the standing project state, the open
//! loops, and the wiki menu. Everything is budgeted, because a briefing that grows
//! becomes wallpaper and wallpaper is not read.
//!
//! Two things here are deliberate and easy to get backwards:
//!
//! * **The standing brief goes in on every start, resume and compact included.** The
//!   other sections are skipped on resume and compact because the conversation still
//!   carries them — but compaction is the precise moment the context was destroyed,
//!   which makes it the moment state most needs restating, not least.
//! * **The inbox section carries the consolidation report itself**, not an instruction
//!   to go and read the inbox. Telling the model to consolidate is a pull, and a pull
//!   fires about 2% of the time. The report arriving unasked is the whole point.

use crate::recall::hook;

use super::{consolidate, state};
use crate::report::loop_lines;
use crate::paths;

const DEFAULT_THRESHOLD: usize = 5;
const STATE_BUDGET: usize = 1400;
const REPORT_SIGNALS: usize = 12; // enough to act on, short enough to read
const REPORT_BUDGET: usize = 1200;
const WIKI_PAGES: usize = 8;

pub fn nudge(_args: &[String]) -> crate::R<i32> {
    // Two callers, one body: the SessionStart hook, which sends a payload and wants the
    // JSON envelope back, and a human typing `cml nudge`, who has no payload and wants
    // to read the briefing. Guessing wrong costs either a hang or a wall of JSON.
    let payload = hook::read();
    let cwd = match &payload {
        Some(p) => hook::field(p, "cwd").to_string(),
        None => std::env::current_dir().unwrap_or_default().to_string_lossy().into_owned(),
    };
    let cwd = cwd.as_str();
    if cwd.is_empty() {
        return hook::passthrough();
    }
    // resume/compact already carry the conversation; re-injecting the static sections
    // would be the exact bloat this briefing is budgeted against.
    let source = payload.as_ref().map_or("", |p| hook::field(p, "source"));
    let pointer = source == "resume" || source == "compact";

    let mut sections: Vec<String> = Vec::new();
    if let Some(s) = inbox_section(cwd) {
        sections.push(s);
    }
    if let Ok(conn) = crate::db::open() {
        let project = paths::label_for_cwd(cwd);
        let st = state::project_state(&conn, &project, STATE_BUDGET);
        if !st.is_empty() {
            sections.push(st);
        }
        if !pointer {
            // A failed query is not a reason to withhold the rest of the briefing.
            let lines = loop_lines(&conn, 30, 3, None).unwrap_or_default();
            if !lines.is_empty() {
                let mut s =
                    String::from("[cml] open loops — asks that keep coming back unresolved:");
                for l in &lines {
                    s.push_str(" // ");
                    s.push_str(l);
                }
                sections.push(s);
            }
        }
    }
    if !pointer {
        let topics = wiki_topics(WIKI_PAGES);
        if !topics.is_empty() {
            sections.push(format!(
                "[cml] wiki topics on file (recall: cml search --role wiki): {topics}"
            ));
        }
    }
    if sections.is_empty() {
        return if payload.is_some() { hook::passthrough() } else { Ok(0) };
    }

    let mut msg = sections.join("\n\n");
    msg.push_str(&format!(
        " [context injected: {:.1}kB]",
        msg.len() as f64 / 1000.0
    ));
    if payload.is_none() {
        println!("{msg}");
        return Ok(0);
    }
    hook::inject("SessionStart", &msg)
}

/// The learning-loop section: how much raw signal has piled up, and what it says.
///
/// Silent below the threshold — `CML_NUDGE_THRESHOLD` moves it — because a two-line
/// inbox is not a consolidation, it is a Tuesday.
fn inbox_section(cwd: &str) -> Option<String> {
    let threshold = std::env::var("CML_NUDGE_THRESHOLD")
        .ok()
        .and_then(|t| t.parse::<usize>().ok())
        .filter(|t| *t > 0)
        .unwrap_or(DEFAULT_THRESHOLD);

    let project = paths::label_for_cwd(cwd);
    let inboxes = consolidate::collect(Some(&project));
    let n: usize = inboxes.iter().map(|i| i.signals.len()).sum();
    if n < threshold {
        return None;
    }
    let path = inboxes
        .first()
        .map_or_else(|| paths::inbox_path_for(cwd), |i| i.path.clone());

    Some(format!(
        "[claude-memory-light learning loop] {n} raw signals captured since last consolidation \
         ({}). They are grouped below, newest first — nothing was written to memory, and nothing \
         will be: this is a draft for you to approve. Early this session, before deep work: \
         promote the durable ones (a correction, a preference, a non-obvious workflow) into a \
         memory note, anything recurring into CLAUDE.md, then run `cml consolidate --clear` to \
         drop the lines you consolidated. Drop the noise — most lines are nothing. Full-history \
         recall is available via `cml search`.{}",
        path.display(),
        consolidate::brief(&inboxes, REPORT_SIGNALS, REPORT_BUDGET)
    ))
}

/// "name — first summary line" for each wiki page, one flat line, capped.
///
/// The menu tells the model what it *can* recall; the content stays on disk until it is
/// asked for. That asymmetry is the only reason this fits in a briefing at all.
fn wiki_topics(cap: usize) -> String {
    let Ok(entries) = std::fs::read_dir(paths::wiki_dir()) else {
        return String::new();
    };
    let mut pages: Vec<std::path::PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    pages.sort();

    let shown = cap.min(pages.len());
    let mut out = pages
        .iter()
        .take(shown)
        .map(|p| {
            let stem = p.file_stem().unwrap_or_default().to_string_lossy();
            let summary = first_prose_line(p);
            let about = if summary.is_empty() { "(empty)" } else { summary.as_str() };
            format!("{stem} \u{2014} {about}")
        })
        .collect::<Vec<_>>()
        .join(" | ");
    if pages.len() > shown {
        out.push_str(&format!(" | +{} more", pages.len() - shown));
    }
    out
}

/// The first line of a page that is neither blank nor a heading: what the page is about,
/// as opposed to what it is called.
fn first_prose_line(path: &std::path::Path) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    text.lines()
        .map(str::trim_start)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .map_or_else(String::new, |l| crate::text::squeeze(l, 60))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wiki_page_is_named_by_its_first_prose_line() {
        let dir = std::env::temp_dir().join(format!("cml-wiki-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let page = dir.join("touchpad.md");
        std::fs::write(&page, "# Touchpad\n\n   tap-drag needs xinput, not synaptics\nmore\n")
            .unwrap();
        assert_eq!(first_prose_line(&page), "tap-drag needs xinput, not synaptics");

        // A page with nothing but headings has no summary, and must not panic.
        let empty = dir.join("empty.md");
        std::fs::write(&empty, "# only a heading\n").unwrap();
        assert!(first_prose_line(&empty).is_empty());
        assert!(first_prose_line(&dir.join("absent.md")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_threshold_is_read_from_the_environment_but_never_zero() {
        // Documents the parse, not the process: an unset or junk value falls back.
        for (raw, want) in [("12", 12usize), ("0", DEFAULT_THRESHOLD), ("junk", DEFAULT_THRESHOLD)] {
            let parsed = Some(raw.to_string())
                .and_then(|t| t.parse::<usize>().ok())
                .filter(|t| *t > 0)
                .unwrap_or(DEFAULT_THRESHOLD);
            assert_eq!(parsed, want, "{raw}");
        }
    }
}
