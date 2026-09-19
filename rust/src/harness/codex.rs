//! Codex CLI: rollout JSONL under `~/.codex/sessions/YYYY/MM/DD/`.
//!
//! The same line-per-entry shape as Claude Code with different key names. Codex
//! wraps each record in `{type, payload}` where the payload is a `ResponseItem`,
//! and text lives in `content[].text` under `input_text` / `output_text` rather
//! than plain `text`.
//!
//! # Confidence
//!
//! Written from the upstream format, not from a live store: `~/.codex` is empty
//! on the machine this was built on. Both the wrapped (`{type,payload}`) and the
//! bare (`{type,role,content}`) spellings are accepted for that reason — the
//! reader costs one extra `.get()` and buys tolerance of a format nobody here
//! could observe.

use std::path::PathBuf;

use serde_json::Value;

use super::{Harness, Role, Session, Turn};

pub fn root() -> PathBuf {
    if let Some(h) = std::env::var_os("CODEX_HOME") {
        return PathBuf::from(h).join("sessions");
    }
    crate::paths::home_dir().join(".codex").join("sessions")
}

pub fn sessions() -> Vec<Session> {
    let mut out = Vec::new();
    let mut stack = vec![root()];
    // The date tree is three levels deep and may hold anything; a walk is
    // shorter than three nested read_dirs and does not care if that changes.
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let path = e.path();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(meta) = crate::index::files::meta_of(&path) else {
                continue;
            };
            let Some(s) = read(&path, meta.size, meta.mtime) else {
                continue;
            };
            out.push(s);
        }
    }
    out
}

fn read(path: &std::path::Path, size: i64, mtime: i64) -> Option<Session> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let mut turns = Vec::new();
    let mut cwd = String::new();
    // `rollout-2026-08-19T15-03-34-<uuid>.jsonl` -> the uuid is the session, but
    // the whole stem is unique and traces back to the file, which is what the
    // `session` column is for.
    let mut id = path.file_stem()?.to_string_lossy().into_owned();

    for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // The session meta line carries the cwd and the real id.
        if let Some(meta) = v.get("payload").filter(|p| {
            v.get("type").and_then(Value::as_str) == Some("session_meta")
                || p.get("cwd").is_some()
        }) {
            if let Some(c) = meta.get("cwd").and_then(Value::as_str) {
                cwd = c.to_string();
            }
            if let Some(i) = meta.get("id").and_then(Value::as_str) {
                id = i.to_string();
            }
        }
        let ts = v
            .get("timestamp")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // Wrapped or bare: the item is the payload if there is one.
        let item = v.get("payload").unwrap_or(&v);
        if let Some(turn) = turn_of(item, ts) {
            turns.push(turn);
        }
    }
    (!turns.is_empty()).then(|| Session {
        harness: Harness::Codex,
        id,
        cwd,
        file: path.to_string_lossy().into_owned(),
        size,
        mtime,
        turns,
    })
}

fn turn_of(item: &Value, ts: String) -> Option<Turn> {
    let kind = item.get("type").and_then(Value::as_str).unwrap_or("message");
    match kind {
        "message" => {
            let role = match item.get("role").and_then(Value::as_str)? {
                "user" => Role::User,
                "assistant" => Role::Assistant,
                // `developer` and `system` are instruction plumbing, including
                // every hook-injected context block. Indexing those would feed
                // cml's own recall output back into cml.
                _ => return None,
            };
            let text = content_text(item.get("content")?);
            (!text.trim().is_empty()).then_some(Turn { role, text, ts })
        }
        "function_call" | "local_shell_call" | "custom_tool_call" => {
            let mut out = String::new();
            for key in ["name", "arguments", "action", "input"] {
                match item.get(key) {
                    Some(Value::String(s)) => push(&mut out, s),
                    Some(v) => push(&mut out, &v.to_string()),
                    None => {}
                }
            }
            (!out.trim().is_empty()).then_some(Turn { role: Role::Tool, text: out, ts })
        }
        "function_call_output" | "custom_tool_call_output" => {
            let out = match item.get("output") {
                Some(Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => return None,
            };
            let mut capped = String::new();
            push(&mut capped, &out);
            (!capped.trim().is_empty()).then_some(Turn { role: Role::Tool, text: capped, ts })
        }
        _ => None,
    }
}

/// `content` is an array of `{type, text}`; Codex spells the types
/// `input_text` and `output_text`.
fn content_text(c: &Value) -> String {
    if let Some(s) = c.as_str() {
        return s.to_string();
    }
    let Some(arr) = c.as_array() else {
        return String::new();
    };
    arr.iter()
        .filter(|b| {
            matches!(
                b.get("type").and_then(Value::as_str),
                Some("input_text" | "output_text" | "text") | None
            )
        })
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &std::path::Path, name: &str, lines: &[&str]) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, lines.join("\n")).unwrap();
        p
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cml-codex-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_wrapped_rollout_yields_prose_and_work_but_not_plumbing() {
        let dir = tmp("wrapped");
        let p = write(
            &dir,
            "rollout-x.jsonl",
            &[
                r#"{"type":"session_meta","payload":{"id":"abc","cwd":"/home/u/dev/app"}}"#,
                r#"{"type":"response_item","timestamp":"2026-08-19T15:03:34Z","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"why is the build slow"}]}}"#,
                r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"injected memory"}]}}"#,
                r#"{"type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"cmd\":\"cargo build\"}"}}"#,
                r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"lto was on"}]}}"#,
            ],
        );
        let s = read(&p, 1, 2).unwrap();
        assert_eq!(s.id, "abc", "the meta line names the session");
        assert_eq!(s.cwd, "/home/u/dev/app");
        let got: Vec<_> = s.turns.iter().map(|t| (t.role, t.text.as_str())).collect();
        assert_eq!(
            got,
            vec![
                (Role::User, "why is the build slow"),
                (Role::Tool, "shell {\"cmd\":\"cargo build\"}"),
                (Role::Assistant, "lto was on"),
            ],
            "developer-role plumbing must not be indexed"
        );
        assert_eq!(s.turns[0].ts, "2026-08-19T15:03:34Z");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bare_item_line_reads_too() {
        let dir = tmp("bare");
        let p = write(
            &dir,
            "rollout-y.jsonl",
            &[r#"{"type":"message","role":"user","content":[{"type":"input_text","text":"bare shape"}]}"#],
        );
        let s = read(&p, 1, 2).unwrap();
        assert_eq!(s.turns.len(), 1);
        assert_eq!(s.turns[0].text, "bare shape");
        assert_eq!(s.id, "rollout-y", "no meta line: the filename is the id");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_transcript_with_no_turns_is_not_a_session() {
        let dir = tmp("empty");
        let p = write(&dir, "rollout-z.jsonl", &["not json", "{}"]);
        assert!(read(&p, 1, 2).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
