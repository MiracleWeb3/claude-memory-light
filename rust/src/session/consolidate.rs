//! `cml consolidate` — turn the raw learning inbox into a report you can act on.
//!
//! The gap this closes: durable memory was only ever written when the assistant *chose*
//! to consolidate, and a capability that depends on the model choosing to invoke it
//! fires about 2% of the time — the same measurement that moved retrieval into a hook.
//! So the report is not something to remember to ask for. It is produced and surfaced
//! by the SessionStart briefing (see `nudge.rs`), which is the one moment it can change
//! what happens next.
//!
//! Push means "the report arrives without being asked". It does **not** mean files are
//! rewritten behind anyone's back, and that boundary is deliberate: unreviewed
//! auto-written memory is exactly how a memory system poisons itself, one plausible
//! wrong line at a time, with no moment at which anybody looked. So nothing here writes
//! a memory file. It reads the inbox, groups it, resolves the files each signal touched
//! against the memory notes that actually exist, and prints. `--clear` drops the lines
//! it consolidated, and only after the report has been emitted.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::paths;
use crate::text::squeeze;

/// Flags in the order the report presents them. Anything else sorts after these.
const FLAG_ORDER: [&str; 3] = ["correction?", "skill?", "note"];

const TEXT_CHARS: usize = 220;

/// One captured turn, as the Stop hook wrote it.
pub struct Signal {
    pub ts: String,
    pub flag: String,
    pub text: String,
    /// Files the turn touched, which is the closest thing the inbox has to a target.
    pub touches: Vec<String>,
    pub project: String,
}

/// One project's inbox, split into what a report consumes and what it must leave alone.
pub struct Inbox {
    pub path: PathBuf,
    /// Lines that are not signals — a title, a hand-written note. `--clear` keeps these.
    pub keep: Vec<String>,
    pub signals: Vec<Signal>,
}

/// Parse an inbox file's text. Entry lines look like
/// `- [2026-08-07 06:59] (correction?) text`, optionally followed by indented detail
/// lines carrying `files:` / `ran:` / `skills:` / `did:`.
pub fn parse(text: &str, project: &str) -> Inbox {
    let mut keep = Vec::new();
    let mut signals: Vec<Signal> = Vec::new();
    for line in text.lines() {
        if let Some(sig) = parse_entry(line, project) {
            signals.push(sig);
            continue;
        }
        // An indented continuation belongs to the entry above it.
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(last) = signals.last_mut() {
                last.touches.extend(files_of(line));
                continue;
            }
        }
        if !line.trim().is_empty() {
            keep.push(line.to_string());
        }
    }
    Inbox { path: PathBuf::new(), keep, signals }
}

fn parse_entry(line: &str, project: &str) -> Option<Signal> {
    let rest = line.trim_start().strip_prefix("- [")?;
    let (ts, rest) = rest.split_once("] ")?;
    let rest = rest.strip_prefix('(')?;
    let (flag, rest) = rest.split_once(") ")?;
    // The capture hook writes targets on the indented line below (see `files_of`), but a
    // hand-written line may append them inline. Split them off rather than leaving them
    // glued to the text: the targets are the actionable half.
    let (text, touches) = match rest.split_once("[\u{2192} may touch:") {
        Some((before, after)) => {
            let list = after.split_once(']').map_or(after, |(l, _)| l);
            (before, split_list(list))
        }
        None => (rest, Vec::new()),
    };
    Some(Signal {
        ts: ts.to_string(),
        flag: flag.to_string(),
        text: text.trim().to_string(),
        touches,
        project: project.to_string(),
    })
}

/// The `files:` field of a detail line, which is what a signal actually touched.
fn files_of(line: &str) -> Vec<String> {
    let Some((_, rest)) = line.split_once("files: ") else {
        return Vec::new();
    };
    // Fields are separated by ' · '; take only this one.
    split_list(rest.split(" \u{b7} ").next().unwrap_or(rest))
}

fn split_list(list: &str) -> Vec<String> {
    list.split(',')
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty())
        .collect()
}

/// Every inbox on disk, or just one project's.
pub fn collect(project: Option<&str>) -> Vec<Inbox> {
    let dir = paths::inbox_dir();
    let mut paths_found: Vec<PathBuf> = match project {
        Some(p) => vec![dir.join(format!("{p}.md"))],
        None => {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                return Vec::new();
            };
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "md"))
                .collect()
        }
    };
    paths_found.sort();

    paths_found
        .into_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            let label = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
            let mut inbox = parse(&text, &label);
            inbox.path = path;
            (!inbox.signals.is_empty()).then_some(inbox)
        })
        .collect()
}

/// Every curated memory note that exists, by file name, so a `files:` target can be
/// told apart from a scratch file that was never memory in the first place.
pub fn memory_notes() -> HashSet<String> {
    let mut out = HashSet::new();
    for dir in paths::memory_dirs() {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(".md") {
                out.insert(name);
            }
        }
    }
    out
}

/// The grouped report body: signals newest first, grouped by flag.
pub fn report(inboxes: &[Inbox], limit: usize, notes: &HashSet<String>) -> String {
    let mut all: Vec<&Signal> = inboxes.iter().flat_map(|i| i.signals.iter()).collect();
    all.sort_by(|a, b| b.ts.cmp(&a.ts));
    let total = all.len();
    all.truncate(limit);
    // Say what was left out. A report that silently shows 12 of 60 reads as "there were
    // 12", and the next `--clear` then retires 60 lines nobody saw.
    let elided = if total > all.len() {
        format!("\n(showing the {} most recent of {total})", all.len())
    } else {
        String::new()
    };

    let mut flags: Vec<&str> = Vec::new();
    for s in &all {
        if !flags.contains(&s.flag.as_str()) {
            flags.push(&s.flag);
        }
    }
    flags.sort_by_key(|f| FLAG_ORDER.iter().position(|k| k == f).unwrap_or(FLAG_ORDER.len()));

    let mut out = elided;
    for flag in flags {
        let group: Vec<&&Signal> = all.iter().filter(|s| s.flag == flag).collect();
        out.push_str(&format!("\n{flag} ({})", group.len()));
        for s in group {
            out.push_str(&format!(
                "\n\u{b7} {} \u{b7} {} \u{b7} {}",
                s.ts,
                s.project,
                squeeze(&s.text, TEXT_CHARS)
            ));
            let targets = resolve(s, notes);
            if !targets.is_empty() {
                out.push_str(&format!("\n    \u{2192} may touch: {}", targets.join(", ")));
            }
        }
    }
    out
}

/// Which of a signal's touched files are memory notes, and which are not. Both are
/// worth seeing: "no memory note" is where a new one would go.
fn resolve(s: &Signal, notes: &HashSet<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    s.touches
        .iter()
        .filter(|f| f.ends_with(".md"))
        .filter(|f| seen.insert(f.to_string()))
        .map(|f| {
            if notes.contains(f) {
                format!("{f} (on file)")
            } else {
                format!("{f} (new)")
            }
        })
        .take(4)
        .collect()
}

/// The report the SessionStart briefing embeds, capped to `budget` bytes so a briefing
/// never becomes wallpaper.
pub fn brief(inboxes: &[Inbox], limit: usize, budget: usize) -> String {
    let body = report(inboxes, limit, &memory_notes());
    if body.len() <= budget {
        return body;
    }
    let mut cut = budget;
    while cut > 0 && !body.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n\u{2026} (`cml consolidate` for the rest)", &body[..cut])
}

/// Drop every consolidated signal, keeping any hand-written lines the file also held.
fn clear(inbox: &Inbox) -> std::io::Result<()> {
    let mut kept = inbox.keep.join("\n");
    if !kept.is_empty() {
        kept.push('\n');
    }
    std::fs::write(&inbox.path, kept)
}

/// The project a bare `cml consolidate` is about: the session's, when a hook payload is
/// on stdin, else the shell's. Scoping to one project by default is what keeps the
/// report about the work in front of you; `--all` opts into every inbox on disk.
fn current_project() -> String {
    if let Some(payload) = crate::recall::hook::read() {
        let cwd = crate::recall::hook::field(&payload, "cwd");
        if !cwd.is_empty() {
            return paths::label_for_cwd(cwd);
        }
    }
    super::state::current_project()
}

pub fn consolidate(args: &[String]) -> crate::R<i32> {
    let mut project: Option<String> = None;
    let mut limit = 40;
    let mut do_clear = false;
    let mut all = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--project" => project = it.next().cloned(),
            "--limit" => {
                if let Some(v) = it.next().and_then(|v| v.parse::<usize>().ok()).filter(|v| *v > 0) {
                    limit = v;
                }
            }
            "--clear" => do_clear = true,
            "--all" => all = true,
            _ => {}
        }
    }
    // Clearing must never drop a line the report did not show.
    if do_clear {
        limit = usize::MAX;
    }
    if !all && project.is_none() {
        project = Some(current_project());
    }

    let inboxes = collect(project.as_deref());
    if inboxes.is_empty() {
        println!("no signals in the learning inbox ({})", paths::inbox_dir().display());
        return Ok(0);
    }
    let total: usize = inboxes.iter().map(|i| i.signals.len()).sum();

    println!(
        "[cml consolidate] {total} signal(s) across {} inbox(es), newest first. Nothing below is \
         written to memory automatically — it is a draft for you to approve.",
        inboxes.len()
    );
    println!("{}", report(&inboxes, limit, &memory_notes()));
    println!(
        "\nApply: promote the durable ones into a memory note under {}, add its one-line pointer \
         to MEMORY.md, and anything recurring into CLAUDE.md.",
        first_memory_dir()
    );
    if do_clear {
        let mut cleared = 0;
        for inbox in &inboxes {
            match clear(inbox) {
                Ok(()) => cleared += inbox.signals.len(),
                Err(e) => eprintln!("cml: cannot clear {}: {e}", inbox.path.display()),
            }
        }
        println!("cleared {cleared} consolidated signal(s) from {} inbox(es)", inboxes.len());
    } else {
        println!("Then `cml consolidate --clear` to drop the {total} consolidated line(s).");
    }
    Ok(0)
}

fn first_memory_dir() -> String {
    paths::memory_dirs()
        .first()
        .map_or_else(|| "~/.claude/projects/-/memory".to_string(), |p| p.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const INBOX: &str = "\
# Learning inbox
- [2026-08-07 06:59] (correction?) no, the work lane was never reachable from the CLI
    files: reference-repowise.md, search.cpp  \u{b7}  ran: cml search --role work  \u{b7}  2 failed
    did: fixed the lane enum
- [2026-08-09 21:00] (note) port the recall hook to rust
    files: recall.rs
- [2026-08-08 12:00] (skill?) always run the tests before claiming done
";

    #[test]
    fn an_entry_line_parses_into_a_signal_with_its_targets() {
        let inbox = parse(INBOX, "cml");
        assert_eq!(inbox.signals.len(), 3);
        assert_eq!(inbox.keep, vec!["# Learning inbox"], "a hand-written title is not a signal");

        let first = &inbox.signals[0];
        assert_eq!(first.ts, "2026-08-07 06:59");
        assert_eq!(first.flag, "correction?");
        assert_eq!(first.text, "no, the work lane was never reachable from the CLI");
        assert_eq!(first.touches, vec!["reference-repowise.md", "search.cpp"]);
        assert_eq!(first.project, "cml");
        // `ran:` and `did:` are detail, not targets.
        assert_eq!(inbox.signals[1].touches, vec!["recall.rs"]);
    }

    /// Continuation lines and the record of earlier passes are not signals. If either
    /// parsed as one, `--clear` would delete the record of what was already decided.
    #[test]
    fn detail_lines_and_consolidation_records_are_not_signals() {
        for line in [
            "    ran: mullvad status | pgrep -af brave",
            "    did: checked the proxy",
            "<!-- consolidated through 2026-07-19: wrote websearch.md -->",
            "# Learning inbox",
        ] {
            assert!(parse_entry(line, "p").is_none(), "should not parse: {line}");
        }
        let survivors = parse(
            "<!-- consolidated through 2026-07-19 -->\n- [2026-08-01 10:00] (note) something\n",
            "p",
        );
        assert_eq!(survivors.signals.len(), 1);
        assert_eq!(survivors.keep, vec!["<!-- consolidated through 2026-07-19 -->"]);
    }

    #[test]
    fn the_inline_target_form_is_parsed_too() {
        // The C++ capture never wrote this form, but a hand-edited line may.
        let inbox = parse(
            "- [2026-08-07 06:59] (correction?) you missed the lane [\u{2192} may touch: a.md, MEMORY.md]",
            "p",
        );
        assert_eq!(inbox.signals[0].touches, vec!["a.md", "MEMORY.md"]);
    }

    #[test]
    fn junk_lines_are_kept_not_parsed_and_never_panic() {
        for junk in ["", "- [", "- [no closing", "- [ts] no flag", "    orphan continuation"] {
            let inbox = parse(junk, "p");
            assert!(inbox.signals.is_empty(), "{junk:?} is not a signal");
        }
        // A truncated inbox still yields the entries that were complete.
        let inbox = parse("- [2026-08-09 21:00] (note) fine\n- [broken", "p");
        assert_eq!(inbox.signals.len(), 1);
        assert_eq!(inbox.keep, vec!["- [broken"]);
    }

    #[test]
    fn the_report_groups_by_flag_newest_first_and_resolves_targets() {
        let inbox = parse(INBOX, "cml");
        let notes: HashSet<String> = ["reference-repowise.md".to_string()].into_iter().collect();
        let out = report(&[inbox], 40, &notes);

        let correction = out.find("correction? (1)").expect("corrections lead");
        let skill = out.find("skill? (1)").expect("skills follow");
        let note = out.find("note (1)").expect("notes last");
        assert!(correction < skill && skill < note, "flag order is priority order: {out}");

        assert!(out.contains("\u{b7} 2026-08-09 21:00 \u{b7} cml \u{b7} port the recall hook to rust"));
        assert!(out.contains("\u{2192} may touch: reference-repowise.md (on file)"));
        // A file that is not markdown is not a memory target, and never shown as one.
        assert!(!out.contains("search.cpp"));
        assert!(!out.contains("recall.rs"));
    }

    #[test]
    fn the_briefing_copy_is_capped() {
        let inbox = parse(INBOX, "cml");
        let short = brief(&[inbox], 40, 80);
        assert!(short.len() < 200);
        assert!(short.ends_with("(`cml consolidate` for the rest)"));
    }

    #[test]
    fn clearing_keeps_the_hand_written_lines() {
        let dir = std::env::temp_dir().join(format!("cml-clear-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cml.md");
        std::fs::write(&path, INBOX).unwrap();

        let mut inbox = parse(INBOX, "cml");
        inbox.path = path.clone();
        clear(&inbox).unwrap();

        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, "# Learning inbox\n");
        assert!(parse(&after, "cml").signals.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
