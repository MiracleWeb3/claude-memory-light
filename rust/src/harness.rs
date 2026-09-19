//! Where sessions come from, once "a session" stops meaning "a Claude Code session".
//!
//! Every other agent harness writes the same three facts — who spoke, what they
//! said, when — behind a different filename and a different key. This module is
//! the only place that difference is allowed to exist: each adapter normalises its
//! store into [`Session`] + [`Turn`], and the indexer above it sees one shape.
//!
//! # Why not a trait
//!
//! An enum. The set of harnesses is closed at compile time (a user cannot install
//! a new one into a stripped binary), the dispatch is once per directory rather
//! than once per row, and `detect_all` wants to *return the list*, which a trait
//! object would only make heavier. `metal` rule: reach for the smallest construct
//! that holds the requirement.
//!
//! # What an adapter must not do
//!
//! Adapters read. They never write to a harness's own store: opening opencode's
//! live SQLite read-write would take a lock the editor is holding, and a memory
//! tool that freezes the editor it remembers for is worse than no memory tool.

pub mod codex;
pub mod jcode;
pub mod opencode;

use std::path::PathBuf;

/// One agent harness cml can read history out of.
///
/// `Claude` is not in this enum. Its transcripts flow through the original
/// `index::transcripts` path unchanged — the whole point of the port was that the
/// existing 208 MB index stays byte-compatible, and routing it through a second
/// parser would risk exactly the divergence this crate spent a rewrite avoiding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Harness {
    Jcode,
    Opencode,
    Codex,
}

/// The value stored in the `origin` table's `harness` column, and what
/// `--harness` accepts on the command line.
impl Harness {
    pub const ALL: [Self; 3] = [Self::Jcode, Self::Opencode, Self::Codex];

    pub fn id(self) -> &'static str {
        match self {
            Self::Jcode => "jcode",
            Self::Opencode => "opencode",
            Self::Codex => "codex",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|h| h.id() == s)
    }

    /// Where this harness keeps history, honouring its own env override first.
    ///
    /// Returns the path whether or not it exists; [`Self::detect`] is what asks.
    pub fn root(self) -> PathBuf {
        match self {
            Self::Jcode => jcode::root(),
            Self::Opencode => opencode::root(),
            Self::Codex => codex::root(),
        }
    }

    /// Is this harness actually installed on this machine?
    pub fn detect(self) -> bool {
        let root = self.root();
        match self {
            // A store is only evidence if it holds something. An empty ~/.codex
            // is a directory some other tool made, and claiming to support a
            // harness on that basis is how an install matrix starts lying.
            Self::Opencode => root.is_file(),
            _ => root.is_dir() && std::fs::read_dir(&root).is_ok_and(|mut d| d.next().is_some()),
        }
    }

    /// Read every session this harness holds.
    pub fn sessions(self) -> Vec<Session> {
        match self {
            Self::Jcode => jcode::sessions(),
            Self::Opencode => opencode::sessions(),
            Self::Codex => codex::sessions(),
        }
    }
}

/// Every harness installed here, in a stable order.
pub fn detected() -> Vec<Harness> {
    Harness::ALL.into_iter().filter(|h| h.detect()).collect()
}

/// Who spoke. Deliberately the same four values `index::transcripts` writes into
/// the `role` column, so a search filter cannot tell a Claude row from a jcode one
/// by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

/// One message, normalised.
#[derive(Debug, Clone)]
pub struct Turn {
    pub role: Role,
    pub text: String,
    /// `YYYY-MM-DD HH:MM`, or empty when the source carries no usable clock.
    pub ts: String,
}

/// One conversation, normalised.
#[derive(Debug, Clone)]
pub struct Session {
    pub harness: Harness,
    pub id: String,
    /// The working directory the session ran in, which is what becomes the
    /// `project` label. Empty when the store does not record one.
    pub cwd: String,
    /// The file this came out of, for the `files` staleness table. For a SQLite
    /// store this is the database path with the session id appended, so one
    /// session's rows can still be dropped and rewritten independently.
    pub file: String,
    /// Size and mtime of the backing file at read time.
    pub size: i64,
    pub mtime: i64,
    pub turns: Vec<Turn>,
}

impl Session {
    /// The `project` column value: the harness's cwd run through the same
    /// flattening Claude Code applies to its own project directories, so
    /// `--project parserx` matches rows from every harness at once.
    pub fn project(&self) -> String {
        if self.cwd.is_empty() {
            return "misc".to_string();
        }
        crate::paths::label_for_cwd(&self.cwd)
    }
}

/// Milliseconds since the epoch -> the timestamp string the index already stores.
///
/// Not `iso_minute`. The `ts` column of a transcript row holds Claude Code's own
/// `2026-07-10T16:56:57.496Z`, and `recall` sorts and groups on that string —
/// so a harness row written as `2026-07-10 16:56` would sort *before* every
/// Claude row of the same day (space `0x20` < `T` `0x54`) and quietly lose every
/// recency tie. One format in the column, and this is the one already in it.
pub fn ts_from_millis(ms: i64) -> String {
    ts_from_secs(ms.div_euclid(1000))
}

/// Seconds since the epoch -> the same ISO-8601 instant.
pub fn ts_from_secs(secs: i64) -> String {
    let rem = secs.rem_euclid(86_400);
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        crate::paths::iso_date(secs),
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_and_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for h in Harness::ALL {
            assert_eq!(Harness::parse(h.id()), Some(h));
            assert!(seen.insert(h.id()), "duplicate harness id {}", h.id());
        }
        assert_eq!(Harness::parse("claude"), None, "claude is not an adapter");
        assert_eq!(Harness::parse(""), None);
    }

    #[test]
    fn millis_become_the_timestamp_format_already_in_the_column() {
        assert_eq!(ts_from_millis(1_754_000_000_000), "2025-07-31T22:13:20Z");
        // Sub-second and pre-epoch values must not round toward zero.
        assert_eq!(ts_from_millis(999), "1970-01-01T00:00:00Z");
        assert_eq!(ts_from_millis(-1), "1969-12-31T23:59:59Z");
    }

    /// The whole reason this is ISO rather than `iso_minute`: harness rows and
    /// Claude rows share one column, and `recall` orders on it as a string.
    #[test]
    fn a_harness_row_sorts_against_a_real_claude_timestamp() {
        // A real row out of the 208 MB index on the machine this was written on.
        let claude = "2026-07-10T16:56:57.496Z";
        // 2026-07-10T17:00:00Z and 2026-07-10T16:00:00Z.
        assert!(ts_from_secs(1_783_702_800).as_str() > claude, "later must sort later");
        assert!(ts_from_secs(1_783_699_200).as_str() < claude, "earlier must sort earlier");
    }

    #[test]
    fn a_session_without_a_cwd_still_has_a_project() {
        let s = Session {
            harness: Harness::Jcode,
            id: "s".into(),
            cwd: String::new(),
            file: "f".into(),
            size: 0,
            mtime: 0,
            turns: Vec::new(),
        };
        assert_eq!(s.project(), "misc");
    }
}
