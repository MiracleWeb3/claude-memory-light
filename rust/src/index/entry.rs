//! One `.jsonl` line -> at most one entry, plus the filters that judge its text.
//!
//! Claude Code writes each content block as its own line, so narration cannot be
//! spotted from a single entry. Parse the whole stream first, then keep an assistant
//! text only if nothing tool-related follows it before the next human message: that
//! is the turn's actual answer, everything else is "doing X now".
//!
//! # The sidechain branch is deliberately absent
//!
//! The C++ parser dropped every entry carrying `isSidechain: true`. That flag marks
//! subagent turns, and *every* line of a `subagents/*.jsonl` transcript carries it —
//! 786 of 999 transcript files on this machine, all of the parallel work, invisible.
//! Walking those directories without deleting this branch would index 786 files and
//! write zero rows, which is the failure that looks like success.
//!
//! It was there because subagent turns used to be inlined into the parent
//! transcript. They are not any more: 0 of 214 top-level session files on this
//! machine contain a sidechain entry. Legacy files that still do are covered by the
//! `mem` dedup key, which is what makes a continued session safe to index twice.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    AssistantText,
    AssistantTool,
    UserHuman,
    UserTool,
    Summary,
}

#[derive(Debug)]
pub struct Entry {
    pub kind: Kind,
    pub text: String,
    pub ts: String,
    /// The entry's own `sessionId`. In a subagent transcript this is the *parent*
    /// session, which is exactly the attribution a subagent row needs.
    pub session: String,
}

/// A file read is a megabyte and a megabyte of source is not a memory. The cap keeps
/// the errno and the head of a stack trace, which is the part that identifies the
/// failure months later.
const TOOL_MAX: usize = 1200;

pub fn parse(path: &Path, session_fallback: &str) -> Vec<Entry> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // filter_map, not map_while: a line that is not valid UTF-8 is skipped, not a
    // reason to stop reading the rest of the session.
    // map_while, not filter_map: a transcript that hits a read error mid-file yields
    // `Err` forever, and filter_map would spin on it rather than stop at the bad line.
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(kind) = field(&v, "type") else {
            continue;
        };

        if kind == "summary" {
            if let Some(s) = field(&v, "summary").filter(|s| !s.trim().is_empty()) {
                out.push(Entry {
                    kind: Kind::Summary,
                    text: s.to_string(),
                    ts: String::new(),
                    session: session_fallback.to_string(),
                });
            }
            continue;
        }
        if kind != "user" && kind != "assistant" {
            continue;
        }
        let Some(content) = v.get("message").and_then(|m| m.get("content")) else {
            continue;
        };
        let ts = field(&v, "timestamp").unwrap_or_default().to_string();
        let session = field(&v, "sessionId")
            .filter(|s| !s.is_empty())
            .unwrap_or(session_fallback)
            .to_string();

        let (kind, text) = if kind == "assistant" {
            if has_block(content, "tool_use") {
                (Kind::AssistantTool, tool_text(content, "tool_use"))
            } else {
                (Kind::AssistantText, text_of(content))
            }
        } else if has_block(content, "tool_result") {
            (Kind::UserTool, tool_text(content, "tool_result"))
        } else {
            (Kind::UserHuman, text_of(content))
        };
        // Tool entries keep their empty text (the length floor judges them later);
        // a blank human or assistant turn is not an entry at all.
        let tool = matches!(kind, Kind::AssistantTool | Kind::UserTool);
        if tool || !text.trim().is_empty() {
            out.push(Entry { kind, text, ts, session });
        }
    }
    out
}

/// True when `entries[i]` is the last assistant text before the next human turn.
pub fn turn_final(entries: &[Entry], i: usize) -> bool {
    entries[i + 1..]
        .iter()
        .find_map(|e| match e.kind {
            // A later text in the same turn; keep looking.
            Kind::AssistantText => None,
            // Work followed, so this was narration.
            Kind::AssistantTool | Kind::UserTool => Some(false),
            // The turn ended here.
            Kind::UserHuman | Kind::Summary => Some(true),
        })
        .unwrap_or(true)
}

fn field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// `content` is either a plain string or an array of blocks.
fn text_of(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    let Some(arr) = content.as_array() else {
        return String::new();
    };
    arr.iter()
        .filter(|b| field(b, "type") == Some("text"))
        .filter_map(|b| field(b, "text"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn has_block(content: &Value, kind: &str) -> bool {
    content
        .as_array()
        .is_some_and(|a| a.iter().any(|b| field(b, "type") == Some(kind)))
}

/// The command that ran, or what it printed.
///
/// Assistant text is the conversation *about* the work; this is the work. Measured
/// on a real transcript: 741 kB of assistant text kept against 2,751 kB of tool
/// traffic, and the tool half is where `ModuleNotFoundError: No module named
/// 'curl_cffi'` lives — it appears nowhere else.
///
/// One function for both directions because the difference is two field names: a
/// `tool_use` is its name plus its string arguments (a Write's `content` is the
/// file, so non-strings are skipped), a `tool_result` is its content, which nests
/// one level deeper when it is a block array.
fn tool_text(content: &Value, kind: &str) -> String {
    let Some(arr) = content.as_array() else {
        return String::new();
    };
    let mut out = String::new();
    for b in arr.iter().filter(|b| field(b, "type") == Some(kind)) {
        if let Some(name) = field(b, "name") {
            append_capped(&mut out, name);
        }
        match b.get("input").and_then(Value::as_object) {
            Some(input) => {
                for v in input.values().filter_map(Value::as_str) {
                    append_capped(&mut out, v);
                }
            }
            None => match b.get("content") {
                Some(Value::String(s)) => append_capped(&mut out, s),
                Some(Value::Array(inner)) => {
                    for s in inner.iter().filter_map(|ib| field(ib, "text")) {
                        append_capped(&mut out, s);
                    }
                }
                _ => {}
            },
        }
    }
    out
}

fn append_capped(out: &mut String, s: &str) {
    if out.len() >= TOOL_MAX {
        return;
    }
    if !out.is_empty() {
        out.push(' ');
    }
    let room = TOOL_MAX.saturating_sub(out.len()).min(s.len());
    // `str` is UTF-8 by construction, so the cut has to land on a boundary; the C++
    // version sliced bytes and could store a half character in the index.
    let mut end = room;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    out.push_str(&s[..end]);
}

// The noise predicate (envelope prefixes, bare acks, hook markers) lives in
// crate::text, because `loops` and `eval` gate on the identical list. Two copies of
// that table is what produced the `<command-message>` bug: the envelope's FIRST tag
// was missing from one of them, and 85 slash-command rows walked in as user prose.

#[cfg(test)]
mod tests {
    use super::*;

    fn e(kind: Kind) -> Entry {
        Entry { kind, text: String::new(), ts: String::new(), session: String::new() }
    }

    #[test]
    fn only_the_last_assistant_text_of_a_turn_survives() {
        let entries = vec![
            e(Kind::UserHuman),
            e(Kind::AssistantText), // narration: work follows
            e(Kind::AssistantTool),
            e(Kind::AssistantText), // the answer
            e(Kind::UserHuman),
        ];
        assert!(!turn_final(&entries, 1));
        assert!(turn_final(&entries, 3));
    }

    #[test]
    fn tool_text_never_splits_a_multibyte_character() {
        let long = "\u{e9}".repeat(2000);
        let content = serde_json::json!([{"type": "tool_result", "content": long}]);
        let out = tool_text(&content, "tool_result");
        assert!(out.len() <= TOOL_MAX, "cap not applied: {}", out.len());
        assert!(out.chars().all(|c| c == '\u{e9}'));
    }
}
