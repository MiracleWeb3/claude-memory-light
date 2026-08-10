//! Semantic search: brute force, and that is the whole design.
//!
//! The C++ tree reached this through sqlite-vec — 324K of vendored C, a `vec0`
//! virtual table, and an `sqlite3_auto_extension` call that was the only `unsafe`
//! in the binary. What it bought was an ANN index over 3,827 vectors.
//!
//! A cosine sweep over the largest lane here is 57,683 x 256 floats: 15 million
//! multiply-adds, spread across cores by rayon, behind a SQLite read that costs
//! more than the arithmetic does. There is no accuracy loss because there is no
//! approximation, no index to rebuild, and no extension to load. The dependency,
//! its vendored source, and the `unsafe` all leave together.
//!
//! `search` never returns `Err`. A missing model, an absent `emb` table, or vectors
//! of the wrong width all degrade to an empty result, because the caller's fallback
//! is keyword search — turning a cold cache into a failed command would be worse
//! than the thing it is protecting against.

use std::sync::OnceLock;

use rayon::prelude::*;
use rusqlite::{params, Connection};

use crate::encode::model::Model;
use crate::lane::Lane;
use crate::R;

/// Vectors decoded at once. 4096 x 256 floats is ~4 MB in flight, so the largest
/// lane is swept in bounded memory rather than 59 MB of blobs at once.
const CHUNK: usize = 4096;

/// The model, loaded at most once per process.
///
/// Not a micro-optimisation: a unified search calls this once per lane, and the
/// load is a 30 MB read plus a 29,528-entry vocabulary parse. Doing it three times
/// for one query is the kind of cost that gets blamed on "semantic search is slow".
/// A failed load is cached as `None` — it will not start working mid-process.
fn model() -> Option<&'static Model> {
    static MODEL: OnceLock<Option<Model>> = OnceLock::new();
    MODEL.get_or_init(|| Model::load().ok()).as_ref()
}

/// Little-endian f32 array — the format `emb.v` stores.
pub fn to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// The inverse. A trailing partial float is ignored rather than fatal: a truncated
/// blob is a corrupt row, and one bad row must not take down a search.
pub fn from_blob(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Cosine similarity, 0.0 for a zero vector or a length mismatch.
///
/// The zero case is real, not defensive: a row whose every token is [UNK] pools to
/// nothing, and `NaN` from `0/0` would sort ahead of every genuine hit.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom <= 0.0 {
        0.0
    } else {
        dot / denom
    }
}

/// Rows of `lane` ranked by similarity to `query`, best first.
///
/// Always `Ok`; see the module note. The `R` return exists so the signature matches
/// every other lookup in the tree.
pub fn search(conn: &Connection, query: &str, lane: Lane, limit: usize) -> R<Vec<(i64, f32)>> {
    if limit == 0 || query.trim().is_empty() {
        return Ok(Vec::new());
    }
    // No model on this machine: keyword-only search, not a broken command.
    let Some(model) = model() else {
        return Ok(Vec::new());
    };
    let q = model.encode(query);

    // The `emb` table is absent on a read-only connection opened before the schema
    // was ever created.
    let mut stmt = match conn.prepare("SELECT rowid_ref, dim, v FROM emb WHERE lane = ?1") {
        Ok(s) => s,
        Err(_) => return Ok(Vec::new()),
    };
    let mut rows = match stmt.query(params![lane.table()]) {
        Ok(r) => r,
        Err(_) => return Ok(Vec::new()),
    };

    let mut scored: Vec<(i64, f32)> = Vec::new();
    let mut chunk: Vec<(i64, Vec<u8>)> = Vec::with_capacity(CHUNK);
    while let Ok(Some(row)) = rows.next() {
        // Vectors built by a different model are skipped, not compared. The C++
        // parsed this width back out of the `CREATE VIRTUAL TABLE` SQL text; here
        // it is a column.
        let (Ok(dim), Ok(rowid), Ok(blob)) =
            (row.get::<_, i64>(1), row.get::<_, i64>(0), row.get::<_, Vec<u8>>(2))
        else {
            continue;
        };
        if dim as usize != q.len() {
            continue;
        }
        chunk.push((rowid, blob));
        if chunk.len() == CHUNK {
            score(&q, &mut chunk, &mut scored);
        }
    }
    score(&q, &mut chunk, &mut scored);

    scored.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(limit);
    Ok(scored)
}

/// Decode and score one chunk in parallel, draining it for the next.
fn score(q: &[f32], chunk: &mut Vec<(i64, Vec<u8>)>, out: &mut Vec<(i64, f32)>) {
    out.par_extend(
        chunk
            .par_drain(..)
            .map(|(rowid, blob)| (rowid, cosine(q, &from_blob(&blob)))),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_vectors_are_one_and_orthogonal_are_zero() {
        let a = vec![1.0, 2.0, 3.0, 4.0];
        assert!((cosine(&a, &a) - 1.0).abs() < 1e-6, "identical must be 1.0");

        let x = vec![1.0, 0.0, 0.0];
        let y = vec![0.0, 1.0, 0.0];
        assert!(cosine(&x, &y).abs() < 1e-6, "orthogonal must be 0.0");

        // Scale-invariant, which is the point of cosine over dot.
        let scaled: Vec<f32> = a.iter().map(|v| v * 7.5).collect();
        assert!((cosine(&a, &scaled) - 1.0).abs() < 1e-6);

        // Opposite direction is -1, so it sorts last rather than looking neutral.
        let flipped: Vec<f32> = a.iter().map(|v| -v).collect();
        assert!((cosine(&a, &flipped) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn degenerate_inputs_score_zero_instead_of_nan() {
        let zero = vec![0.0f32; 4];
        let a = vec![1.0, 2.0, 3.0, 4.0];
        assert_eq!(cosine(&zero, &a), 0.0, "a zero vector must not produce NaN");
        assert_eq!(cosine(&zero, &zero), 0.0);
        assert_eq!(cosine(&a, &[1.0, 2.0]), 0.0, "mismatched widths must not compare");
    }

    #[test]
    fn blobs_round_trip_through_sqlite_storage() {
        let v: Vec<f32> = vec![0.0, -1.5, 3.25, 1e-8, -0.0, f32::MIN_POSITIVE, 12345.678];
        let bytes = to_blob(&v);
        assert_eq!(bytes.len(), v.len() * 4, "one f32 is four little-endian bytes");
        assert_eq!(from_blob(&bytes), v, "round trip must be exact, not approximate");

        // Little-endian by spec, so the encoding is checked, not assumed: 1.0f32 is
        // 0x3F800000.
        assert_eq!(to_blob(&[1.0]), vec![0x00, 0x00, 0x80, 0x3F]);

        // A truncated blob loses the partial float and keeps the rest.
        let mut torn = to_blob(&v);
        torn.truncate(torn.len() - 2);
        assert_eq!(from_blob(&torn).len(), v.len() - 1);
        assert!(from_blob(&[]).is_empty());
    }

    #[test]
    fn an_index_with_no_vectors_returns_no_hits_rather_than_an_error() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::ensure_schema(&conn).unwrap();
        let hits = search(&conn, "anything", Lane::Conversation, 5).expect("must not error");
        assert!(hits.is_empty());
        // An empty query short-circuits before the model is ever looked for.
        assert!(search(&conn, "   ", Lane::Tools, 5).unwrap().is_empty());
        assert!(search(&conn, "x", Lane::Tools, 0).unwrap().is_empty());
    }
}
