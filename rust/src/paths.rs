//! Where a session's files live, and how its timestamps are written.
//!
//! Both halves must match the C++ tree byte for byte: the two binaries append to the
//! same inbox files and write the same `index.db`, so a divergence here silently splits
//! the memory in two. `crate::db` already owns the data directory itself; this is only
//! the naming built on top of it.

use std::path::PathBuf;

/// `$HOME`, or `/` when the environment has none (a hook can be spawned that bare).
pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// `/home/user` -> `-home-user`: Claude Code's own project-directory flattening.
pub fn flatten(path: &str) -> String {
    path.chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

/// A flattened project dir name -> the short label used for inbox filenames and for
/// the `project` column of every indexed row.
pub fn project_label(dirname: &str) -> String {
    let fh = flatten(&home_dir().to_string_lossy());
    let label = if dirname == fh {
        "home".to_string()
    } else if let Some(rest) = dirname.strip_prefix(&fh).filter(|r| r.len() > 1 && r.starts_with('-'))
    {
        rest[1..].to_string()
    } else {
        dirname.trim_start_matches('-').to_string()
    };
    if label.is_empty() {
        "misc".to_string()
    } else {
        label
    }
}

/// The project label for a working directory, which is what every hook actually has.
pub fn label_for_cwd(cwd: &str) -> String {
    project_label(&flatten(cwd))
}

pub fn inbox_dir() -> PathBuf {
    crate::db::home().join("inbox")
}

pub fn inbox_path_for(cwd: &str) -> PathBuf {
    inbox_dir().join(format!("{}.md", label_for_cwd(cwd)))
}

pub fn wiki_dir() -> PathBuf {
    crate::db::home().join("wiki")
}

/// Every curated memory directory: `~/.claude/projects/*/memory`. The user's own is
/// `projects/-/memory`, but a per-repo one is equally valid and equally indexed.
pub fn memory_dirs() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(crate::db::transcripts_dir()) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path().join("memory"))
        .filter(|p| p.is_dir())
        .collect();
    out.sort();
    out
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Unix seconds -> `YYYY-MM-DD HH:MM` (UTC).
///
/// No `chrono`: one calendar conversion does not justify a dependency, and this is the
/// standard days-to-civil algorithm — 8 lines, no table, correct for every proleptic
/// Gregorian date rather than only for the ones near today.
pub fn iso_minute(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", rem / 3600, (rem % 3600) / 60)
}

/// Unix seconds -> `YYYY-MM-DD` (UTC).
pub fn iso_date(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattening_matches_claude_codes_own() {
        assert_eq!(flatten("/home/u/dev/x.y"), "-home-u-dev-x-y");
        assert_eq!(flatten(""), "");
    }

    #[test]
    fn a_project_label_is_the_path_below_home() {
        let fh = flatten(&home_dir().to_string_lossy());
        assert_eq!(project_label(&fh), "home");
        assert_eq!(project_label(&format!("{fh}-dev-cml")), "dev-cml");
        // Outside home the leading dashes are dropped, and nothing is still something.
        assert_eq!(project_label("-etc-nginx"), "etc-nginx");
        assert_eq!(project_label("---"), "misc");
        assert_eq!(project_label(""), "misc");
    }

    #[test]
    fn timestamps_are_utc_and_zero_padded() {
        assert_eq!(iso_minute(0), "1970-01-01 00:00");
        assert_eq!(iso_minute(1_754_000_000), "2025-07-31 22:13");
        assert_eq!(iso_date(1_754_000_000), "2025-07-31");
        // A leap day, and the century rule around it.
        assert_eq!(iso_date(951_782_400), "2000-02-29");
        // Before the epoch the floor division must not round toward zero.
        assert_eq!(iso_minute(-1), "1969-12-31 23:59");
    }
}
