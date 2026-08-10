//! `cml recall` — the read half of the memory loop, fired on every prompt without the
//! model being consulted (see `retrieve.rs` for why).
//!
//! This file owns the gate the retrieval core deliberately does not: a row already
//! injected this session is not injected twice. And it owns the contract that whatever
//! happens in here, the prompt still goes through.

pub mod hook;
pub mod retrieve;

use rusqlite::Connection;

pub use self::retrieve::{retrieve, scene_hit, Hit, Opts};

/// A briefing, not a transcript.
const MAX_HITS: usize = 3;

const PREAMBLE: &str = "[cml recall] from your own history, matched on this prompt — \
it is what was true when written, so check it still holds before relying on it:";

pub fn run(_args: &[String]) -> crate::R<i32> {
    let Some(payload) = hook::read() else {
        return hook::passthrough();
    };
    let prompt = hook::field(&payload, "prompt");
    if prompt.is_empty() {
        return hook::passthrough();
    }
    let session = hook::field(&payload, "session_id");

    // Read-write: the dedupe table is written from here. A machine without an index
    // yet is not an error, it is a session with nothing to recall.
    let Ok(conn) = crate::db::open() else {
        return hook::passthrough();
    };

    match brief(&conn, prompt, session) {
        Some(msg) => hook::inject("UserPromptSubmit", &msg),
        None => hook::passthrough(),
    }
}

/// The whole decision, with the database as its only input — so it can be exercised
/// without a process, a payload, or a hook.
fn brief(conn: &Connection, prompt: &str, session: &str) -> Option<String> {
    // L2 first, and it gets one line of three. A scene says what a whole session
    // concluded, which is the one thing no single row can say; behind the row lanes it
    // would compete for a budget they have already spent, which is exactly how the work
    // lane sat unreachable while being fully wired.
    let scene = scene_hit(conn, prompt, session);

    // Over-fetch: rows already injected this session are skipped below, and the next
    // fresh one should take the freed slot rather than leaving the briefing short.
    let conv = retrieve(conn, prompt, session, MAX_HITS * 4, Opts::default());

    // The work — commands and their output, in its own table. It gets a RESERVED slot,
    // not an appended one: appending put it after the conversation hits, which had
    // already spent the whole three-line budget, so 29,703 tool rows stayed unreachable
    // exactly as if the lane had never been wired up. A lane that only runs when the
    // other lane comes up short is not wired up.
    let work = retrieve(conn, prompt, session, 2, Opts { tools: true, ..Opts::default() });
    if conv.is_empty() && work.is_empty() && scene.is_none() {
        return None;
    }

    // Three lines, still. A scene spends one of them, it does not add a fourth: this is
    // seen on every prompt, and growing the budget is how a briefing becomes wallpaper.
    let mut lines: Vec<String> = Vec::new();
    if let Some(s) = scene {
        if fresh(conn, session, &s) {
            lines.push(s.line);
        }
    }
    let conv_budget = MAX_HITS - lines.len() - usize::from(!work.is_empty());
    for h in conv {
        if lines.len() >= conv_budget {
            break;
        }
        if fresh(conn, session, &h) {
            lines.push(h.line);
        }
    }
    for h in work {
        if lines.len() >= MAX_HITS {
            break;
        }
        if fresh(conn, session, &h) {
            lines.push(format!("(ran) {}", h.line));
        }
    }
    if lines.is_empty() {
        return None;
    }

    let mut msg = String::from(PREAMBLE);
    for l in &lines {
        msg.push_str("\n\u{b7} "); // U+00B7 MIDDLE DOT
        msg.push_str(l);
    }
    Some(msg)
}

/// Claim `hit` for this session. False means it was already injected here.
///
/// Keyed on `stable_key` rather than rowid so the claim survives a reindex. With no
/// session id there is nothing to dedupe against, and an unwritable table is not a
/// reason to withhold a briefing — both say "fresh" rather than "silent".
fn fresh(conn: &Connection, session: &str, hit: &Hit) -> bool {
    if session.is_empty() {
        return true;
    }
    match conn.execute(
        "INSERT OR IGNORE INTO recalled(session, key) VALUES(?1, ?2)",
        rusqlite::params![session, hit.key],
    ) {
        Ok(changed) => changed != 0,
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::ensure_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO mem(text, role, project, session, ts, file) VALUES \
             ('the touchpad tap-drag fix lives in xinput properties', 'assistant', 'omc', \
              's-old', '2026-08-09T10:00:00Z', 'f')",
            [],
        )
        .unwrap();
        conn
    }

    const ASK: &str = "where did we land on the touchpad tapdrag xinput thing";

    #[test]
    fn a_briefing_is_injected_once_per_session_then_never_again() {
        let conn = seeded();
        let first = brief(&conn, ASK, "s-live").expect("the row must be reachable");
        assert!(first.starts_with("[cml recall] from your own history"));
        assert!(first.contains("\n\u{b7} 2026-08-09 assistant/omc: "), "{first}");

        // The third gate: the same prompt in the same session says nothing new.
        assert!(brief(&conn, ASK, "s-live").is_none());
        // ...but a different session has not seen it.
        assert!(brief(&conn, ASK, "s-other").is_some());
    }

    #[test]
    fn dedupe_is_keyed_on_content_so_it_survives_a_reindex() {
        let conn = seeded();
        assert!(brief(&conn, ASK, "s-live").is_some());
        // Reindexing renumbers rowids; the row itself is unchanged.
        conn.execute("DELETE FROM mem", []).unwrap();
        conn.execute(
            "INSERT INTO mem(rowid, text, role, project, session, ts, file) VALUES \
             (999, 'the touchpad tap-drag fix lives in xinput properties', 'assistant', 'omc', \
              's-old', '2026-08-09T10:00:00Z', 'f')",
            [],
        )
        .unwrap();
        assert!(
            brief(&conn, ASK, "s-live").is_none(),
            "a new rowid for the same row must not re-inject it"
        );
    }

    #[test]
    fn no_session_id_means_no_dedupe_rather_than_no_briefing() {
        let conn = seeded();
        assert!(brief(&conn, ASK, "").is_some());
        assert!(brief(&conn, ASK, "").is_some(), "without a session there is nothing to dedupe");
    }

    #[test]
    fn a_prompt_that_asks_nothing_gets_nothing() {
        let conn = seeded();
        assert!(brief(&conn, "ok", "s-live").is_none());
        assert!(brief(&conn, "", "s-live").is_none());
    }

    #[test]
    fn the_work_lane_keeps_its_reserved_slot() {
        let conn = seeded();
        // Three conversation rows would spend the whole budget on their own.
        for i in 0..3 {
            conn.execute(
                "INSERT INTO mem(text, role, project, session, ts, file) VALUES \
                 (?1, 'user', 'omc', ?2, '2026-08-09T09:00:00Z', 'f')",
                rusqlite::params![
                    format!("touchpad tapdrag xinput question number {i} about the pointer"),
                    format!("s-{i}")
                ],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO work(text, role, project, session, ts, file) VALUES \
             ('xinput set-prop tapdrag touchpad 1', 'tool', 'omc', 's-old', \
              '2026-08-09T10:00:00Z', 'f')",
            [],
        )
        .unwrap();

        let msg = brief(&conn, ASK, "s-live").unwrap();
        assert!(msg.contains("\u{b7} (ran) "), "the tools lane must not be crowded out: {msg}");
        assert_eq!(msg.matches("\n\u{b7} ").count(), 3, "three lines, still");
    }
}
