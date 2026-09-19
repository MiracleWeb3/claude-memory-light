//! Memory that checks itself against the world before it speaks.
//!
//! Every other memory system — Mem0, Zep, Letta, Graphiti, Engram — stores text
//! and metadata about *when* it was learned. None stores how to find out whether
//! it is still true. Temporal knowledge graphs come closest and still miss the
//! case that matters here: their invalidation fires when a contradicting episode
//! *arrives*, so a deleted flag, which asserts nothing, leaves the fact standing
//! with an open validity window — a stale claim wearing a certificate.
//!
//! Code memory does not rot by contradiction. It rots by the referent dying, in
//! silence. The only thing that catches that is asking the world.
//!
//! Measured on this machine's 332 memory and wiki rows: 241 distinct absolute
//! paths are named, and **64 of them (27%) do not exist**. A quarter of the
//! concrete detail in memory is false and is served as fact, because nothing ever
//! asked.
//!
//! So a claim here is not a string. It is a string plus a predicate that can
//! refute it, and retrieval runs the predicate first: refuted claims are withheld
//! rather than ranked. The predicates are derived from the text already stored —
//! nothing needs rewriting — and each one is a `stat` or a `PATH` lookup, so the
//! whole check costs microseconds and never calls a model.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// What a claim asserts about the world, and how to find out if it still holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// A path the text names as existing.
    PathExists(String),
    /// An executable the text names as runnable.
    CommandExists(String),
}

/// The world's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Truth {
    /// The world agrees. Safe to state.
    Holds,
    /// The world disagrees. The claim is false *now*, whatever it was then.
    Refuted,
    /// The probe cannot see, so it cannot judge.
    ///
    /// `/etc/wireguard` is `drwx------ root`, and a `stat` from this user returns
    /// EACCES, not ENOENT. An early version read that as absence and convicted
    /// `/etc/wireguard/rezka-cy.key`, a file that is on the disk. A negative from
    /// a probe that cannot see the thing is not evidence of absence — it is the
    /// probe's limit, and the claim must survive it.
    Unknown,
}

impl Check {
    /// The thing being asserted, for display.
    pub fn subject(&self) -> &str {
        match self {
            Check::PathExists(p) | Check::CommandExists(p) => p,
        }
    }

    /// Ask the world. Microseconds: a `stat`, or a scan of `PATH`.
    pub fn test(&self, home: &Path, path_dirs: &[PathBuf]) -> Truth {
        match self {
            Check::PathExists(p) => match std::fs::symlink_metadata(expand(p, home)) {
                Ok(_) => Truth::Holds,
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Truth::Unknown,
                Err(_) => Truth::Refuted,
            },
            Check::CommandExists(c) => {
                if path_dirs.iter().any(|d| d.join(c).exists()) {
                    Truth::Holds
                } else {
                    Truth::Refuted
                }
            }
        }
    }
}

/// `~` is the only expansion: a memory row is written by a human or by the model,
/// never by a shell, so `$VARS` in one are prose rather than references.
///
/// The home passed in must be the USER's home. An early version used
/// `db::home()`, which is cml's own data directory — every `~/...` claim expanded
/// under `~/.claude/claude-memory-light/`, so `~/.claude/settings.json` was
/// reported as a falsehood while sitting on disk. That produced a headline number
/// of 58% rot against a true 27%: an uncalibrated predicate convicting the
/// innocent, which is the exact failure this module exists to prevent.
fn expand(p: &str, home: &Path) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(p),
    }
}

/// Pull every checkable claim out of a memory row.
///
/// Deliberately narrow. A claim is extracted only when the text's own syntax makes
/// the assertion unambiguous — an absolute path, or a command in backticks. Prose
/// like "the parser is slow" is not extractable and must stay unjudged: a system
/// that guesses at predicates would refute opinions, which is worse than silence.
pub fn extract(text: &str) -> Vec<Check> {
    let mut out = Vec::new();
    for tok in tokens(text) {
        if let Some(c) = as_path(&tok) {
            out.push(Check::PathExists(c));
        }
    }
    // Backticked commands are NOT extracted, though the machinery is here and
    // tested. Measured on the real corpus, the head-plus-argument heuristic
    // convicted `not`, `set`, `for`, `except` and `extern` — prose and code
    // fragments, not invocations — and flagged `tsc` and `vite`, which are real
    // but live under npx rather than on PATH. A predicate that accuses the
    // innocent is worse than no predicate: it is how a checker becomes noise.
    // Paths carry no such ambiguity, so v1 ships paths only.
    out.sort_by(|a, b| a.subject().cmp(b.subject()));
    out.dedup();
    out
}

/// Split on whitespace and the punctuation that ends a reference in prose.
fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || c == '`' || c == '"' || c == '\'' || c == '(')
        .map(|t| t.trim_end_matches(|c| ".,;:!?)]}".contains(c)).to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// An absolute path, or `None` for everything that merely looks like one.
///
/// `~75s` and `~1280` are "about 75 seconds" and "about 1280", and an early
/// version of this counted both as dead paths — which inflated the measured rot
/// from 27% to 52%. A leading `~` followed by a digit is prose, always.
fn as_path(tok: &str) -> Option<String> {
    let rooted = tok.starts_with("~/")
        || tok.starts_with("/home/")
        || tok.starts_with("/etc/")
        || tok.starts_with("/usr/")
        || tok.starts_with("/opt/")
        || tok.starts_with("/var/")
        || tok.starts_with("/srv/");
    if !rooted || tok.len() < 6 {
        return None;
    }
    // A path is written with the characters a filesystem accepts; anything else in
    // the token means it is prose that happens to contain a slash.
    if !tok
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._@+-~".contains(c))
    {
        return None;
    }
    // A glob or a placeholder names a shape, not a file: `~/.claude/projects/*/memory`
    // is true of the machine while no such directory exists.
    if tok.contains('*') || tok.contains('<') {
        return None;
    }
    Some(tok.to_string())
}

/// Commands written as `` `name …` `` — the backtick plus a following argument is
/// what distinguishes an invocation from a word someone merely quoted.
///
/// Kept and tested, not wired: see the note in `extract`. When a corpus arrives
/// where this predicate can be calibrated, the call site is one line.
#[cfg(test)]
fn backticked_commands(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('`') else { break };
        let inner = &after[..close];
        rest = &after[close + 1..];
        let mut parts = inner.split_whitespace();
        let (Some(head), Some(_arg)) = (parts.next(), parts.next()) else {
            continue;
        };
        if head.len() < 3 || head.contains('/') || head.contains('=') {
            continue;
        }
        if !head.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
            continue;
        }
        out.push(head.to_string());
    }
    out
}

/// Directories on `PATH`, resolved once per run rather than per claim.
pub fn path_dirs() -> Vec<PathBuf> {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// A row's verdict: which of its claims the world refuted.
pub struct Audit {
    pub checked: usize,
    pub refuted: Vec<String>,
}

impl Audit {
    /// True when the world contradicts something this row states.
    pub fn rotten(&self) -> bool {
        !self.refuted.is_empty()
    }
}

/// Test every claim in one row.
pub fn audit(text: &str, home: &Path, path_dirs: &[PathBuf]) -> Audit {
    let checks = extract(text);
    let mut refuted = Vec::new();
    let mut checked = 0;
    for c in &checks {
        match c.test(home, path_dirs) {
            Truth::Refuted => {
                checked += 1;
                refuted.push(c.subject().to_string());
            }
            Truth::Holds => checked += 1,
            // Not counted either way: an unseeable claim must not push a row
            // toward "everything it says is dead", which is what withholds it.
            Truth::Unknown => {}
        }
    }
    Audit { checked, refuted }
}

/// `cml truth` — audit stored memory against the world.
pub fn run(args: &[String]) -> crate::R<i32> {
    let quiet = args.iter().any(|a| a == "--quiet");
    let conn = crate::db::open_ro()?;
    let home = crate::paths::home_dir();
    let dirs = path_dirs();

    let mut stmt = conn.prepare(
        "SELECT text, ts, project FROM mem WHERE role IN ('memory','wiki')",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
    })?;

    let mut n_rows = 0_usize;
    let mut n_claims = 0_usize;
    let mut n_refuted = 0_usize;
    let mut rotten_rows = 0_usize;
    let mut worst: Vec<(usize, String, String, Vec<String>)> = Vec::new();
    let mut by_subject: HashMap<String, usize> = HashMap::new();

    for row in rows.flatten() {
        let (text, ts, project) = row;
        n_rows += 1;
        let a = audit(&text, &home, &dirs);
        n_claims += a.checked;
        n_refuted += a.refuted.len();
        if a.rotten() {
            rotten_rows += 1;
            for s in &a.refuted {
                *by_subject.entry(s.clone()).or_default() += 1;
            }
            let head: String = text.chars().take(60).collect();
            worst.push((a.refuted.len(), ts, project, a.refuted.clone()));
            let _ = head;
        }
    }

    println!(
        "{n_rows} rows · {n_claims} checkable claims · {n_refuted} refuted by the world"
    );
    if n_claims > 0 {
        println!(
            "{rotten_rows} of {n_rows} rows ({:.0}%) state something that is no longer true; \
             {:.0}% of claims are false",
            100.0 * rotten_rows as f64 / n_rows as f64,
            100.0 * n_refuted as f64 / n_claims as f64,
        );
    }
    if quiet || by_subject.is_empty() {
        return Ok(0);
    }
    let mut subjects: Vec<(String, usize)> = by_subject.into_iter().collect();
    subjects.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    println!("\nmost-repeated falsehoods:");
    for (s, n) in subjects.iter().take(20) {
        println!("  {n:3}x  {s}");
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs() -> Vec<PathBuf> {
        path_dirs()
    }

    /// The bug that doubled the measured rot: `~75s` is "about 75 seconds".
    #[test]
    fn prose_that_looks_like_a_path_is_not_a_claim() {
        for prose in ["~75s", "~1280", "~2850", "~Jul", "/e", "and/or"] {
            assert!(as_path(prose).is_none(), "{prose:?} must not be read as a path");
        }
    }

    #[test]
    fn a_real_path_is_a_claim_and_a_glob_is_not() {
        assert_eq!(as_path("~/.claude/CLAUDE.md"), Some("~/.claude/CLAUDE.md".into()));
        assert_eq!(as_path("/etc/caddy/Caddyfile"), Some("/etc/caddy/Caddyfile".into()));
        // A shape, not a file: true of the machine while nothing is at that literal path.
        assert!(as_path("~/.claude/projects/*/memory").is_none());
        assert!(as_path("/etc/<name>.conf").is_none());
    }

    #[test]
    fn a_dead_path_is_refuted_and_a_live_one_holds() {
        let home = crate::paths::home_dir();
        let dead = Check::PathExists("/opt/definitely-not-here/xyzzy".into());
        assert_eq!(dead.test(&home, &dirs()), Truth::Refuted);
        // `/usr` exists on every machine this runs on.
        let live = Check::PathExists("/usr/bin".into());
        assert_eq!(live.test(&home, &dirs()), Truth::Holds);
    }

    /// The rule this module would otherwise break: unseeable is not absent.
    #[test]
    fn an_unreadable_directory_yields_unknown_not_refuted() {
        let home = crate::paths::home_dir();
        // /etc/wireguard is 0700 root on this machine; /proc/1 is root-only too.
        // Whichever exists, the verdict must not be Refuted.
        for p in ["/etc/wireguard/any.key", "/proc/1/mem"] {
            let v = Check::PathExists(p.into()).test(&home, &dirs());
            assert_ne!(v, Truth::Refuted, "{p} is unreadable, not absent");
        }
    }

    #[test]
    fn a_command_claim_is_tested_against_path() {
        let home = crate::paths::home_dir();
        assert_eq!(Check::CommandExists("ls".into()).test(&home, &dirs()), Truth::Holds);
        assert_eq!(
            Check::CommandExists("zzz-not-a-real-binary".into()).test(&home, &dirs()),
            Truth::Refuted
        );
    }

    /// Words in backticks are not invocations; requiring an argument is what tells
    /// `cml search x` apart from a quoted noun like `policy`.
    #[test]
    fn only_a_backticked_invocation_counts_as_a_command() {
        assert_eq!(backticked_commands("use `cml search foo` here"), vec!["cml"]);
        assert!(backticked_commands("the `policy` field").is_empty());
        assert!(backticked_commands("`co_await` is C++").is_empty());
        assert!(backticked_commands("`/usr/bin/x y`").is_empty());
    }

    /// The whole point: a row that names a dead file must come back rotten, and a
    /// row of pure opinion must come back unjudged rather than refuted.
    #[test]
    fn a_row_is_judged_only_on_what_it_actually_asserts() {
        let home = crate::paths::home_dir();
        let rotten = audit("config lives at /opt/definitely-not-here/xyzzy now", &home, &dirs());
        assert!(rotten.rotten());
        assert_eq!(rotten.checked, 1);

        let opinion = audit("the parser feels slow and the API is ugly", &home, &dirs());
        assert_eq!(opinion.checked, 0, "prose carries no checkable claim");
        assert!(!opinion.rotten(), "an opinion must never be refuted");
    }
}
