//! `cml capture` — the Stop hook: append a digest of the turn to the learning inbox.
//!
//! The ask is already in the index; what nobody remembers a week later is the *work*.
//! So this records the files touched, the commands run, whether anything failed, and
//! how the turn ended. Pure transcript parsing: no LLM call, no network, no state.
//!
//! Design law: a memory hook must never block or break the session. Every path here
//! returns cleanly, including "the transcript is not there" and "the payload is junk".

use std::io::BufRead;

use serde_json::Value;

use crate::recall::hook;
use crate::text::{is_noise, looks_like_correction, squeeze};

use crate::paths;

const MAX_FILES: usize = 6;
const MAX_CMDS: usize = 4;
const CMD_CHARS: usize = 48;
const OUTCOME_CHARS: usize = 220;
const ASK_CHARS: usize = 300;

/// What one turn did.
#[derive(Default)]
pub struct Digest {
    pub ask: String,
    pub files: Vec<String>,
    pub commands: Vec<String>,
    pub skills: Vec<String>,
    pub failures: usize,
    pub outcome: String,
}

impl Digest {
    /// Indented continuation lines. Empty for a pure-conversation turn, so the caller
    /// falls back to a plain one-line entry.
    pub fn detail_lines(&self) -> Vec<String> {
        let mut parts: Vec<String> = Vec::new();
        if !self.files.is_empty() {
            parts.push(format!("files: {}", self.files.join(", ")));
        }
        if !self.commands.is_empty() {
            parts.push(format!("ran: {}", self.commands.join(" | ")));
        }
        if !self.skills.is_empty() {
            parts.push(format!("skills: {}", self.skills.join(", ")));
        }
        if self.failures > 0 {
            parts.push(format!("{} failed", self.failures));
        }

        let mut out = Vec::new();
        if !parts.is_empty() {
            out.push(format!("    {}", parts.join("  \u{b7}  ")));
        }
        if !self.outcome.is_empty() {
            out.push(format!("    did: {}", self.outcome));
        }
        out
    }
}

/// The text of a message: a bare string, or the `text` blocks of a content array.
fn text_of(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    let Some(blocks) = content.as_array() else {
        return String::new();
    };
    blocks
        .iter()
        .filter(|b| hook::field(b, "type") == "text")
        .map(|b| hook::field(b, "text"))
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn push_unique(v: &mut Vec<String>, item: String, cap: usize) {
    if item.is_empty() || v.len() >= cap || v.contains(&item) {
        return;
    }
    v.push(item);
}

/// Strip the directory so "src/main.rs" and a 90-char absolute path read the same.
fn basename(p: &str) -> String {
    p.rsplit('/').next().unwrap_or(p).to_string()
}

/// Record the verb, not the plumbing: first real command, no comments, no chains.
fn head_of_command(cmd: &str) -> String {
    cmd.split(['\n', ';'])
        .map(str::trim_start)
        .find(|piece| !piece.is_empty() && !piece.starts_with('#'))
        .map_or_else(String::new, |piece| squeeze(piece, CMD_CHARS))
}

fn collect_tool_use(d: &mut Digest, block: &Value) {
    let name = hook::field(block, "name");
    let Some(input) = block.get("input") else {
        return;
    };
    match name {
        "Edit" | "Write" | "NotebookEdit" | "MultiEdit" => {
            push_unique(&mut d.files, basename(hook::field(input, "file_path")), MAX_FILES);
        }
        "Bash" => {
            push_unique(&mut d.commands, head_of_command(hook::field(input, "command")), MAX_CMDS);
        }
        "Skill" => push_unique(&mut d.skills, hook::field(input, "skill").to_string(), MAX_CMDS),
        "Task" | "Agent" => {
            let agent = hook::field(input, "subagent_type");
            if !agent.is_empty() {
                push_unique(&mut d.skills, format!("agent:{agent}"), MAX_CMDS);
            }
        }
        _ => {}
    }
}

/// Failed tool calls arrive as user-role `tool_result` blocks flagged `is_error`.
fn count_failures(d: &mut Digest, content: &Value) {
    let Some(blocks) = content.as_array() else {
        return;
    };
    d.failures += blocks
        .iter()
        .filter(|b| hook::field(b, "type") == "tool_result")
        .filter(|b| b.get("is_error").and_then(Value::as_bool).unwrap_or(false))
        .count();
}

/// Digest of the LAST turn in a transcript.
///
/// Single pass, no buffering: every real user message resets the accumulator, so
/// whatever survives to EOF belongs to the final turn. Hook injections and
/// slash-command envelopes wear the user role and must not start a new turn, which is
/// what `is_noise` is doing here.
pub fn digest(transcript_path: &str) -> Digest {
    let mut d = Digest::default();
    let Ok(file) = std::fs::File::open(transcript_path) else {
        return d;
    };
    for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("isSidechain").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        let entry_type = hook::field(&v, "type");
        let message = v.get("message");
        let role = message.map_or("", |m| hook::field(m, "role"));
        let content = message.and_then(|m| m.get("content"));

        if entry_type == "user" || role == "user" {
            let txt = content.map(text_of).unwrap_or_default();
            if !txt.trim().is_empty() && !is_noise(&txt) {
                d = Digest { ask: txt, ..Digest::default() };
            } else if let Some(c) = content {
                count_failures(&mut d, c);
            }
            continue;
        }

        if entry_type == "assistant" || role == "assistant" {
            let Some(content) = content else { continue };
            if let Some(blocks) = content.as_array() {
                for block in blocks.iter().filter(|b| hook::field(b, "type") == "tool_use") {
                    collect_tool_use(&mut d, block);
                }
            }
            let txt = text_of(content);
            if !txt.trim().is_empty() {
                d.outcome = squeeze(&txt, OUTCOME_CHARS);
            }
        }
    }
    d
}

/// The inbox entry for a digest: one flagged line, plus indented detail when the turn
/// did any work. This is the format `cml consolidate` reads back.
pub fn entry_line(d: &Digest, at: i64) -> String {
    let flag = if looks_like_correction(&d.ask) { "correction?" } else { "note" };
    let mut line = format!("- [{}] ({flag}) {}\n", paths::iso_minute(at), squeeze(&d.ask, ASK_CHARS));
    for extra in d.detail_lines() {
        line.push_str(&extra);
        line.push('\n');
    }
    line
}

pub fn capture(_args: &[String]) -> crate::R<i32> {
    // No passthrough JSON: the Stop hook's stdout is not a channel, and the C++ tree
    // printed nothing here either.
    let Some(payload) = hook::read() else {
        return Ok(0);
    };
    let cwd = hook::field(&payload, "cwd");
    let transcript = hook::field(&payload, "transcript_path");
    if cwd.is_empty() || transcript.is_empty() {
        return Ok(0);
    }

    let d = digest(transcript);
    if d.ask.is_empty() || is_noise(&d.ask) {
        return Ok(0);
    }

    let inbox = paths::inbox_path_for(cwd);
    if let Some(dir) = inbox.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&inbox) {
        use std::io::Write;
        let _ = f.write_all(entry_line(&d, paths::now_secs()).as_bytes());
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_transcript(name: &str, lines: &[&str]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("cml-{name}-{}.jsonl", std::process::id()));
        std::fs::write(&path, lines.join("\n")).unwrap();
        path
    }

    #[test]
    fn the_last_real_ask_wins_and_carries_its_work() {
        let path = write_transcript(
            "digest",
            &[
                r#"{"type":"user","message":{"role":"user","content":"first ask, superseded"}}"#,
                r#"{"type":"user","message":{"role":"user","content":"no, port recall.cpp instead"}}"#,
                // One JSON document per line: this is what a real .jsonl transcript is.
                // r##"..."## because the payload itself contains `"#` — a bash comment
                // inside a JSON string. With a single hash the raw literal ends there,
                // and the rest of the line parses as Rust.
                r##"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/a/b/recall.rs"}},{"type":"tool_use","name":"Bash","input":{"command":"# a comment\ncargo test --all"}},{"type":"tool_use","name":"Skill","input":{"skill":"ponytail"}},{"type":"text","text":"ported it"}]}}"##,
                r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","is_error":true}]}}"#,
                r#"{"isSidechain":true,"type":"user","message":{"role":"user","content":"a subagent ask"}}"#,
            ],
        );
        let d = digest(path.to_str().unwrap());
        assert_eq!(d.ask, "no, port recall.cpp instead");
        assert_eq!(d.files, vec!["recall.rs"]);
        assert_eq!(d.commands, vec!["cargo test --all"], "comments and chains are plumbing");
        assert_eq!(d.skills, vec!["ponytail"]);
        assert_eq!(d.failures, 1);
        assert_eq!(d.outcome, "ported it");

        let line = entry_line(&d, 1_754_000_000);
        assert!(line.starts_with("- [2025-07-31 22:13] (correction?) no, port recall.cpp instead\n"));
        assert!(line.contains("    files: recall.rs  \u{b7}  ran: cargo test --all  \u{b7}  skills: ponytail  \u{b7}  1 failed\n"));
        assert!(line.ends_with("    did: ported it\n"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn an_envelope_does_not_start_a_turn() {
        let path = write_transcript(
            "envelope",
            &[
                r#"{"type":"user","message":{"role":"user","content":"the real ask about the touchpad"}}"#,
                r#"{"type":"user","message":{"role":"user","content":"<command-message>ponytail</command-message>"}}"#,
                r#"{"type":"user","message":{"role":"user","content":"ok"}}"#,
            ],
        );
        let d = digest(path.to_str().unwrap());
        assert_eq!(d.ask, "the real ask about the touchpad");
        assert_eq!(entry_line(&d, 0).split(' ').nth(3), Some("(note)"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_missing_or_corrupt_transcript_is_an_empty_digest_not_a_crash() {
        assert!(digest("/nonexistent/transcript.jsonl").ask.is_empty());
        let path = write_transcript("junk", &["{not json", "", "[]", "null"]);
        assert!(digest(path.to_str().unwrap()).ask.is_empty());
        let _ = std::fs::remove_file(path);
    }
}
