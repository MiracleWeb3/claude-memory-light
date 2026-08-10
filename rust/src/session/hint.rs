//! `cml hint` — UserPromptSubmit: notice what kind of thing the user just said, and
//! say so once per session per kind.
//!
//! A suggestion, never a write. The classifier is a phrase table because the alternative
//! — asking a model what a prompt "is" — costs a round trip on every keystroke-to-enter
//! and would be wrong in a different way each time. Once per session per category is the
//! whole anti-wallpaper design: the second preference in a session hints nothing.

use rusqlite::Connection;

use crate::recall::hook;
use crate::text::looks_like_correction;

/// Order is priority: a correction outranks the preference wording inside it. The
/// phrases are matched against the prompt lowercased and space-padded, so " always "
/// cannot fire on "alwaysOn".
pub struct Cat {
    pub name: &'static str,
    pub message: &'static str,
    phrases: &'static [&'static str],
}

const CATS: [Cat; 5] = [
    Cat {
        name: "correction",
        message: "[cml] reads like a correction — once resolved, capture it to memory so it \
                  sticks (learning loop).",
        phrases: &[],
    },
    Cat {
        name: "preference",
        message: "[cml] reads like a durable preference — consider capturing it so future \
                  sessions inherit it.",
        phrases: &[
            " from now on ", " always ", " never ", " prefer", " by default ", " going forward ",
        ],
    },
    Cat {
        name: "decision",
        message: "[cml] reads like a decision — wiki material once it's concluded.",
        phrases: &[" we will ", " let's go with ", " lets go with ", " decided to ", " we choose "],
    },
    Cat {
        name: "method",
        message: "[cml] reads like a handed method — try exactly that first; capture it if it \
                  works.",
        phrases: &[" do it like ", " the fix is ", " try this ", " instead use ", " use this approach "],
    },
    Cat {
        name: "reference",
        message: "[cml] contains a link — worth a wiki/reference note if it should be findable \
                  later.",
        phrases: &["http://", "https://"],
    },
];

/// Which category this prompt reads as, or nothing.
pub fn classify_prompt(prompt: &str) -> Option<&'static Cat> {
    if looks_like_correction(prompt) {
        return Some(&CATS[0]);
    }
    let padded = format!(" {} ", prompt.to_lowercase());
    CATS.iter().find(|c| c.phrases.iter().any(|p| padded.contains(*p)))
}

pub fn hint(_args: &[String]) -> crate::R<i32> {
    let Some(payload) = hook::read() else {
        return hook::passthrough();
    };
    let prompt = hook::field(&payload, "prompt");
    let session = hook::field(&payload, "session_id");
    // No session id means no dedupe; stay silent rather than hint on every prompt.
    if prompt.is_empty() || session.is_empty() {
        return hook::passthrough();
    }
    let Some(cat) = classify_prompt(prompt) else {
        return hook::passthrough();
    };
    let Ok(conn) = crate::db::open() else {
        return hook::passthrough();
    };
    if !claim(&conn, session, cat.name) {
        return hook::passthrough(); // already hinted this category here
    }
    hook::inject("UserPromptSubmit", cat.message)
}

/// First hint of this category in this session? An unwritable table means silence: a
/// hint repeated on every prompt is worse than a hint missed.
fn claim(conn: &Connection, session: &str, category: &str) -> bool {
    conn.execute(
        "INSERT OR IGNORE INTO hints(session, category) VALUES(?1, ?2)",
        rusqlite::params![session, category],
    )
    .is_ok_and(|changed| changed != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_correction_outranks_the_preference_wording_inside_it() {
        // Contains " never ", but it is first and foremost a correction.
        let c = classify_prompt("no, you should never do that again").unwrap();
        assert_eq!(c.name, "correction");
    }

    #[test]
    fn each_category_is_reachable_from_something_a_user_types() {
        for (prompt, want) in [
            ("from now on run the tests before you claim done", "preference"),
            ("we will go with the rust port", "decision"),
            ("the fix is to reserve a slot for the work lane", "method"),
            ("see https://example.com/docs for the api", "reference"),
        ] {
            assert_eq!(classify_prompt(prompt).map(|c| c.name), Some(want), "{prompt}");
        }
        assert!(classify_prompt("port recall.cpp to rust").is_none());
    }

    #[test]
    fn once_per_session_per_category() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::ensure_schema(&conn).unwrap();
        assert!(claim(&conn, "s1", "preference"));
        assert!(!claim(&conn, "s1", "preference"), "the second one is wallpaper");
        assert!(claim(&conn, "s1", "decision"), "a different category is not the same hint");
        assert!(claim(&conn, "s2", "preference"), "a different session has not seen it");
    }
}
