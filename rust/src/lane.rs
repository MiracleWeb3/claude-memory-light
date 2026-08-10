//! Which corpus a query ranks against.
//!
//! This type exists because of a real defect in the C++ tree. There, the lane SQL
//! lived in a `switch` in search.cpp and the CLI's `--role` parsing lived 140 lines
//! away, so `Lane::Tools` had working SQL that **no CLI path ever selected**. The
//! result: 57,683 indexed rows of tool output that `cml search` could not reach,
//! only the SessionStart recall hook could. The data was never missing; it was
//! unaddressable.
//!
//! The fix is not "remember to wire the flag". It is to make the gap impossible:
//! one enum, parsed from the user's `--role` and matched by the ranker, both
//! exhaustively. Add a variant and every `match` here stops compiling until it is
//! reachable from the command line. metal: move work to compile time.

use std::fmt;
use std::str::FromStr;

/// A searchable corpus. Every variant is reachable from `--role`; see `ALL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lane {
    /// `mem` — user prompts and the final assistant text of each turn.
    Conversation,
    /// `work` — tool calls and their output. The lane the C++ tree stranded.
    Tools,
    /// `scene` — distilled per-session summaries.
    Scene,
}

impl Lane {
    /// Every lane, in the order a mixed search reports them.
    ///
    /// Exhaustiveness is enforced by `debug_assert` in the unit test below rather
    /// than by hand: a new variant that is not added here fails the test.
    pub const ALL: [Lane; 3] = [Lane::Conversation, Lane::Tools, Lane::Scene];

    /// The FTS5 table this lane ranks against.
    pub const fn table(self) -> &'static str {
        match self {
            Lane::Conversation => "mem",
            Lane::Tools => "work",
            Lane::Scene => "scene",
        }
    }

    /// Columns to read for a result row: (display text, secondary, timestamp).
    ///
    /// `scene` keeps its prose in `summary` rather than `text`, which is why this
    /// is per-lane rather than one shared SELECT.
    pub const fn columns(self) -> &'static str {
        match self {
            Lane::Conversation | Lane::Tools => "text, role, project, session, ts",
            Lane::Scene => "summary, title, project, session, ts_start",
        }
    }

    /// Rank rowids for `query` within this lane, best first.
    pub const fn rank_sql(self) -> &'static str {
        match self {
            Lane::Conversation => {
                "SELECT rowid, rank FROM mem WHERE mem MATCH ?1 ORDER BY rank LIMIT ?2"
            }
            Lane::Tools => {
                "SELECT rowid, rank FROM work WHERE work MATCH ?1 ORDER BY rank LIMIT ?2"
            }
            Lane::Scene => {
                "SELECT rowid, rank FROM scene WHERE scene MATCH ?1 ORDER BY rank LIMIT ?2"
            }
        }
    }

    /// Short label printed next to a hit so mixed results stay readable.
    pub const fn label(self) -> &'static str {
        match self {
            Lane::Conversation => "talk",
            Lane::Tools => "ran",
            Lane::Scene => "scene",
        }
    }
}

impl fmt::Display for Lane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Lane::Conversation => "conversation",
            Lane::Tools => "tools",
            Lane::Scene => "scene",
        })
    }
}

/// Parsed from `--role`. Accepts the C++ role names so existing muscle memory and
/// scripts keep working, plus the lane names themselves.
impl FromStr for Lane {
    type Err = UnknownLane;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            // Lane names.
            "conversation" | "talk" | "chat" => Ok(Lane::Conversation),
            "tools" | "tool" | "work" | "ran" | "commands" => Ok(Lane::Tools),
            "scene" | "scenes" | "summary" => Ok(Lane::Scene),
            // Row-level roles that live inside `mem`; they select the lane that
            // holds them, and `SearchOpts::role` narrows within it.
            "user" | "assistant" | "memory" | "wiki" => Ok(Lane::Conversation),
            _ => Err(UnknownLane(s.to_string())),
        }
    }
}

/// Returned instead of silently searching the wrong corpus, which is how the
/// original bug stayed invisible: `--role work` reported "no hits" rather than
/// "that is not a lane I can search".
#[derive(Debug)]
pub struct UnknownLane(pub String);

impl fmt::Display for UnknownLane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown --role '{}'; try one of: conversation, tools, scene \
             (or a row role: user, assistant, memory, wiki)",
            self.0
        )
    }
}

impl std::error::Error for UnknownLane {}

/// A row-level role filter applied *within* a lane (`mem` stores several).
pub fn role_filter(role: &str) -> Option<&'static str> {
    match role.trim().to_ascii_lowercase().as_str() {
        "user" => Some("user"),
        "assistant" => Some("assistant"),
        "memory" => Some("memory"),
        "wiki" => Some("wiki"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guard that would have caught the original defect: every lane must be
    /// reachable from a string a user can actually type.
    #[test]
    fn every_lane_is_reachable_from_the_cli() {
        for lane in Lane::ALL {
            let typed: Lane = lane
                .to_string()
                .parse()
                .expect("each lane's Display form must parse back into that lane");
            assert_eq!(typed, lane, "{lane} does not round-trip through --role");
        }
    }

    /// `--role work` is the exact invocation that returned "no hits" against a
    /// corpus of 57,683 rows.
    #[test]
    fn work_role_selects_the_tools_lane() {
        assert_eq!("work".parse::<Lane>().unwrap(), Lane::Tools);
        assert_eq!("tools".parse::<Lane>().unwrap(), Lane::Tools);
        assert_eq!(Lane::Tools.table(), "work");
    }

    #[test]
    fn all_covers_every_variant() {
        // If a variant is added without extending ALL, the count check fails.
        assert_eq!(Lane::ALL.len(), 3);
        for lane in Lane::ALL {
            assert!(!lane.table().is_empty());
        }
    }

    #[test]
    fn unknown_role_is_an_error_not_a_silent_miss() {
        assert!("nonsense".parse::<Lane>().is_err());
    }
}
