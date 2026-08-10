//! What is worth remembering, what a prompt is asking about, and how a row is named.
//!
//! Three filters and two keys, all pure functions over `&str`, because every one of
//! them is a gate the rest of the loop trusts and none of them can be checked from
//! the outside once it is buried in a hook.
//!
//! Two of the filters were wrong until 2026-07-21 and made the index 25% junk:
//! `is_noise` matched `<command-name>` but transcripts store the envelope as
//! `<command-message>`, so every slash-command row walked in; `looks_like_correction`
//! matched bare words including "no", "why" and "fix", so "why are people hyped about
//! shodan" scored as a correction and the flag carried no information at all. Both
//! tables below are therefore data, not cleverness — and both are tested.
//!
//! Case folding is ASCII-only on purpose. FTS5's `unicode61` tokenizer folds Cyrillic
//! and accents for the retrieval itself; this is the *overlap floor*, and both sides of
//! every comparison go through the same function here, so the two stay consistent.

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

/// An OR query over a whole pasted document is not a query.
const MAX_TERMS: usize = 12;

/// Machine-generated turns that wear a human role in the transcript.
const ENVELOPE_PREFIXES: [&str; 18] = [
    "Caveat:",
    "<command-message>",
    "<command-name>",
    "<local-command",
    "<system-reminder>",
    "<task-notification>",
    "<channel source=",
    "<user-prompt-submit-hook>",
    "<teammate-message",
    "Another Claude session sent a message:",
    "[Image: source:",
    "Base directory for this skill",
    "Launching skill:",
    "Stop hook feedback:",
    "[Request interrupted",
    "This session is being continued",
    "Continue from where you left off",
    "(Re-invocation of",
];

/// A whole turn that is one of these is an acknowledgement, not a memory.
const ACKS: [&str; 34] = [
    "ok", "okay", "k", "yes", "yep", "yeah", "no", "nope", "sure", "go", "go ahead",
    "do it", "continue", "continue please", "proceed", "next", "thanks", "thank you",
    "ty", "nice", "cool", "great", "perfect", "y", "n", "/compact", "stop", "wait",
    "hi", "hey", "hello", "yes please", "no please", "implement please",
];

/// A correction is a *phrase*, never a bare word. A false negative costs one unflagged
/// line; a false positive poisons every entry in the inbox.
const CORRECTION_PHRASES: [&str; 29] = [
    "no,", "nope", "dont ", "do not ", "wrong", "not what", "thats not", "instead of",
    "why did you", "why do you", "why are you", "you broke", "you missed", "you forgot",
    "shouldnt", "should not", "isnt ", "doesnt ", "never do", "revert", "undo ",
    "actually no", "not correct", "not right", "didnt work", "doesnt work", "still no",
    "nothing changed", "stop doing",
];

/// Function words carry no retrieval signal but would dominate an OR query.
const STOP: [&str; 80] = [
    "the", "and", "for", "are", "but", "not", "you", "all", "can", "was", "one", "our",
    "out", "how", "its", "who", "did", "yes", "get", "has", "him", "his", "she", "her",
    "too", "use", "why", "what", "this", "that", "with", "have", "from", "they", "been",
    "were", "when", "your", "said", "each", "which", "them", "then", "than", "some",
    "would", "there", "their", "about", "into", "just", "like", "make", "made", "does",
    "doing", "dont", "cant", "wont", "should", "could", "need", "want", "please", "okay",
    "also", "only", "very", "more", "most", "much", "still", "again", "now", "here",
    "let", "lets", "will", "any", "because",
];

/// Lowercase and drop apostrophes, both the ASCII one and U+2019, so "doesn't",
/// "doesn’t" and "doesnt" all reach the same phrase table.
fn fold(s: &str) -> String {
    s.chars()
        .filter(|c| *c != '\'' && *c != '\u{2019}')
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Collapse runs of whitespace, then clip to `max` *bytes*, never mid-character.
pub fn squeeze(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max + 4));
    for word in s.split_ascii_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    if out.len() <= max {
        return out;
    }
    let mut cut = max;
    while cut > 0 && !out.is_char_boundary(cut) {
        cut -= 1;
    }
    out.truncate(cut);
    out.push('\u{2026}');
    out
}

/// Slash-command envelopes, hook injections, task notifications, bare acks.
pub fn is_noise(text: &str) -> bool {
    let t = text.trim_matches(|c: char| c.is_ascii_whitespace());
    if t.is_empty() {
        return true;
    }
    if ENVELOPE_PREFIXES.iter().any(|p| t.starts_with(*p)) {
        return true;
    }
    if t.contains("hook additional context:") || t.contains("hook success:") {
        return true;
    }
    // Strip surrounding punctuation but keep '/' so "/compact" still matches.
    let bare = t.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '/'));
    ACKS.contains(&fold(bare).as_str())
}

pub fn looks_like_correction(text: &str) -> bool {
    let lowered = fold(text);
    CORRECTION_PHRASES.iter().any(|p| lowered.contains(*p))
}

/// Content words worth querying on: lowercased, >= 3 bytes, de-duplicated, function
/// words dropped, capped.
///
/// Letters, digits and every non-ASCII character stay in the token, so a Russian word
/// survives as one unit instead of shattering into noise.
pub fn content_terms(prompt: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut cur = String::new();
    for c in prompt.chars() {
        if c.is_ascii_alphanumeric() || !c.is_ascii() {
            cur.push(c.to_ascii_lowercase());
        } else {
            flush(&mut cur, &mut out, &mut seen);
        }
    }
    flush(&mut cur, &mut out, &mut seen);
    out.truncate(MAX_TERMS);
    out
}

fn flush(cur: &mut String, out: &mut Vec<String>, seen: &mut HashSet<String>) {
    if cur.len() >= 3 && !STOP.contains(&cur.as_str()) && seen.insert(cur.clone()) {
        out.push(std::mem::take(cur));
    }
    cur.clear();
}

/// How many distinct query terms appear in `text`. The relevance floor: one shared
/// word with a prompt is a coincidence, two is a topic.
pub fn term_overlap(text: &str, terms: &[String]) -> usize {
    let hay = text.to_ascii_lowercase();
    terms.iter().filter(|t| hay.contains(t.as_str())).count()
}

/// Names a row by what it *is*, not by where it sits, so dedupe and curation survive
/// a reindex that renumbers every rowid.
pub fn stable_key(session: &str, ts: &str, role: &str, text: &str) -> String {
    let head: String = text.chars().take(64).collect();
    format!("{session}|{ts}|{role}|{head}")
}

/// `stable_key` -> curated gist: what the briefing displays for a distilled row.
///
/// Missing table, unreadable index, anything: an empty map, never an error. Every
/// caller is on a hook path where the honest fallback is the raw row.
pub fn gist_lookup(conn: &Connection) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(mut st) = conn.prepare("SELECT key, gist FROM distilled WHERE gist != ''") else {
        return out;
    };
    let Ok(rows) = st.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    }) else {
        return out;
    };
    for (key, gist) in rows.flatten() {
        out.insert(key, gist);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_envelope_that_leaked_85_rows_is_noise() {
        // The exact regression: the old list had <command-name>, which is the SECOND
        // tag in the envelope, so every slash-command row was indexed as user prose.
        assert!(is_noise("<command-message>ponytail is running…</command-message>"));
        assert!(is_noise("<command-name>/ponytail</command-name>"));
        assert!(is_noise("<system-reminder>ignore me</system-reminder>"));
        assert!(is_noise("PreToolUse:Bash hook additional context: do the thing"));
        assert!(is_noise(""));
        assert!(is_noise("   \n\t "));
    }

    #[test]
    fn acks_are_noise_but_real_asks_are_not() {
        for ack in ["ok", "OK!", "  yes.  ", "do it", "/compact", "thanks"] {
            assert!(is_noise(ack), "{ack:?} should be noise");
        }
        assert!(!is_noise("ok but why does the recall hook fire twice"));
        assert!(!is_noise("port recall.cpp to rust"));
    }

    #[test]
    fn corrections_are_phrases_not_bare_words() {
        assert!(looks_like_correction("no, that's not what I asked for"));
        assert!(looks_like_correction("why did you rewrite the whole file"));
        assert!(looks_like_correction("doesn\u{2019}t work, still no output"));
        // The false positive that made the flag meaningless.
        assert!(!looks_like_correction("why are people hyped about shodan"));
        assert!(!looks_like_correction("fix the build"));
    }

    #[test]
    fn squeeze_collapses_then_clips_on_a_char_boundary() {
        assert_eq!(squeeze("  a\t\tb \n c  ", 100), "a b c");
        assert_eq!(squeeze("abcdef", 3), "abc\u{2026}");
        // A multibyte char straddling the cut must not be split.
        let clipped = squeeze("aa\u{044f}\u{044f}\u{044f}", 3);
        assert!(clipped.is_char_boundary(clipped.len()));
        assert_eq!(clipped, "aa\u{2026}");
    }

    #[test]
    fn content_terms_drops_stopwords_shorts_and_duplicates() {
        let t = content_terms("Why does the recall hook recall the same recall row?");
        assert_eq!(t, vec!["recall", "hook", "same", "row"]);
        // "yes" and "do it" ask nothing: under the two-term floor the caller applies.
        assert!(content_terms("do it").len() < 2);
        // Non-ASCII survives as one token rather than shattering per byte.
        assert_eq!(content_terms("\u{043a}\u{043b}\u{044e}\u{0447}"), vec!["\u{043a}\u{043b}\u{044e}\u{0447}"]);
    }

    #[test]
    fn overlap_is_the_two_word_floor() {
        let terms = content_terms("touchpad tap drag regression");
        assert!(term_overlap("the TOUCHPAD tap-drag fix", &terms) >= 2);
        assert_eq!(term_overlap("nothing in common", &terms), 0);
    }

    #[test]
    fn stable_key_survives_a_reindex_and_clips_at_64_chars() {
        let long = "\u{044f}".repeat(100);
        let key = stable_key("s1", "2026-08-10", "user", &long);
        assert!(key.starts_with("s1|2026-08-10|user|"));
        assert_eq!(key.chars().filter(|c| *c == '\u{044f}').count(), 64);
    }
}
