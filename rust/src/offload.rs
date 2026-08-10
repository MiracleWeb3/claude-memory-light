//! PostToolUse: condense a large tool result before it reaches the context window.
//!
//! Measured over 146 transcripts on this machine: 19,662 tool results, 18.3 MB,
//! median 239 bytes — but the 12.4% at or above 2 KB carry 63.1% of all bytes. One
//! call in eight is worth touching; the other seven are already small.
//!
//! Nothing here calls an LLM. A hook that blocks on a network round-trip stalls
//! every tool call in the session, so the rules are fixed: keep the errors, keep
//! the edges, count the rest.
//!
//! What the C++ predecessor needed and Rust does not: the hex-escape dance around
//! `\xE2\x9F\xA8` (a hex escape eats every hex digit that follows it, so
//! `"\xE2\x9F\xA8cml"` was `\xA8c` and `-Werror`), and the buffered-`ofstream`
//! flush check — `fs::write` is a `write(2)`, so ENOSPC is an `Err` here rather
//! than a silent 0-byte file discovered in a destructor.

use std::io::Read;
use std::path::PathBuf;

/// Below this, condensing costs more header than it saves body.
const MIN_BYTES: usize = 2048;
const HEAD: usize = 5;
const TAIL: usize = 15;

/// How far a flagged line may drag context upward. Unbounded is not an option: a
/// pretty-printed JSON body would never terminate the run and the whole file stays.
///
/// 1, picked by the corpus — 1,046 real Bash results:
///
/// ```text
///   depth   0     1     2     3     4     6     8    12
///   bytes  49.4  48.2  47.2  46.4  45.6  44.4  43.4  41.9   % saved
///   surv   48.3  53.3  55.3  57.1  58.0  59.4  60.4  62.6   % leave-one-needle-out
/// ```
///
/// Both curves are monotone and opposed, so depth is a rate, not a threshold — and
/// 0->1 is the knee: 1.2 points of bytes buys 5.0 points of survival, where every
/// later step buys 1-2.
const DRAG_UP: usize = 1;

/// Substring, case-folded. Measured against real output, not guessed: the first
/// version of this list was "a line containing the word error", and real tools
/// mostly do not print that word. It dropped all eleven of `command not found`,
/// `Permission denied`, `No such file or directory`, `cannot find -lfoo`, `Unable
/// to locate package`, `Couldn't connect`, `Killed`, and every Python frame.
///
/// Warnings are flagged, not merely counted: a `-Werror` tree makes a warning an
/// error, and the old split also produced an accident — `-Werror=unused-variable`
/// matched the *error* needle while `-Wunused-variable` did not.
///
/// Public because `Opts::skip_needle` is an index into THIS slice; a second copy in
/// a calibration harness would drift and then measure a condenser that is not the
/// one shipping.
pub const FLAG_NEEDLES: [&str; 31] = [
    "error", "fail", "panic", "traceback", "undefined reference", "fatal",
    "segmentation fault", "assertion", "exception", "warning", "deprecated",
    "command not found", "no such file", "permission denied", "cannot access",
    "cannot stat", "cannot find", "cannot open", "unable to", "not found",
    "couldn't", "could not", "refused", "timed out", "killed", "aborted",
    "denied", "invalid", "unexpected", "missing", "expected",
];

pub struct Condensed {
    pub text: String,
    pub elided: usize,
    /// "flagged", not "errors": the rule is a substring match, and a successful
    /// `npm ls` listing error-ex@1.3.2 is not three errors. The header states this
    /// number to a model that will reason over it, so it must claim only what it
    /// knows — lines worth your eyes.
    pub flagged: usize,
    pub grew: bool,
}

/// Calibration only. Production calls [`condense`], so a number measured through
/// this seam is a number about production. Leave-one-needle-out needs a needle
/// switched off; the drag-rule baseline needs both passes off.
#[derive(Clone, Copy)]
pub struct Opts {
    pub skip_needle: Option<usize>,
    pub drags: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Self { skip_needle: None, drags: true }
    }
}

/// Allowlist, deliberately: an unknown tool is left alone, so a tool added by a
/// future Claude Code version is never silently condensed.
///
/// Bash only. Measured over 146 transcripts: of the 11.60 MB carried by results at
/// or above 2 KB, Read is 53.7% and Bash 35.2%. Read is deliberately never touched
/// — at PostToolUse you have just read that file because you need it. Grep/Glob/MCP
/// wait until their result shapes are recorded, because `updatedToolOutput` is
/// validated against the tool's own schema and rejected SILENTLY on a mismatch, so
/// guessing produces a feature that does nothing and reports success.
pub fn condensable(tool_name: &str, bytes: usize) -> bool {
    bytes >= MIN_BYTES && tool_name == "Bash"
}

/// Pure. `spill_path` is named in the replacement so the original stays reachable;
/// pass `""` to omit the pointer.
pub fn condense(out: &str, spill_path: &str) -> Condensed {
    condense_with(out, spill_path, Opts::default())
}

pub fn condense_with(out: &str, spill_path: &str, o: Opts) -> Condensed {
    let lines = split_lines(out);
    let n = lines.len();
    let mut keep = vec![false; n];
    let mut seed = vec![false; n];
    let mut flagged = 0;

    for i in 0..n {
        if i < HEAD || i + TAIL >= n {
            keep[i] = true;
        }
        if has_any(&lines[i].to_ascii_lowercase(), o.skip_needle) {
            keep[i] = true;
            seed[i] = true; // ONLY a needle match seeds a drag — never a head/tail edge
            flagged += 1;
        }
    }

    if o.drags {
        // Downward: a flagged line drags the contiguous indented run beneath it.
        // Measured on a real pytest traceback this recovers 8 frames of 8 — no
        // `File "..."` line matches any needle, so this rule does all the work.
        //
        // Seeded from `seed`, not from `keep`: seeding from any kept line lets an
        // indented body hanging off the head window swallow the file (a 483-line
        // pretty JSON went from 96% saved to 0%). And there is no `!keep[i+1]`
        // guard — it reads as redundancy but doubles as a propagation gate, so a
        // needle at index 0-3 could not reach past the head edge at 4, which is
        // exactly where a combined stdout+stderr capture puts its failure.
        for i in 0..n.saturating_sub(1) {
            if seed[i] && indented(lines[i + 1]) {
                keep[i + 1] = true;
                seed[i + 1] = true;
            }
        }
        // Upward, bounded: GCC prints its instantiation chain ABOVE the error and
        // at column 0, so neither the needles nor the downward rule reach it.
        // `tpl2.cpp:6:14: required from here` is the line naming the file the user
        // owns; without it every surviving `error:` points into /usr/include.
        for i in 0..n {
            if !seed[i] {
                continue;
            }
            for k in 1..=DRAG_UP.min(i) {
                if lines[i - k].is_empty() {
                    break; // a blank line ends the block
                }
                keep[i - k] = true;
            }
        }
    }

    let mut text = String::new();
    let mut gap = 0;
    let mut elided = 0;
    for i in 0..n {
        if keep[i] {
            flush_gap(&mut text, &mut gap); // or the text claims false adjacency
            text.push_str(lines[i]);
            text.push('\n');
        } else {
            elided += 1;
            gap += 1;
        }
    }
    flush_gap(&mut text, &mut gap);

    let mut header = format!("⟨cml: {} elided · {flagged} flagged", plural(elided, "line"));
    if !spill_path.is_empty() {
        header.push_str(&format!(" · full: {spill_path}"));
    }
    header.push_str("⟩\n");

    let mut c = Condensed { text: header + &text, elided, flagged, grew: false };
    // Never hand back more than we were given. A single-line 9 KB JSON, or any
    // output of 20 lines or fewer, keeps every line by construction (HEAD + TAIL)
    // and would otherwise be re-emitted under a header claiming nothing was elided
    // — costing tokens, not saving them. Measured: a 9,692-byte curl result came
    // back 9,770 bytes.
    if c.text.len() >= out.len() {
        c.text = out.to_string();
        c.elided = 0;
        c.flagged = 0; // the pass that produced it was thrown away; do not report its count
        c.grew = true;
    }
    c
}

/// Writes `body` verbatim under the session's spill directory; returns the path, or
/// `None` on any failure.
///
/// The condensed replacement is what persists to the transcript — the original is
/// gone from it entirely — so this file is the only surviving copy, and a failed
/// spill must mean no condensation, never a lost turn.
pub fn spill_write(session: &str, tool_use_id: &str, body: &str) -> Option<PathBuf> {
    // Both components are pasted into a filesystem path and both arrive as JSON on
    // stdin, so anything that is not a plain name is refused rather than resolved.
    // `tool_use_id` used to fall back to a constant "result": traversal-safe, and a
    // collision instead — two unnamed spills in one session write the same file and
    // the second erases the first, whose transcript entry still names that path.
    if !plain_name(session) || !plain_name(tool_use_id) {
        return None;
    }
    let dir = crate::db::home().join("spill").join(session);
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{tool_use_id}.txt"));
    match std::fs::write(&path, body) {
        Ok(()) => Some(path),
        // No empty file left behind looking authoritative.
        Err(_) => {
            let _ = std::fs::remove_file(&path);
            None
        }
    }
}

/// The hook entry: PostToolUse JSON on stdin, hook JSON on stdout.
pub fn run(_args: &[String]) -> crate::R<i32> {
    let mut payload = String::new();
    // A stdin that cannot be read is an empty payload, which is a passthrough.
    let _ = std::io::stdin().read_to_string(&mut payload);
    println!("{}", hook_response(&payload));
    Ok(0)
}

/// Whatever happens, the tool result must go through untouched.
const PASSTHROUGH: &str = r#"{"continue": true}"#;

/// Split from [`run`] because the emitted bytes are the contract, and the failure
/// they guard is invisible: `updatedToolOutput` is validated against the tool's own
/// output schema and REJECTED SILENTLY on a mismatch. Claude Code carries the
/// message "PostToolUse hook returned updatedToolOutput that does not match
/// `${toolName}`'s output shape; using original output", but it never reaches
/// stdout, stderr, or --debug. A replacement missing one of Bash's five keys is
/// indistinguishable from a hook that did nothing — hence the object, not the bare
/// string that reads so much more naturally.
fn hook_response(payload: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) else {
        return PASSTHROUGH.into();
    };
    // Field names taken from a recorded payload, not the documentation:
    // `tool_response` is an object, never a string.
    let (Some(tool), Some(stdout)) =
        (v["tool_name"].as_str(), v["tool_response"]["stdout"].as_str())
    else {
        return PASSTHROUGH.into();
    };
    if !condensable(tool, stdout.len()) {
        return PASSTHROUGH.into();
    }

    // The spill goes first, and a failed spill means no replacement at all: the
    // transcript keeps only what we emit, so condensing without a saved original
    // destroys the output permanently.
    let session = v["session_id"].as_str().unwrap_or_default();
    let tuid = v["tool_use_id"].as_str().unwrap_or_default();
    let Some(spill) = spill_write(session, tuid, stdout) else {
        return PASSTHROUGH.into();
    };
    let c = condense(stdout, &spill.to_string_lossy());
    if c.grew {
        return PASSTHROUGH.into();
    }

    // Every sibling field is echoed back unchanged; only stdout moves.
    let resp = &v["tool_response"];
    let flag = |k: &str| resp[k].as_bool().unwrap_or(false);
    serde_json::json!({
        "continue": true,
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "updatedToolOutput": {
                "stdout": c.text,
                "stderr": resp["stderr"].as_str().unwrap_or_default(),
                "interrupted": flag("interrupted"),
                "isImage": flag("isImage"),
                "noOutputExpected": flag("noOutputExpected"),
            }
        }
    })
    .to_string()
}

fn flush_gap(text: &mut String, gap: &mut usize) {
    if *gap == 0 {
        return;
    }
    text.push_str(&format!("⟨… {} …⟩\n", plural(*gap, "line")));
    *gap = 0;
}

/// "1 line", not "1 lines". The header and the gap markers are read by a model that
/// reasons over them; sloppy grammar is one more thing it has to decide to trust.
fn plural(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

fn has_any(folded: &str, skip: Option<usize>) -> bool {
    FLAG_NEEDLES
        .iter()
        .enumerate()
        .any(|(i, needle)| Some(i) != skip && folded.contains(needle))
}

/// A trailing newline ends the last line rather than starting an empty one.
fn split_lines(s: &str) -> Vec<&str> {
    let mut v: Vec<&str> = s.split('\n').collect();
    if v.last().is_some_and(|l| l.is_empty()) {
        v.pop();
    }
    v
}

/// An indented line is a continuation of the unindented line above it — that is how
/// tracebacks, GCC carets, gtest expectations and pytest assertions all print.
/// Keeping a flagged line without its indented run keeps the fact that something
/// failed and throws away where.
fn indented(s: &str) -> bool {
    s.starts_with(' ') || s.starts_with('\t')
}

fn plain_name(s: &str) -> bool {
    !s.is_empty() && !s.contains('/') && !s.contains("..")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big_log(lines: usize, body: &str) -> String {
        (0..lines).map(|i| format!("{body} {i}\n")).collect()
    }

    /// The marker is what the user reads in every oversized Bash result. Its exact
    /// bytes are a contract, not a detail — a reformat is a visible regression.
    #[test]
    fn marker_format_is_exact() {
        let c = condense(&big_log(200, "compiling"), "/tmp/spill/s/t.txt");
        let first = c.text.lines().next().unwrap();
        assert_eq!(
            first,
            "⟨cml: 180 lines elided · 0 flagged · full: /tmp/spill/s/t.txt⟩",
            "header format changed"
        );
        assert!(c.text.contains("⟨… 180 lines …⟩"), "gap marker format changed");

        // Singular, and the spill pointer omitted when there is none. HEAD + TAIL
        // is 20, so 21 lines elide exactly one — and they have to be long enough
        // that a header still fits under the original.
        let log: String = (0..21).map(|i| format!("line {i} {}\n", "y".repeat(190))).collect();
        let c = condense(&log, "");
        assert_eq!(c.text.lines().next().unwrap(), "⟨cml: 1 line elided · 0 flagged⟩");
        assert!(c.text.contains("⟨… 1 line …⟩"));

        // One flagged line, and the header says "flagged" — never "errors". A
        // substring match is not a verdict: `npm ls` listing error-ex@1.3.2 once
        // reported "3 errors" from a command that exited 0.
        let mut log = big_log(200, "dependency");
        log.push_str("npm error missing peer\n");
        let c = condense(&log, "");
        assert_eq!(c.text.lines().next().unwrap(), "⟨cml: 181 lines elided · 1 flagged⟩");
        assert!(!c.text.contains(" errors"));
    }

    #[test]
    fn floor_and_allowlist() {
        assert!(condensable("Bash", 4000));
        assert!(!condensable("Bash", MIN_BYTES - 1), "under 2 KB is left alone");
        assert!(condensable("Bash", MIN_BYTES), "the floor itself is condensable");
        for tool in ["Read", "Edit", "Write", "Grep", "mcp__foo__bar", "SomeFutureTool"] {
            assert!(!condensable(tool, 400_000), "{tool} must be left alone");
        }
    }

    /// HEAD + TAIL == 20, so 20 lines or fewer keep every line by construction and a
    /// header can only add bytes. Measured: a 9,692-byte curl result came back 9,770.
    #[test]
    fn elision_threshold_keeps_edges_and_detects_growth() {
        let log = big_log(200, "line");
        let c = condense(&log, "/tmp/s.txt");
        assert!(c.text.contains("line 0\n") && c.text.contains("line 4\n"), "5 head lines");
        assert!(c.text.contains("line 185\n") && c.text.contains("line 199\n"), "15 tail lines");
        assert!(!c.text.contains("line 100\n"), "the middle is gone");
        assert_eq!(c.elided, 180);
        assert!(!c.grew);

        // The head ends at 4 and the tail resumes at 185; without a marker between
        // them the reader sees two lines as adjacent that never were.
        let h = c.text.find("line 4\n").unwrap();
        let t = c.text.find("line 185\n").unwrap();
        let marker = h + c.text[h..].find('…').expect("a gap marker between the two edges");
        assert!(h < marker && marker < t);

        for lines in 0..=20 {
            let short = big_log(lines, "x");
            let c = condense(&short, "/tmp/s.txt");
            assert_eq!(c.text, short, "{lines} lines must come back untouched");
            assert!(c.elided == 0 && c.flagged == 0, "and carry no count from a discarded pass");
        }
        // A 9 KB single line is the same case seen from the byte side.
        let one_liner = "x".repeat(9000);
        assert!(condense(&one_liner, "/tmp/s.txt").grew);
    }

    #[test]
    fn what_survives_is_the_diagnosis_not_just_the_failure() {
        // A pytest traceback: no `File "..."` line matches a needle, so the
        // indented-run rule is doing all the work here.
        let mut log = big_log(200, "collecting");
        log.push_str("Traceback (most recent call last):\n");
        log.push_str("  File \"/x/boom.py\", line 3, in <module>\n");
        log.push_str("    raise KeyError('missing')\n");
        log.push_str(&big_log(200, "teardown"));
        let c = condense(&log, "/tmp/s.txt");
        assert!(c.text.contains("boom.py"), "WHERE it failed, not just THAT it failed");
        assert!(c.text.contains("raise KeyError"), "the whole run, not only its first line");

        // GCC's instantiation chain prints ABOVE the error at column 0.
        let mut log = big_log(60, "[..] Building CXX object");
        log.push_str("tpl2.cpp:6:14:   required from here\n");
        log.push_str("/usr/include/c++/13/predefined_ops.h:45: error: no match for 'operator<'\n");
        log.push_str(&big_log(60, "[..] Building CXX object"));
        let c = condense(&log, "/tmp/s.txt");
        assert!(c.text.contains("tpl2.cpp:6"), "the line naming the file the user owns");

        // A failure on line 0 keeps its body past the head edge at 4.
        let mut log = "error: no match for 'operator<'\n".to_string();
        for i in 0..40 {
            log.push_str(&format!("  candidate {i} rejected\n"));
        }
        log.push_str(&big_log(200, "compiling"));
        let c = condense(&log, "/tmp/s.txt");
        assert!(c.text.contains("candidate 39"), "the whole indented body, not four lines of it");
        assert!(!c.text.contains("compiling 100"), "and the drag stops at the run's end");

        // N1: an all-indented body must still condense, or pretty JSON keeps whole.
        let mut json = "{\n".to_string();
        for i in 0..400 {
            json.push_str(&format!("    \"key{i}\": {i},\n"));
        }
        json.push_str("}\n");
        let c = condense(&json, "/tmp/s.txt");
        assert!(!c.grew && c.text.len() < json.len() / 4);
    }

    /// The calibration seam must default to production and BITE when set, or the gap
    /// it measures is fiction.
    #[test]
    fn calibration_seam_defaults_to_production() {
        let mut log = big_log(200, "collecting");
        log.push_str("Traceback (most recent call last):\n");
        log.push_str("  File \"/x/boom.py\", line 3, in <module>\n");
        log.push_str(&big_log(200, "teardown"));

        let shipped = condense(&log, "/tmp/s.txt");
        assert_eq!(shipped.text, condense_with(&log, "/tmp/s.txt", Opts::default()).text);
        assert!(shipped.text.contains("boom.py"));
        assert!(
            !condense_with(&log, "/tmp/s.txt", Opts { drags: false, ..Opts::default() })
                .text
                .contains("boom.py"),
            "drags=false is the pre-rule baseline"
        );

        let tb = FLAG_NEEDLES.iter().position(|n| *n == "traceback").expect("needle by name");
        assert_eq!(shipped.flagged, 1);
        assert_eq!(
            condense_with(&log, "/tmp/s.txt", Opts { skip_needle: Some(tb), ..Opts::default() })
                .flagged,
            0,
            "skipping one needle removes exactly that needle's matches"
        );
    }

    /// The spill is the ONLY surviving copy, so byte-identity is the contract — a
    /// summary of the original is not a recovery path. One test owns `$CML_HOME`
    /// because the variable is process-global.
    #[test]
    fn spill_round_trips_and_the_hook_emits_bashs_five_keys() {
        let sandbox = std::env::temp_dir().join(format!("cml-offload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&sandbox);
        std::env::set_var("CML_HOME", &sandbox);

        let body = "the original, untouched\nwith two lines\n";
        let path = spill_write("sess-abc", "toolu_123", body).expect("a path");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), body, "byte-identical");

        for (s, t) in [("", "t"), ("../../etc", "t"), ("sess", "../../tmp/pwned"), ("sess", "")] {
            assert!(spill_write(s, t, "x").is_none(), "traversal or blank is refused: {s:?} {t:?}");
        }

        let stdout = big_log(400, "line");
        let payload = serde_json::json!({
            "tool_name": "Bash", "tool_use_id": "toolu_t1", "session_id": "sess-hook",
            "tool_response": {"stdout": stdout, "stderr": "boom", "interrupted": false,
                              "isImage": false, "noOutputExpected": false}
        })
        .to_string();
        let out = hook_response(&payload);
        let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        let upd = &v["hookSpecificOutput"]["updatedToolOutput"];
        // All five, or the whole replacement is dropped without a word.
        for k in ["stdout", "stderr", "interrupted", "isImage", "noOutputExpected"] {
            assert!(!upd[k].is_null(), "Bash's `{k}` is missing from updatedToolOutput");
        }
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        assert_eq!(upd["stderr"], "boom", "siblings echo back — only stdout moves");
        assert!(upd["stdout"].as_str().unwrap().starts_with("⟨cml: "));
        assert_eq!(
            std::fs::read_to_string(sandbox.join("spill/sess-hook/toolu_t1.txt")).unwrap(),
            stdout,
            "the original landed on disk before the swap"
        );

        // No session means no spill, which must mean passthrough — not a
        // condensation with nowhere to fall back to.
        let orphan = serde_json::json!({
            "tool_name": "Bash", "tool_use_id": "toolu_t2",
            "tool_response": {"stdout": stdout}
        })
        .to_string();
        assert_eq!(hook_response(&orphan), PASSTHROUGH);
        assert_eq!(hook_response("not json"), PASSTHROUGH);
        assert_eq!(hook_response(""), PASSTHROUGH);
        assert_eq!(
            hook_response(&serde_json::json!({"tool_name": "Bash",
                "tool_response": {"stdout": "just a line\n"}})
            .to_string()),
            PASSTHROUGH,
            "under the floor nothing is touched"
        );

        std::env::remove_var("CML_HOME");
        let _ = std::fs::remove_dir_all(&sandbox);
    }
}
