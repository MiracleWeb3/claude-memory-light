//! The hook boundary: read a Claude Code payload off stdin, print a reply that never
//! blocks the session.
//!
//! One module for all five hooks, because the contract is the same for all of them and
//! it is absolute: *whatever happens in here, the session goes through*. Empty stdin,
//! truncated JSON, a payload with the wrong shape, a missing index — every one of those
//! ends at `passthrough()`, never at an error and never at a panic. A hook that dies
//! breaks every session start on the machine, which is a far worse failure than a
//! briefing that did not appear.
//!
//! That is also why nothing here returns `Result` to the caller and why the writes go
//! through `write!` rather than `println!`: a closed stdout would panic the process on
//! the macro, and a broken pipe is not worth taking a session down for.

use std::io::Write;

use serde_json::Value;

/// Read and parse the payload. `None` means "say nothing and let the prompt through",
/// which is the correct outcome for empty *and* for malformed input alike.
///
/// A terminal on stdin short-circuits to `None` before reading a byte. These commands
/// are also typed by hand, and `read_to_string` on a tty waits for a Ctrl-D that is
/// never coming — a hook that hangs takes the whole session with it.
pub fn read() -> Option<Value> {
    use std::io::IsTerminal;
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return None;
    }
    parse(&std::io::read_to_string(stdin).ok()?)
}

/// Split from `read` so the malformed-input contract is testable without a process.
pub fn parse(payload: &str) -> Option<Value> {
    if payload.trim().is_empty() {
        return None;
    }
    serde_json::from_str(payload).ok()
}

/// A string field, or `""` — every field this tree reads is legitimately absent on some
/// event kind, so a miss is a value, not an error.
pub fn field<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// The reply that changes nothing.
pub fn passthrough() -> crate::R<i32> {
    emit("{\"continue\": true}");
    Ok(0)
}

/// The reply that injects `msg` as context for `event` (`SessionStart` or
/// `UserPromptSubmit`).
pub fn inject(event: &str, msg: &str) -> crate::R<i32> {
    // serde_json owns the escaping: the C++ tree hand-rolled it and spent a bug on
    // emitting numeric escapes where the named ones belonged.
    let quoted = Value::String(msg.to_string()).to_string();
    emit(&format!(
        "{{\"continue\": true, \"hookSpecificOutput\": {{\"hookEventName\": \"{event}\", \
         \"additionalContext\": {quoted}}}}}"
    ));
    Ok(0)
}

fn emit(line: &str) {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_and_empty_payloads_parse_to_nothing() {
        // Each of these has reached a hook in production. None may reach a panic.
        for bad in ["", "   \n", "{", "not json at all", "[1,2,3", "\u{0}"] {
            assert!(parse(bad).is_none(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn a_real_user_prompt_submit_payload_parses() {
        let v = parse(
            r#"{"session_id":"abc","transcript_path":"/t.jsonl","cwd":"/home/u/dev",
                "hook_event_name":"UserPromptSubmit","prompt":"why is recall silent"}"#,
        )
        .expect("a well-formed payload must parse");
        assert_eq!(field(&v, "prompt"), "why is recall silent");
        assert_eq!(field(&v, "session_id"), "abc");
        assert_eq!(field(&v, "cwd"), "/home/u/dev");
        // A field the event does not carry is empty, not an error.
        assert_eq!(field(&v, "source"), "");
        // A non-string field reads as absent rather than as its debug form.
        assert_eq!(field(&parse(r#"{"prompt":42}"#).unwrap(), "prompt"), "");
    }

    #[test]
    fn valid_json_that_is_not_an_object_still_does_not_panic() {
        let v = parse("[1,2,3]").expect("an array is valid JSON");
        assert_eq!(field(&v, "prompt"), "");
    }
}
