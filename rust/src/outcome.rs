//! What happened last time you ran this.
//!
//! Every other lane answers "what was said" or "what was run". This one answers
//! "what did running it *cost*" — it pairs each `tool_use` with the `tool_result`
//! carrying its id, decides whether that result is a failure, and keeps a running
//! tally per normalized command signature.
//!
//! The tally is the whole product. Measured on this machine's 1,214 transcripts:
//! 42,118 Bash calls, a 4.8% base failure rate. Against that base, a naive "this
//! command failed before" rule fires on a third of all calls and is right 13% of
//! the time — a second always-on suggester, which is exactly what got deleted from
//! this machine for being noise. The measured curve:
//!
//! ```text
//!   bar   signatures   % of calls   precision   failures caught
//!    5%          114        33.0%         13%               73%
//!   10%           58         9.9%         21%               36%
//!   15%           38         3.9%         30%               20%
//!   20%           11         2.4%         30%               12%
//!   25%            4         0.1%         87%                1%
//! ```
//!
//! 15% and 20% are equally precise, so the quieter one wins: at 20% the gate speaks
//! on one call in forty and is right six times more often than chance. Silence is
//! the feature, not coverage — a gate that interrupts every sixth command gets
//! switched off within a day however accurate it is.

use rusqlite::Connection;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// Below this many observations a rate is noise, whatever it looks like.
const MIN_TRIES: u32 = 4;
/// Wilson lower bound a signature must clear to be worth a line of context.
/// Measured: 30% precision, firing on 2.4% of calls — the quietest bar at which
/// precision stops improving.
const WARN: f64 = 0.20;
/// Above this the tally states rather than hints: measured 87% precision, on one
/// call in a thousand.
const HARD: f64 = 0.25;
/// Longest signature kept. Two head tokens never reach this; the cap is a guard
/// against a pathological single token, not a design parameter.
const SIG_MAX: usize = 60;
/// How much of a result is scanned for failure markers. Errors land at the top or
/// the bottom; the middle of a 200 kB build log decides nothing.
const SCAN: usize = 4000;

/// Prefixes that describe *where* a command runs, not *what* it does. Skipped when
/// a later segment carries the real verb, so `cd /repo && cargo test` signs as
/// `cargo test` rather than as `cd PATH` — which was the defect that made the first
/// measurement useless: `cd PATH` scored 212 failures against 4,536 successes, i.e.
/// exactly the base rate, and would have fired on a fifth of everything.
const CONTEXT_ONLY: [&str; 6] = ["cd", "sudo", "env", "time", "nice", "then"];

/// Markers that mean the command did not do what it was asked.
///
/// Deliberately not `is_error` alone: that flag is set by the harness for refusals
/// and tool-level faults, and on a real transcript it covers 1 result in 130. A
/// `cargo build` that ends in `error: aborting due to 6 previous errors` carries
/// `is_error: false` and is the failure that actually costs time.
const MARKERS: [&str; 12] = [
    "error:",
    "error[",
    "FAILED",
    "Traceback (most recent",
    "fatal:",
    "Permission denied",
    "command not found",
    "No such file or directory",
    "panicked at",
    "SyntaxError",
    "error: aborting",
    "cannot find",
];

/// A signature's record, and what the gate says about it.
pub struct Verdict {
    pub sig: String,
    pub tries: u32,
    pub fails: u32,
    /// Wilson 95% lower bound on the failure rate.
    pub low: f64,
    pub sample: String,
}

impl Verdict {
    /// True when the tally is strong enough to state rather than hint.
    pub fn hard(&self) -> bool {
        self.low > HARD
    }

    /// One line, because a gate that needs a paragraph will be turned off.
    pub fn line(&self) -> String {
        let pct = 100.0 * f64::from(self.fails) / f64::from(self.tries);
        let head = if self.hard() { "failed" } else { "often failed" };
        let mut s = format!(
            "[cml] `{}` {} here before: {}/{} ({:.0}%)",
            self.sig, head, self.fails, self.tries, pct
        );
        if !self.sample.is_empty() {
            s.push_str(" — last: ");
            s.push_str(&self.sample);
        }
        s
    }
}

/// Normalize a command into the thing worth counting.
///
/// Paths, hashes and numbers are erased so that the same command in two repos, or
/// against two ports, is one signature. What survives is the verb and its target.
pub fn signature(cmd: &str) -> Option<String> {
    let mut fallback = None;
    for segment in split_segments(cmd) {
        let raw: Vec<&str> = segment
            .split_whitespace()
            .filter(|t| !t.starts_with('-') && !t.contains('=') && !is_redirect(t))
            .collect();
        // The program's NAME survives, its path does not: `/usr/bin/python3 x.py`
        // and `python3 x.py` are the same act. Erasing the head into `PATH` was
        // measured at 10.2% firing and 21% precision — it merged every
        // absolute-path invocation on the machine into one signature.
        let mut tokens: Vec<String> = Vec::with_capacity(raw.len());
        for (i, t) in raw.iter().enumerate() {
            tokens.push(if i == 0 { basename(t) } else { canon(t) });
        }
        tokens.retain(|t| !t.is_empty());
        let Some(head) = tokens.first() else { continue };
        if CONTEXT_ONLY.contains(&head.as_str()) {
            // Remember it in case the whole command is only context, then keep
            // looking for the segment that does the work.
            fallback.get_or_insert_with(|| head.clone());
            continue;
        }
        let mut sig = tokens.iter().take(2).cloned().collect::<Vec<_>>().join(" ");
        sig.truncate(floor_boundary(&sig, SIG_MAX));
        return Some(sig);
    }
    fallback
}

/// The head token reduced to the program being run: last path component, quotes
/// and a trailing `:` stripped. `./build.sh` and `/opt/x/build.sh` are one program.
fn basename(tok: &str) -> String {
    let t = tok.trim_matches(|c| c == '"' || c == '\'' || c == '`');
    let name = t.rsplit('/').next().unwrap_or(t);
    if name.is_empty() || name.chars().all(|c| c.is_ascii_digit()) {
        return canon(t);
    }
    let mut s = name.to_string();
    s.truncate(floor_boundary(&s, SIG_MAX));
    s
}

/// Shell plumbing that says nothing about what ran.
fn is_redirect(tok: &str) -> bool {
    tok.starts_with('>')
        || tok.starts_with('<')
        || tok.starts_with('&')
        || tok.contains(">&")
        || tok == "2>&1"
}

/// One token, with everything volatile replaced by its kind.
fn canon(tok: &str) -> String {
    let t = tok.trim_matches(|c| c == '"' || c == '\'' || c == '`');
    if t.is_empty() {
        return String::new();
    }
    if t.contains('/') || t.starts_with('~') {
        return "PATH".into();
    }
    if t.len() >= 7 && t.chars().all(|c| c.is_ascii_hexdigit()) {
        return "HASH".into();
    }
    if t.chars().all(|c| c.is_ascii_digit()) {
        return "N".into();
    }
    t.to_string()
}

/// Split on the separators that start a new command, keeping order.
fn split_segments(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let bytes: Vec<char> = cmd.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        let two = if i + 1 < bytes.len() {
            [bytes[i], bytes[i + 1]]
        } else {
            [bytes[i], ' ']
        };
        if two == ['&', '&'] || two == ['|', '|'] {
            out.push(std::mem::take(&mut cur));
            i += 2;
            continue;
        }
        if bytes[i] == ';' || bytes[i] == '|' {
            out.push(std::mem::take(&mut cur));
            i += 1;
            continue;
        }
        cur.push(bytes[i]);
        i += 1;
    }
    out.push(cur);
    out.into_iter().filter(|s| !s.trim().is_empty()).collect()
}

/// Largest index <= max that is a char boundary.
fn floor_boundary(s: &str, max: usize) -> usize {
    let mut end = max.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Does this result read as a failure?
pub fn looks_failed(is_error: bool, text: &str) -> bool {
    if is_error {
        return true;
    }
    let head = &text[..floor_boundary(text, SCAN)];
    MARKERS.iter().any(|m| head.contains(m))
}

/// Wilson score lower bound at 95%, which is what keeps 1-out-of-1 from ranking
/// above 24-out-of-28. A raw rate would put every single-observation fluke on top.
pub fn wilson_low(k: u32, n: u32) -> f64 {
    if n == 0 {
        return 0.0;
    }
    let (k, n) = (f64::from(k), f64::from(n));
    let z = 1.96_f64;
    let p = k / n;
    let denom = 1.0 + z * z / n;
    let centre = p + z * z / (2.0 * n);
    let margin = z * (p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt();
    ((centre - margin) / denom).max(0.0)
}

/// Tally every Bash call in one transcript file into `acc`.
fn scan_file(path: &Path, acc: &mut HashMap<String, (u32, u32, String)>) {
    let Ok(body) = std::fs::read_to_string(path) else {
        return;
    };
    // tool_use id -> signature, awaiting the result that carries the same id.
    let mut pending: HashMap<String, String> = HashMap::new();
    for line in body.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) else {
            continue;
        };
        for b in blocks {
            match b.get("type").and_then(Value::as_str) {
                Some("tool_use") if b.get("name").and_then(Value::as_str) == Some("Bash") => {
                    let (Some(id), Some(cmd)) = (
                        b.get("id").and_then(Value::as_str),
                        b.pointer("/input/command").and_then(Value::as_str),
                    ) else {
                        continue;
                    };
                    if let Some(sig) = signature(cmd) {
                        pending.insert(id.to_string(), sig);
                    }
                }
                Some("tool_result") => {
                    let Some(id) = b.get("tool_use_id").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(sig) = pending.remove(id) else { continue };
                    let text = result_text(b);
                    let failed = looks_failed(
                        b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                        &text,
                    );
                    let e = acc.entry(sig).or_insert((0, 0, String::new()));
                    e.0 += 1;
                    if failed {
                        e.1 += 1;
                        // Keep the first failure that actually said something. A
                        // harness-level refusal carries no marker line, and letting
                        // it overwrite leaves the most-failing signatures with an
                        // empty sample — which is most of the line's value gone.
                        if e.2.is_empty() {
                            e.2 = first_marker_line(&text);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// A result's text, whether it arrived as a string or as a block array.
fn result_text(b: &Value) -> String {
    match b.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(inner)) => inner
            .iter()
            .filter_map(|ib| ib.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// The first line that carries a marker — the part of a 200-line build log that
/// says what went wrong.
fn first_marker_line(text: &str) -> String {
    let head = &text[..floor_boundary(text, SCAN)];
    let line = head
        .lines()
        .find(|l| MARKERS.iter().any(|m| l.contains(m)))
        .unwrap_or("")
        .trim();
    let mut s = line.to_string();
    s.truncate(floor_boundary(&s, 120));
    s
}

/// Rebuild the tally from every transcript. Returns (signatures, calls seen).
pub fn rebuild(conn: &mut Connection) -> crate::R<(usize, u32)> {
    let root = crate::db::transcripts_dir();
    let mut files = Vec::new();
    collect_jsonl(&root, &mut files);

    let mut acc: HashMap<String, (u32, u32, String)> = HashMap::new();
    for f in &files {
        scan_file(f, &mut acc);
    }
    let calls = acc.values().map(|(t, _, _)| *t).sum();

    let tx = conn.transaction()?;
    tx.execute("DELETE FROM outcome", [])?;
    {
        let mut ins = tx.prepare(
            "INSERT INTO outcome(sig, tries, fails, sample) VALUES(?1,?2,?3,?4)",
        )?;
        for (sig, (tries, fails, sample)) in &acc {
            ins.execute(rusqlite::params![sig, tries, fails, sample])?;
        }
    }
    tx.commit()?;
    Ok((acc.len(), calls))
}

/// Every `.jsonl` under `root`, recursively.
fn collect_jsonl(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_jsonl(&p, out);
        } else if p.extension().is_some_and(|x| x == "jsonl") {
            out.push(p);
        }
    }
}

/// What the tally says about a command, or nothing — which is the common case and
/// the point.
pub fn verdict(conn: &Connection, cmd: &str) -> Option<Verdict> {
    let sig = signature(cmd)?;
    let row = conn
        .query_row(
            "SELECT tries, fails, sample FROM outcome WHERE sig = ?1",
            [&sig],
            |r| Ok((r.get::<_, u32>(0)?, r.get::<_, u32>(1)?, r.get::<_, String>(2)?)),
        )
        .ok()?;
    let (tries, fails, sample) = row;
    if tries < MIN_TRIES || fails == 0 {
        return None;
    }
    let low = wilson_low(fails, tries);
    if low <= WARN {
        return None;
    }
    Some(Verdict { sig, tries, fails, low, sample })
}

/// `cml outcomes [--check <command>]` — rebuild the tally, or ask about one command.
pub fn run(args: &[String]) -> crate::R<i32> {
    if let Some(i) = args.iter().position(|a| a == "--check") {
        let cmd = args.get(i + 1).map(String::as_str).unwrap_or_default();
        let conn = crate::db::open_ro()?;
        return match verdict(&conn, cmd) {
            Some(v) => {
                println!("{}", v.line());
                Ok(0)
            }
            None => {
                println!("[cml] nothing on record against that");
                Ok(0)
            }
        };
    }

    let mut conn = crate::db::open()?;
    crate::db::ensure_schema(&conn)?;
    let (sigs, calls) = rebuild(&mut conn)?;

    // What the gate would actually do, printed as the two numbers that decide
    // whether it is worth keeping: how often it speaks, and how often it is right.
    let mut fires = 0_u32;
    let mut hits = 0_u32;
    let mut rows: Vec<(f64, u32, u32, String)> = Vec::new();
    let mut stmt = conn.prepare("SELECT sig, tries, fails FROM outcome")?;
    let iter = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?, r.get::<_, u32>(2)?))
    })?;
    for row in iter.flatten() {
        let (sig, tries, fails) = row;
        if tries < MIN_TRIES || fails == 0 {
            continue;
        }
        let low = wilson_low(fails, tries);
        if low > WARN {
            fires += tries;
            hits += fails;
            rows.push((low, fails, tries, sig));
        }
    }
    rows.sort_by(|a, b| b.0.total_cmp(&a.0));

    println!("{sigs} signatures from {calls} calls");

    // The threshold is a judgement about how often it is acceptable to interrupt,
    // so print what each one costs rather than defending the compiled-in default.
    // Precision alone does not decide it: a gate that speaks on one call in six
    // gets disabled whatever its precision, which is how the last always-on
    // suggester on this machine died.
    let all: Vec<(u32, u32)> = {
        let mut s = conn.prepare("SELECT tries, fails FROM outcome")?;
        let it = s.query_map([], |r| Ok((r.get::<_, u32>(0)?, r.get::<_, u32>(1)?)))?;
        it.flatten().filter(|(t, f)| *t >= MIN_TRIES && *f > 0).collect()
    };
    let total_fails: u32 = all.iter().map(|(_, f)| f).sum();
    println!("\n  bar   signatures   fires   % of calls   precision   failures caught");
    for bar in [0.05, 0.10, 0.15, 0.20, 0.25, 0.30, 0.40] {
        let sel: Vec<&(u32, u32)> =
            all.iter().filter(|(t, f)| wilson_low(*f, *t) > bar).collect();
        let fire: u32 = sel.iter().map(|(t, _)| t).sum();
        let hit: u32 = sel.iter().map(|(_, f)| f).sum();
        if fire == 0 {
            continue;
        }
        println!(
            "{:5.0}%  {:10}  {:6}  {:10.1}%  {:9.0}%  {:14.0}%",
            bar * 100.0,
            sel.len(),
            fire,
            100.0 * f64::from(fire) / f64::from(calls.max(1)),
            100.0 * f64::from(hit) / f64::from(fire),
            100.0 * f64::from(hit) / f64::from(total_fails.max(1)),
        );
    }
    println!();

    if fires == 0 {
        println!("nothing clears the bar — the gate stays silent everywhere");
        return Ok(0);
    }
    println!(
        "gate fires on {fires} of {calls} calls ({:.1}%), right {:.0}% of the time",
        100.0 * f64::from(fires) / f64::from(calls.max(1)),
        100.0 * f64::from(hits) / f64::from(fires),
    );
    for (low, fails, tries, sig) in rows.iter().take(20) {
        println!("  {:5.1}%  {fails:4}/{tries:<5} {sig}", 100.0 * low);
    }
    Ok(0)
}

/// `cml gate` — the PreToolUse hook. Silent unless the tally says otherwise.
///
/// Every failure path here ends at `passthrough`: a gate that blocks a command
/// because its own database was missing would be worse than no gate at all.
pub fn gate(_args: &[String]) -> crate::R<i32> {
    let Some(payload) = crate::recall::hook::read() else {
        return crate::recall::hook::passthrough();
    };
    if crate::recall::hook::field(&payload, "tool_name") != "Bash" {
        return crate::recall::hook::passthrough();
    }
    let cmd = payload
        .pointer("/tool_input/command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Ok(conn) = crate::db::open_ro() else {
        return crate::recall::hook::passthrough();
    };
    match verdict(&conn, cmd) {
        Some(v) => crate::recall::hook::inject("PreToolUse", &v.line()),
        None => crate::recall::hook::passthrough(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defect that made the first measurement worthless: signing a command by
    /// its `cd` prefix put 212 failures and 4,536 successes under one signature,
    /// which is the base rate wearing a warning label.
    #[test]
    fn a_cd_prefix_does_not_become_the_signature() {
        assert_eq!(signature("cd /home/u/dev/repo && cargo test").as_deref(), Some("cargo test"));
        // A script named without a directory is the command's target, and worth
        // keeping apart: `python3 run.py` and `python3 probe.py` fail differently.
        assert_eq!(signature("cd ~/dev && python3 run.py").as_deref(), Some("python3 run.py"));
        assert_eq!(signature("cd ~/dev && python3 tools/run.py").as_deref(), Some("python3 PATH"));
        // A bare `cd` still signs as something rather than vanishing.
        assert_eq!(signature("cd /tmp").as_deref(), Some("cd"));
    }

    #[test]
    fn volatile_tokens_collapse_so_two_repos_are_one_signature() {
        assert_eq!(
            signature("cargo build --release 2>&1"),
            signature("cargo build --release"),
            "flags and redirections must not split a signature"
        );
        // A hash inside the two-token head collapses; past it, it never enters the
        // signature at all — `git show <any sha>` is one signature either way.
        assert_eq!(signature("checkout 4f2a91cabc123").as_deref(), Some("checkout HASH"));
        assert_eq!(signature("git show 4f2a91c"), signature("git show 9b7e0d2"));
        assert_eq!(signature("sleep 30").as_deref(), Some("sleep N"));
        assert_eq!(signature("sleep 5").as_deref(), Some("sleep N"));
    }

    #[test]
    fn empty_input_has_no_signature() {
        assert_eq!(signature(""), None);
        assert_eq!(signature("   "), None);
        assert_eq!(signature("&& ;"), None);
    }

    /// `is_error` covers 1 result in 130 on a real transcript; the failures that
    /// cost time are the ones that report success at the tool layer.
    #[test]
    fn a_failing_build_is_detected_without_the_error_flag() {
        assert!(looks_failed(false, "   Compiling cml\nerror: aborting due to 6 previous errors"));
        assert!(looks_failed(false, "Traceback (most recent call last):\n  File ..."));
        assert!(looks_failed(true, "anything at all"));
        assert!(!looks_failed(false, "Finished release [optimized] target(s) in 3.2s"));
        assert!(!looks_failed(false, ""));
    }

    /// A single observation must never outrank a well-attested one — this is the
    /// whole reason for the Wilson bound over a raw rate.
    #[test]
    fn one_out_of_one_ranks_below_twenty_four_out_of_twenty_eight() {
        let fluke = wilson_low(1, 1);
        let attested = wilson_low(24, 28);
        assert!(attested > fluke, "24/28 ({attested:.3}) must outrank 1/1 ({fluke:.3})");
        // And the measured base rate stays below the gate's threshold.
        assert!(wilson_low(2016, 42103) < WARN);
    }

    #[test]
    fn a_long_command_signature_stays_a_valid_string() {
        let long = format!("weirdtool{}", "x".repeat(200));
        let sig = signature(&long).expect("a single long token still signs");
        assert!(sig.len() <= SIG_MAX);
    }
}
