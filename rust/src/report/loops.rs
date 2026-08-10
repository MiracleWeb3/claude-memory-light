//! `cml loops` — the asks that keep coming back unresolved.
//!
//! The signal a hand-kept notes system computes by diffing pages falls out of
//! the index for free: every ask ever made is already a row. Two asks are "the
//! same question" when their word sets mostly agree, and a question is chronic
//! when the same one shows up in more than one session.

use std::collections::HashSet;

use rusqlite::Connection;

use super::{num_arg, text};
use crate::text::{is_noise, squeeze};

/// One user ask, as the grouper sees it.
struct Ask {
    text: String,
    session: String,
    ts: String,
}

struct Group {
    tokens: HashSet<String>, // the founding ask's words
    sessions: HashSet<String>,
    first_text: String,
    first_ts: String,
}

/// Lowercased word set — the unit recurrence is judged on. Words of one
/// character carry no topic.
fn tokens_of(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 2)
        .map(str::to_lowercase)
        .collect()
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let shared = a.intersection(b).count();
    shared as f64 / (a.len() + b.len() - shared) as f64
}

/// Formatted lines, most recurrent first, for groups seen in at least
/// `min_sessions` distinct sessions.
///
/// ponytail: O(n²) greedy first-fit against each group's founding ask — fine for
/// a month of user rows; revisit with shingle hashing past ~10k asks.
fn recurring(asks: &[Ask], min_sessions: usize, limit: usize) -> Vec<String> {
    let mut groups: Vec<Group> = Vec::new();
    for ask in asks {
        let tokens = tokens_of(&ask.text);
        if tokens.len() < 5 {
            continue; // "fix it" is not a loop, it's a Tuesday
        }
        let at = match groups.iter().position(|g| jaccard(&tokens, &g.tokens) >= 0.6) {
            Some(at) => at,
            None => {
                groups.push(Group {
                    tokens,
                    sessions: HashSet::new(),
                    first_text: ask.text.clone(),
                    first_ts: ask.ts.clone(),
                });
                groups.len() - 1
            }
        };
        let group = &mut groups[at];
        group.sessions.insert(ask.session.clone());
        // Rows arrive in timestamp order, so this only matters for rows whose
        // transcript carried no timestamp at all.
        if group.first_ts.is_empty() || (!ask.ts.is_empty() && ask.ts < group.first_ts) {
            group.first_ts.clone_from(&ask.ts);
            group.first_text.clone_from(&ask.text);
        }
    }

    groups.sort_by(|a, b| {
        b.sessions.len().cmp(&a.sessions.len()).then_with(|| a.first_ts.cmp(&b.first_ts))
    });
    groups
        .iter()
        .filter(|g| g.sessions.len() >= min_sessions)
        .take(limit)
        .map(|g| {
            let day: String = g.first_ts.chars().take(10).collect();
            format!(
                "carried across {} sessions since {day}: {}",
                g.sessions.len(),
                squeeze(&g.first_text, 110)
            )
        })
        .collect()
}

/// The window, straight out of the index.
///
/// SQLite does the calendar arithmetic — `date('now','-30 days')` against a ts
/// column that is already ISO 8601 text. The C++ tree carried its own
/// seconds-to-civil-date conversion to build this one string.
///
/// One statement rather than two: a NULL `?2` matches every project, so the
/// scoped and unscoped windows cannot drift apart.
fn window(conn: &Connection, days: i64, project: Option<&str>) -> rusqlite::Result<Vec<Ask>> {
    let mut st = conn.prepare(
        "SELECT text, session, ts FROM mem \
         WHERE role='user' AND ts >= date('now', ?1) \
         AND (?2 IS NULL OR project = ?2) ORDER BY ts",
    )?;
    let rows = st.query_map(rusqlite::params![format!("-{} days", days.max(0)), project], |r| {
        Ok(Ask { text: text(r, 0)?, session: text(r, 1)?, ts: text(r, 2)? })
    })?;
    Ok(rows
        .flatten()
        // The index has been rebuilt by several generations of the noise filter,
        // so the gate runs again at read time: rows indexed before the
        // `<command-message>` fix are still in there wearing a user role.
        .filter(|a| !is_noise(&a.text) && !a.text.starts_with("# "))
        .collect())
}

/// The chronic-loop lines for one window: `cml loops` and the recall hook's
/// briefing share this, so the two can never disagree about what is chronic.
///
/// `project` scopes the window — a chronic ask from a different repo is not this
/// project's open thread.
pub fn loop_lines(
    conn: &Connection,
    days: i64,
    limit: usize,
    project: Option<&str>,
) -> crate::R<Vec<String>> {
    Ok(recurring(&window(conn, days, project)?, 2, limit))
}

pub fn loops(args: &[String]) -> crate::R<i32> {
    let days = i64::try_from(num_arg(args, "--days").unwrap_or(30)).unwrap_or(30);
    let limit = num_arg(args, "--limit").unwrap_or(10);

    let conn = crate::db::open()?;
    let lines = loop_lines(&conn, days, limit, None)?;
    if lines.is_empty() {
        println!("no chronic loops in the last {days} days");
        return Ok(0);
    }
    for line in lines {
        println!("{line}");
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(text: &str, session: &str, ts: &str) -> Ask {
        Ask { text: text.into(), session: session.into(), ts: ts.into() }
    }

    #[test]
    fn one_question_asked_in_two_sessions_is_a_loop() {
        let asks = vec![
            ask("why does the recall hook never fire on this machine", "s1", "2026-07-01T09:00"),
            // Reworded, same words: same loop.
            ask("why does the recall hook never fire here on this box", "s2", "2026-07-20T09:00"),
            // Same session twice does not make it chronic.
            ask("please rebuild the index from scratch tonight", "s3", "2026-07-02T09:00"),
            ask("please rebuild the index from scratch tonight", "s3", "2026-07-03T09:00"),
            // Too few words to be a topic.
            ask("fix it now", "s4", "2026-07-04T09:00"),
            ask("fix it now", "s5", "2026-07-05T09:00"),
        ];
        let lines = recurring(&asks, 2, 10);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].starts_with("carried across 2 sessions since 2026-07-01: why does"));
    }

    #[test]
    fn unrelated_asks_do_not_merge() {
        let asks = vec![
            ask("index the subagent transcripts as well please", "s1", "2026-07-01"),
            ask("the map should open in brave and not chrome", "s2", "2026-07-02"),
        ];
        assert!(recurring(&asks, 2, 10).is_empty());
    }

    #[test]
    fn jaccard_is_symmetric_and_bounded() {
        let a = tokens_of("alpha beta gamma");
        let b = tokens_of("alpha beta delta");
        assert!((jaccard(&a, &b) - jaccard(&b, &a)).abs() < f64::EPSILON);
        assert!((jaccard(&a, &a) - 1.0).abs() < f64::EPSILON);
        assert_eq!(jaccard(&a, &HashSet::new()), 0.0);
    }
}
