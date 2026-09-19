//! jcode: one JSON file per session under `~/.jcode/sessions`.
//!
//! The whole conversation is a single document, not a line-per-entry stream, so
//! the reader is a `from_reader` and a walk rather than a `BufRead` loop. Files
//! run to a few hundred kB, which is the size that makes that acceptable.
//!
//! Field shape confirmed against 629 real session files on the machine this was
//! written on: `{ id, cwd, messages: [ { role, content, timestamp } ] }`, where
//! `content` is either a plain string or an array of blocks carrying `text`.
//! Both spellings of the timestamp (`timestamp` and `ts`, seconds or millis) are
//! accepted, because the format drifted across jcode versions and old sessions
//! are exactly the ones worth remembering.

use std::path::PathBuf;

use serde_json::Value;

use super::{Harness, Role, Session, Turn};

pub fn root() -> PathBuf {
    if let Some(h) = std::env::var_os("JCODE_HOME") {
        return PathBuf::from(h).join("sessions");
    }
    crate::paths::home_dir().join(".jcode").join("sessions")
}

pub fn sessions() -> Vec<Session> {
    let dir = root();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let path = e.path();
        // `.bak` and `.journal.jsonl` sit beside the real file and hold the same
        // conversation; indexing them would triple every jcode row.
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Some(meta) = crate::index::files::meta_of(&path) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let id = v
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
            });
        out.push(Session {
            harness: Harness::Jcode,
            cwd: str_field(&v, &["cwd", "working_dir", "workingDirectory"]).unwrap_or_default(),
            turns: turns(&v),
            id,
            file: path.to_string_lossy().into_owned(),
            size: meta.size,
            mtime: meta.mtime,
        });
    }
    out
}

fn turns(v: &Value) -> Vec<Turn> {
    let Some(msgs) = v
        .get("messages")
        .or_else(|| v.get("history"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    msgs.iter()
        .filter_map(|m| {
            let role = match m.get("role").and_then(Value::as_str)? {
                "user" | "human" => Role::User,
                "assistant" | "model" => Role::Assistant,
                "tool" | "tool_result" => Role::Tool,
                _ => return None,
            };
            let content = m.get("content")?;
            // jcode inlines tool calls as blocks inside an assistant message, so
            // the role field alone would file a shell command under prose. The
            // block type is the honest signal, and the tool lane is where a
            // command and its output are actually searched for.
            let (role, text) = match tool_text(content) {
                Some(t) => (Role::Tool, t),
                None => (role, content_text(content)),
            };
            (!text.trim().is_empty()).then(|| Turn { role, text, ts: timestamp(m) })
        })
        .collect()
}

/// The command that ran, or what it printed — `None` when this message carries
/// no tool block at all.
fn tool_text(c: &Value) -> Option<String> {
    let arr = c.as_array()?;
    let mut out = String::new();
    for b in arr {
        let kind = b.get("type").and_then(Value::as_str).unwrap_or_default();
        if kind != "tool_use" && kind != "tool_result" {
            continue;
        }
        if let Some(name) = b.get("name").and_then(Value::as_str) {
            push(&mut out, name);
        }
        match b.get("input").and_then(Value::as_object) {
            Some(input) => {
                for v in input.values().filter_map(Value::as_str) {
                    push(&mut out, v);
                }
            }
            None => {
                if let Some(inner) = b.get("content") {
                    let t = content_text(inner);
                    push(&mut out, &t);
                }
            }
        }
    }
    (!out.trim().is_empty()).then_some(out)
}

/// The same 1200-byte ceiling `index::entry` puts on a Claude tool row: a
/// megabyte of source is not a memory, and the two lanes must not disagree about
/// how much of one is worth keeping.
const TOOL_MAX: usize = 1200;

fn push(out: &mut String, s: &str) {
    if out.len() >= TOOL_MAX || s.is_empty() {
        return;
    }
    if !out.is_empty() {
        out.push(' ');
    }
    let room = TOOL_MAX.saturating_sub(out.len()).min(s.len());
    let mut end = room;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    out.push_str(&s[..end]);
}

/// A message's clock, whichever of the three spellings this file's writer used.
///
/// jcode writes RFC-3339 with nanoseconds (`2026-08-19T15:03:34.783145661Z`),
/// which is already the column's format and passes straight through.
fn timestamp(m: &Value) -> String {
    for key in ["timestamp", "ts", "time", "created_at"] {
        match m.get(key) {
            Some(Value::String(s)) if !s.is_empty() => return s.clone(),
            Some(Value::Number(n)) => {
                let Some(n) = n.as_i64() else { continue };
                // A value past this bound cannot be seconds — year 33658 — so it
                // is millis. Cheaper and more honest than trusting a field name
                // that already proved unstable.
                return if n > 1_000_000_000_000 {
                    super::ts_from_millis(n)
                } else {
                    super::ts_from_secs(n)
                };
            }
            _ => {}
        }
    }
    String::new()
}

/// String, or an array of blocks with `text` in them.
fn content_text(c: &Value) -> String {
    if let Some(s) = c.as_str() {
        return s.to_string();
    }
    let Some(arr) = c.as_array() else {
        return String::new();
    };
    arr.iter()
        .filter_map(|b| {
            b.as_str().map(str::to_string).or_else(|| {
                b.get("text").and_then(Value::as_str).map(str::to_string)
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn str_field(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_and_millis_are_told_apart() {
        let secs = serde_json::json!({"timestamp": 1_754_000_000i64});
        let millis = serde_json::json!({"timestamp": 1_754_000_000_000i64});
        assert_eq!(timestamp(&secs), "2025-07-31T22:13:20Z");
        assert_eq!(timestamp(&millis), "2025-07-31T22:13:20Z");
        // jcode's real spelling is already the column's format.
        let iso = serde_json::json!({"timestamp": "2026-08-19T15:03:34.783145661Z"});
        assert_eq!(timestamp(&iso), "2026-08-19T15:03:34.783145661Z");
        assert_eq!(timestamp(&serde_json::json!({})), "");
    }

    #[test]
    fn content_reads_both_shapes() {
        assert_eq!(content_text(&serde_json::json!("plain")), "plain");
        let blocks = serde_json::json!([{"type": "text", "text": "a"}, {"text": "b"}]);
        assert_eq!(content_text(&blocks), "a b");
        assert_eq!(content_text(&serde_json::json!(7)), "");
    }

    #[test]
    fn turns_keep_role_and_drop_blanks() {
        let v = serde_json::json!({
            "messages": [
                {"role": "user", "content": "hello there"},
                {"role": "assistant", "content": "   "},
                {"role": "system", "content": "ignored"},
                {"role": "tool", "content": [{"type": "text", "text": "ran it"}]},
            ]
        });
        let t = turns(&v);
        assert_eq!(t.len(), 2, "blank and system turns must not survive");
        assert_eq!(t[0].role, Role::User);
        assert_eq!(t[1].role, Role::Tool);
    }

    /// jcode's real shape: the tool call is a block inside an *assistant*
    /// message, so trusting `role` alone files a shell command under prose.
    #[test]
    fn an_inlined_tool_call_lands_in_the_tool_lane() {
        let v = serde_json::json!({
            "messages": [{
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "t1",
                    "name": "Bash",
                    "input": {"command": "cargo test --release"}
                }]
            }]
        });
        let t = turns(&v);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].role, Role::Tool, "a tool_use block is work, not prose");
        assert!(t[0].text.contains("Bash"), "{}", t[0].text);
        assert!(t[0].text.contains("cargo test"), "{}", t[0].text);
    }

    #[test]
    fn a_tool_row_is_capped_on_a_character_boundary() {
        let v = serde_json::json!({
            "messages": [{
                "role": "user",
                "content": [{"type": "tool_result", "content": "\u{e9}".repeat(4000)}]
            }]
        });
        let t = turns(&v);
        assert_eq!(t.len(), 1);
        assert!(t[0].text.len() <= TOOL_MAX, "cap not applied: {}", t[0].text.len());
        assert!(t[0].text.chars().all(|c| c == '\u{e9}'));
    }
}
