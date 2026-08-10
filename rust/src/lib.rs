//! cml — full-history memory for Claude Code sessions.
//!
//! Rust successor to the 7,820-line C++ tree in `../cpp`. It opens the *same*
//! `index.db`, so the machine's existing history stays readable across the switch.
//!
//! What deliberately did not survive the port, because it only ever existed to
//! satisfy C++:
//!
//! * the hand-rolled SQLite RAII wrapper (`db.cpp`) — rusqlite is that wrapper;
//! * the hand-rolled JSON reader (`json.hpp`) — serde_json;
//! * `sqlite-vec` and its 324K of vendored C (`vec.cpp`, `vendor/sqlite-vec.c`) —
//!   a rayon cosine sweep over a `Vec<f32>` is smaller, faster at this scale, and
//!   removes the tree's only `unsafe`;
//! * the UTF-8 scanning in `unicode.hpp` — `str` is UTF-8 by construction.
//!
//! A port carries the system's observable behaviour, not the shape of the code
//! that produced it.

pub mod db;
pub mod lane;
pub mod paths;
/// Helpers shared by more than one command: noise classification, text squeezing,
/// project/path labels, timestamps, and the `distilled` key format.
///
/// Top-level rather than under `recall/` on purpose. These are used by search,
/// index, report and recall alike; hanging them off one command would make the
/// other three depend on it, which is the wrong direction for a leaf utility.
pub mod text;

pub mod distill;
pub mod encode;
pub mod index;
pub mod offload;
pub mod recall;
pub mod report;
pub mod search;
pub mod session;
pub mod vector;

/// The one error type crossing command boundaries.
///
/// No `anyhow`: every command's error path ends at the same place — a message on
/// stderr and a non-zero exit — so the extra crate would buy context strings this
/// binary never reads back.
pub type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// Every command has this shape. `main` does nothing but pick one.
pub type Command = fn(&[String]) -> R<i32>;

/// Resolve a subcommand name to its entry point.
///
/// A table rather than a chain of `if`s so that the set of commands is a value
/// the program can inspect — `help` prints this, instead of a second hand-written
/// list that drifts out of sync with the first.
pub fn command(name: &str) -> Option<(Command, &'static str)> {
    Some(match name {
        "index" => (index::run as Command, "scan transcripts and notes into the index"),
        "search" => (search::run as Command, "search every lane: talk, tools, scenes"),
        "recall" => (recall::run as Command, "hook: inject relevant memory at SessionStart"),
        "capture" => (session::capture as Command, "hook: record this turn's signal"),
        "nudge" => (session::nudge as Command, "hook: consolidation reminder"),
        "hint" => (session::hint as Command, "hook: per-session hint state"),
        "state" => (session::state as Command, "standing context for a project"),
        "consolidate" => (session::consolidate as Command, "promote inbox signals into memory"),
        "distill" => (distill::run as Command, "curate rows into durable gists"),
        "embed" => (encode::run as Command, "compute embeddings for unembedded rows"),
        "forget" => (report::forget as Command, "blocklist rows by id or match"),
        "stats" => (report::stats as Command, "row counts and index size"),
        "doctor" => (report::doctor as Command, "check the install end to end"),
        "loops" => (report::loops as Command, "asks that keep coming back unresolved"),
        "eval" => (report::eval as Command, "measure recall quality against known hits"),
        "offload" => (offload::run as Command, "spill oversized tool output to a file"),
        _ => return None,
    })
}

/// Commands in the order `help` lists them.
pub const COMMANDS: [&str; 16] = [
    "index", "search", "recall", "capture", "nudge", "hint", "state", "consolidate",
    "distill", "embed", "forget", "stats", "doctor", "loops", "eval", "offload",
];

#[cfg(test)]
mod tests {
    /// The registry and the printed list must not drift apart — that drift is the
    /// same class of bug as the stranded search lane.
    #[test]
    fn every_listed_command_resolves() {
        for name in super::COMMANDS {
            assert!(super::command(name).is_some(), "`{name}` is listed but does not resolve");
        }
    }
}
