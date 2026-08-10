//! Markdown notes -> one row each.
//!
//! Memory files and wiki pages are indexed whole: they are already the distilled
//! thing, so there is none of the per-entry judgement transcripts need. The `ts` is
//! the file's mtime, because a curated note has no timestamp of its own.

use std::path::Path;

use rusqlite::Connection;

use super::files::{self, Counts, Known};

pub fn index_dir(
    conn: &mut Connection,
    known: &Known,
    dir: &Path,
    role: &str,
    project: &str,
    force: bool,
) -> crate::R<Counts> {
    let mut total = Counts::default();
    // Most projects have no memory directory at all; that is not an error.
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Ok(total);
    };

    let tx = conn.transaction()?;
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("md") {
            continue;
        }
        let Some(meta) = files::meta_of(&path) else {
            continue;
        };
        let file = path.to_string_lossy().into_owned();
        if !force && !files::is_stale(known, &file, &meta) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };

        files::drop_rows_for_file(&tx, &file)?;
        if !text.trim().is_empty() {
            let session = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            // ponytail: no `asks` handling here. Measured on the live index: 288
            // memory rows and 3 wiki rows, none of them curated — distill judges
            // assistant and user rubrics only, so a note has nothing to preserve.
            tx.execute(
                "INSERT INTO mem(text, role, project, session, ts, file) VALUES(?1,?2,?3,?4,?5,?6)",
                (&text, role, project, &session, crate::paths::iso_date(meta.mtime), &file),
            )?;
            total.rows += 1;
        }
        files::upsert_file(&tx, &file, &meta)?;
        total.files += 1;
    }
    tx.commit()?;
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[test]
    fn a_note_is_one_row_and_stays_one_row() {
        let dir = std::env::temp_dir().join(format!("cml-notes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let notes = dir.join("memory");
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::write(notes.join("wifi-mtu.md"), "# DAITA\npath MTU 1280 kills it\n").unwrap();
        std::fs::write(notes.join("empty.md"), "   \n").unwrap();
        let mut conn = db::open_at(&dir.join("index.db")).unwrap();

        let known = files::known(&conn);
        let c = index_dir(&mut conn, &known, &notes, "memory", "home", false).unwrap();
        assert_eq!((c.files, c.rows), (2, 1), "a blank note is a file, not a row");

        let (role, session): (String, String) = conn
            .query_row("SELECT role, session FROM mem", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((role.as_str(), session.as_str()), ("memory", "wifi-mtu"));

        let known = files::known(&conn);
        let again = index_dir(&mut conn, &known, &notes, "memory", "home", false).unwrap();
        assert_eq!(again.files, 0, "unchanged notes must be skipped");
        assert_eq!(db::count(&conn, "mem"), 1);

        // A directory that does not exist is empty, never an error.
        let known = files::known(&conn);
        let missing = index_dir(&mut conn, &known, &dir.join("nope"), "wiki", "wiki", false).unwrap();
        assert_eq!(missing.files, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
