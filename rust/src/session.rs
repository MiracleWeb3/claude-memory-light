//! Per-session commands: what gets recorded around a conversation, and what gets handed
//! back at the start of the next one.
//!
//! Four of these are hooks — `capture` at Stop, `nudge` at SessionStart, `hint` at
//! UserPromptSubmit — and they share one law: a memory hook must never block or break
//! the session. Bad stdin, a missing index, an unreadable inbox: every path exits 0 and
//! says nothing. The two commands a human types, `state` and `consolidate`, may report
//! an error, because there is somebody there to read it.
//!
//! ## Why `nudge` carries the signals instead of a count
//!
//! The C++ nudge printed a number and a file path and asked the assistant to go read the
//! inbox. That is pull-based, and it did not work: signals sat unconsolidated on this
//! machine across dozens of sessions. Nothing was broken — the reminder fired every
//! time. It was simply possible to notice the line and move on, so that is what
//! happened.
//!
//! A capability that depends on someone choosing to invoke it is, in practice, a
//! capability that does not run. So the signals now arrive *inside* the nudge, grouped
//! and with their targets resolved. Not reading them is still possible; not seeing them
//! is not.
//!
//! ## What is deliberately still manual
//!
//! `consolidate` writes no memory files. It emits a report and, with `--clear`, retires
//! the lines it reported on. Auto-writing memory from unreviewed signals is the
//! documented way this class of system poisons itself, one plausible wrong line at a
//! time, with no moment at which anybody looked. Being push-based about *surfacing* does
//! not require being reckless about *writing*.

pub mod capture;
pub mod consolidate;
pub mod hint;
pub mod nudge;
pub mod state;

pub use self::capture::capture;
pub use self::consolidate::consolidate;
pub use self::hint::hint;
pub use self::nudge::nudge;
pub use self::state::state;
