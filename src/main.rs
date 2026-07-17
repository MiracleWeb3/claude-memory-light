// claude-memory-light (cml): zero-token, daemon-free memory for Claude Code.
//
// Four layers, one binary, no LLM calls, no background processes:
//   verbatim  — every session transcript, FTS5-indexed (`index`, `search`)
//   distilled — Hermes-style learning capture + consolidation nudge (`capture`, `nudge`)
//   curated   — auto-memory *.md files and the wiki vault, same index (`index`)
//   structural— pairs with graphify if installed (`doctor` reports it)
//
// Design law (learned from claude-mem's issue tracker): a memory hook must NEVER
// block or break the session. Every hook-facing subcommand exits 0 no matter what.

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use zerocopy::AsBytes;
use std::env;
use std::error::Error;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

type R<T> = Result<T, Box<dyn Error>>;

fn home() -> PathBuf {
    PathBuf::from(env::var("HOME").expect("HOME not set"))
}

fn data_dir() -> PathBuf {
    match env::var("CML_HOME") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => home().join(".claude/claude-memory-light"),
    }
}

/// "/home/user" -> "-home-user" (Claude Code's project-dir flattening).
fn flatten(path: &str) -> String {
    path.replace(['/', '.'], "-")
}

fn flat_home() -> String {
    flatten(&home().to_string_lossy())
}

/// "-home-user-dev-foo" -> "dev-foo"; "-home-user" -> "home".
fn project_label(dirname: &str) -> String {
    let fh = flat_home();
    let label = if dirname == fh {
        "home".to_string()
    } else if let Some(rest) = dirname.strip_prefix(&format!("{fh}-")) {
        rest.to_string()
    } else {
        dirname.trim_start_matches('-').to_string()
    };
    if label.is_empty() {
        "misc".to_string()
    } else {
        label
    }
}

fn open_db() -> R<Connection> {
    // register sqlite-vec once, before any connection opens
    static VEC_INIT: std::sync::Once = std::sync::Once::new();
    VEC_INIT.call_once(|| unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    });
    let dir = data_dir();
    fs::create_dir_all(&dir)?;
    fs::create_dir_all(dir.join("wiki"))?;
    fs::create_dir_all(dir.join("inbox"))?;
    let conn = Connection::open(dir.join("index.db"))?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         CREATE TABLE IF NOT EXISTS files(path TEXT PRIMARY KEY, size INTEGER, mtime INTEGER);
         CREATE VIRTUAL TABLE IF NOT EXISTS mem USING fts5(
             text, role UNINDEXED, project UNINDEXED, session UNINDEXED,
             ts UNINDEXED, file UNINDEXED, tokenize='porter unicode61');",
    )?;
    Ok(conn)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Unix seconds -> (y, m, d, hh, mm). Howard Hinnant's civil-date algorithm; no chrono dep.
fn civil(secs: i64) -> (i64, u32, u32, u32, u32) {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, (rem / 3600) as u32, (rem % 3600 / 60) as u32)
}

fn iso_date(secs: i64) -> String {
    let (y, m, d, _, _) = civil(secs);
    format!("{y:04}-{m:02}-{d:02}")
}

fn iso_minute(secs: i64) -> String {
    let (y, m, d, hh, mm) = civil(secs);
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}")
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter(|i| i["type"] == "text")
            .filter_map(|i| i["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn is_noise(text: &str) -> bool {
    let t = text.trim_start();
    t.is_empty()
        || t.starts_with("Caveat:")
        || t.starts_with("<command-name>")
        || t.starts_with("<local-command")
        || t.starts_with("<system-reminder>")
}

// ---------- index ----------

struct FileMeta {
    path: PathBuf,
    size: i64,
    mtime: i64,
}

fn meta_of(path: &Path) -> Option<FileMeta> {
    let m = fs::metadata(path).ok()?;
    if !m.is_file() {
        return None;
    }
    let mtime = m
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Some(FileMeta {
        path: path.to_path_buf(),
        size: m.len() as i64,
        mtime,
    })
}

fn unchanged(conn: &Connection, fm: &FileMeta) -> bool {
    conn.query_row(
        "SELECT size, mtime FROM files WHERE path=?1",
        params![fm.path.to_string_lossy()],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
    )
    .map(|(s, t)| s == fm.size && t == fm.mtime)
    .unwrap_or(false)
}

fn index_transcripts(conn: &mut Connection, force: bool) -> R<(usize, usize)> {
    let projects = home().join(".claude/projects");
    let (mut nf, mut nr) = (0usize, 0usize);
    let entries = match fs::read_dir(&projects) {
        Ok(e) => e,
        Err(_) => return Ok((0, 0)), // no transcripts yet — nothing to do
    };
    for pdir in entries.flatten() {
        if !pdir.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let project = project_label(&pdir.file_name().to_string_lossy());
        // ponytail: top-level session files only; subagent transcripts skipped — add if recall ever needs them
        for f in fs::read_dir(pdir.path())?.flatten() {
            let path = f.path();
            if path.extension().map(|e| e != "jsonl").unwrap_or(true) {
                continue;
            }
            let Some(fm) = meta_of(&path) else { continue };
            if !force && unchanged(conn, &fm) {
                continue;
            }
            let pstr = fm.path.to_string_lossy().to_string();
            let session_fallback = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let tx = conn.transaction()?;
            let _ = tx.execute(
                "DELETE FROM vec_mem WHERE rowid IN (SELECT rowid FROM mem WHERE file=?1)",
                params![pstr],
            );
            tx.execute("DELETE FROM mem WHERE file=?1", params![pstr])?;
            let reader = BufReader::new(fs::File::open(&path)?);
            let mut n = 0usize;
            {
                let mut ins = tx.prepare(
                    "INSERT INTO mem(text, role, project, session, ts, file) VALUES(?1,?2,?3,?4,?5,?6)",
                )?;
                for line in reader.lines() {
                    let Ok(line) = line else { continue };
                    let Ok(v) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    let (role, text, ts) = match v["type"].as_str() {
                        Some("summary") => (
                            "summary".to_string(),
                            v["summary"].as_str().unwrap_or("").to_string(),
                            String::new(),
                        ),
                        Some(t) if t == "user" || t == "assistant" => {
                            if v["isSidechain"] == true {
                                continue;
                            }
                            (
                                t.to_string(),
                                text_of(&v["message"]["content"]),
                                v["timestamp"].as_str().unwrap_or("").to_string(),
                            )
                        }
                        _ => continue,
                    };
                    if is_noise(&text) {
                        continue;
                    }
                    let sid = v["sessionId"].as_str().unwrap_or(&session_fallback);
                    ins.execute(params![text, role, project, sid, ts, pstr])?;
                    n += 1;
                }
            }
            upsert_file(&tx, &pstr, &fm)?;
            tx.commit()?;
            nf += 1;
            nr += n;
        }
    }
    Ok((nf, nr))
}

fn upsert_file(tx: &Connection, pstr: &str, fm: &FileMeta) -> R<()> {
    tx.execute(
        "INSERT INTO files(path,size,mtime) VALUES(?1,?2,?3)
         ON CONFLICT(path) DO UPDATE SET size=?2, mtime=?3",
        params![pstr, fm.size, fm.mtime],
    )?;
    Ok(())
}

/// Index a directory of markdown notes as whole-file rows (role: "memory" or "wiki").
fn index_md_dir(conn: &mut Connection, dir: &Path, role: &str, project: &str, force: bool) -> R<(usize, usize)> {
    let (mut nf, mut nr) = (0usize, 0usize);
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok((0, 0)),
    };
    for f in entries.flatten() {
        let path = f.path();
        if path.extension().map(|e| e != "md").unwrap_or(true) {
            continue;
        }
        let Some(fm) = meta_of(&path) else { continue };
        if !force && unchanged(conn, &fm) {
            continue;
        }
        let pstr = fm.path.to_string_lossy().to_string();
        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let tx = conn.transaction()?;
        let _ = tx.execute(
            "DELETE FROM vec_mem WHERE rowid IN (SELECT rowid FROM mem WHERE file=?1)",
            params![pstr],
        );
        tx.execute("DELETE FROM mem WHERE file=?1", params![pstr])?;
        if !text.trim().is_empty() {
            tx.execute(
                "INSERT INTO mem(text, role, project, session, ts, file) VALUES(?1,?2,?3,?4,?5,?6)",
                params![text, role, project, name, iso_date(fm.mtime), pstr],
            )?;
            nr += 1;
        }
        upsert_file(&tx, &pstr, &fm)?;
        tx.commit()?;
        nf += 1;
    }
    Ok((nf, nr))
}

fn index(force: bool) -> R<()> {
    let mut conn = open_db()?;
    let (tf, tr) = index_transcripts(&mut conn, force)?;
    let (mut mf, mut mr) = (0usize, 0usize);
    let projects = home().join(".claude/projects");
    if let Ok(entries) = fs::read_dir(&projects) {
        for pdir in entries.flatten() {
            if !pdir.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let project = project_label(&pdir.file_name().to_string_lossy());
            let (a, b) = index_md_dir(&mut conn, &pdir.path().join("memory"), "memory", &project, force)?;
            mf += a;
            mr += b;
        }
    }
    let (wf, wr) = index_md_dir(&mut conn, &data_dir().join("wiki"), "wiki", "wiki", force)?;
    // incremental semantic pass — only once `cml embed` has initialized the vector table,
    // so a hook never triggers a model download
    let mut embedded = 0usize;
    if vec_table_exists(&conn) {
        if let Ok(model) = embedder() {
            embedded = embed_new(&conn, &model).unwrap_or(0);
        }
    }
    println!(
        "indexed {} file(s), {} row(s), {embedded} embedded  [transcripts {tf}/{tr}, memory {mf}/{mr}, wiki {wf}/{wr}]",
        tf + mf + wf,
        tr + mr + wr
    );
    Ok(())
}

// ---------- search ----------

fn search(args: &[String]) -> R<()> {
    let mut limit = 12usize;
    let mut project: Option<String> = None;
    let mut role: Option<String> = None;
    let mut semantic_only = false;
    let mut keyword_only = false;
    let mut terms: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--limit" => {
                i += 1;
                limit = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(12);
            }
            "--project" => {
                i += 1;
                project = args.get(i).cloned();
            }
            "--role" => {
                i += 1;
                role = args.get(i).cloned();
            }
            "--semantic" => semantic_only = true,
            "--keyword" => keyword_only = true,
            t => terms.push(t.to_string()),
        }
        i += 1;
    }
    if terms.is_empty() {
        return Err("usage: cml search <terms> [--project P] [--role R] [--limit N] [--semantic|--keyword]".into());
    }
    let conn = open_db()?;

    // keyword leg: FTS5 BM25
    let fts_hits: Vec<i64> = if semantic_only {
        Vec::new()
    } else {
        let q = terms
            .iter()
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" ");
        let mut stmt =
            conn.prepare("SELECT rowid FROM mem WHERE mem MATCH ?1 ORDER BY rank LIMIT 60")?;
        let ids: Vec<i64> = stmt
            .query_map(params![q], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        ids
    };

    // semantic leg: local embeddings + sqlite-vec KNN. Hybrid by default once `cml embed` ran.
    let vec_hits: Vec<i64> = if keyword_only || !vec_table_exists(&conn) {
        if semantic_only {
            return Err("semantic index missing — run `cml embed` once to build it".into());
        }
        Vec::new()
    } else {
        match embedder() {
            Ok(model) => {
                let q_emb = model.encode(&[terms.join(" ")]);
                let mut stmt = conn.prepare(
                    "SELECT rowid FROM vec_mem WHERE embedding MATCH ?1 AND k = ?2 ORDER BY distance",
                )?;
                let ids: Vec<i64> = stmt
                    .query_map(params![q_emb[0].as_bytes(), 60i64], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                ids
            }
            Err(e) => {
                if semantic_only {
                    return Err(e);
                }
                Vec::new()
            }
        }
    };

    // reciprocal rank fusion across both legs
    let mut score: std::collections::HashMap<i64, f64> = std::collections::HashMap::new();
    for (i, id) in fts_hits.iter().enumerate() {
        *score.entry(*id).or_default() += 1.0 / (60.0 + i as f64);
    }
    for (i, id) in vec_hits.iter().enumerate() {
        *score.entry(*id).or_default() += 1.0 / (60.0 + i as f64);
    }
    let mut ranked: Vec<(i64, f64)> = score.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let mut fetch = conn.prepare(
        "SELECT ts, role, project, session, substr(text,1,170) FROM mem WHERE rowid=?1",
    )?;
    let mut printed = 0usize;
    for (rowid, _) in &ranked {
        if printed >= limit {
            break;
        }
        let row = fetch.query_row(params![rowid], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        });
        let Ok((ts, rrole, proj, sess, snip)) = row else {
            continue;
        };
        if let Some(p) = &project {
            if !proj.to_lowercase().contains(&p.to_lowercase()) {
                continue;
            }
        }
        if let Some(want) = &role {
            if &rrole != want {
                continue;
            }
        }
        let snip = snip.split_whitespace().collect::<Vec<_>>().join(" ");
        let date: String = if ts.len() >= 10 {
            ts.chars().take(10).collect()
        } else {
            "no-date   ".into()
        };
        let sid: String = sess.chars().take(8).collect();
        println!("{date} {rrole:<9} {proj:<14} {sid} | {snip}");
        printed += 1;
    }
    if printed == 0 {
        println!(
            "no hits for: {} ({})",
            terms.join(" "),
            if vec_hits.is_empty() { "keyword" } else { "hybrid" }
        );
    }
    Ok(())
}

// ---------- semantic: local embeddings (model2vec) + sqlite-vec, zero API calls ----------

const EMBED_CHARS: i64 = 2000;

/// Default is tiny + fast; CML_EMBED_MODEL swaps in e.g. potion-base-32M (better recall)
/// or potion-multilingual-128M (non-English) — then run `cml embed --all` to rebuild.
fn embed_model_id() -> String {
    env::var("CML_EMBED_MODEL").unwrap_or_else(|_| "minishlab/potion-base-8M".into())
}

fn embedder() -> R<model2vec_rs::model::StaticModel> {
    model2vec_rs::model::StaticModel::from_pretrained(&embed_model_id(), None, Some(true), None)
        .map_err(|e| format!("embedding model unavailable ({e}); run `cml embed` with network once").into())
}

fn vec_table_exists(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name='vec_mem'",
        [],
        |_| Ok(()),
    )
    .is_ok()
}

/// Embed every mem row that has no vector yet, in batches. Returns how many were embedded.
fn embed_new(conn: &Connection, model: &model2vec_rs::model::StaticModel) -> R<usize> {
    let rows: Vec<(i64, String)> = conn
        .prepare(
            "SELECT rowid, substr(text,1,?1) FROM mem
             WHERE rowid NOT IN (SELECT rowid FROM vec_mem)",
        )?
        .query_map(params![EMBED_CHARS], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if rows.is_empty() {
        return Ok(0);
    }
    let mut ins = conn.prepare("INSERT INTO vec_mem(rowid, embedding) VALUES (?1, ?2)")?;
    for chunk in rows.chunks(256) {
        let texts: Vec<String> = chunk.iter().map(|(_, t)| t.clone()).collect();
        let embs = model.encode(&texts);
        for ((rowid, _), emb) in chunk.iter().zip(embs.iter()) {
            ins.execute(params![rowid, emb.as_bytes()])?;
        }
    }
    Ok(rows.len())
}

fn embed_cmd(args: &[String]) -> R<()> {
    let conn = open_db()?;
    if args.iter().any(|a| a == "--all") && vec_table_exists(&conn) {
        conn.execute_batch("DROP TABLE vec_mem;")?;
    }
    let model = embedder()?;
    let dim = model.encode(&["init".to_string()])[0].len();
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS vec_mem USING vec0(embedding float[{dim}]);"
    ))?;
    let t0 = std::time::Instant::now();
    let n = embed_new(&conn, &model)?;
    let total: i64 = conn.query_row("SELECT count(*) FROM vec_mem", [], |r| r.get(0))?;
    println!(
        "embedded {n} new row(s) in {:.1}s ({total} total, {dim}-dim, {})",
        t0.elapsed().as_secs_f32(),
        embed_model_id()
    );
    Ok(())
}

// ---------- learning loop: capture (Stop hook) + nudge (SessionStart hook) ----------

const CORRECTION_WORDS: &[&str] = &[
    "no", "not", "dont", "stop", "wrong", "actually", "instead", "should", "shouldnt", "isnt",
    "doesnt", "never", "avoid", "bugged", "fix", "why",
];

fn looks_like_correction(text: &str) -> bool {
    let lowered = text.to_lowercase().replace(['\u{2019}', '\''], "");
    lowered
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| CORRECTION_WORDS.contains(&w))
}

fn read_stdin_json() -> Value {
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);
    serde_json::from_str(&buf).unwrap_or(Value::Null)
}

fn inbox_path_for(cwd: &str) -> PathBuf {
    let label = project_label(&flatten(cwd));
    data_dir().join("inbox").join(format!("{label}.md"))
}

fn last_user_message(transcript_path: &str) -> String {
    let Ok(f) = fs::File::open(transcript_path) else {
        return String::new();
    };
    let mut found = String::new();
    for line in BufReader::new(f).lines() {
        let Ok(line) = line else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v["type"] == "user" || v["message"]["role"] == "user" {
            if v["isSidechain"] == true {
                continue;
            }
            let txt = text_of(&v["message"]["content"]);
            if !txt.trim().is_empty() {
                found = txt;
            }
        }
    }
    found
}

/// Stop hook: append the turn's user message to the per-project learning inbox.
/// Exits 0 on every path — a memory hook must never block the session.
fn capture() {
    let data = read_stdin_json();
    let cwd = data["cwd"].as_str().unwrap_or_default();
    let tp = data["transcript_path"].as_str().unwrap_or_default();
    if cwd.is_empty() || tp.is_empty() {
        return;
    }
    let txt = last_user_message(tp);
    if txt.trim().is_empty() || is_noise(&txt) {
        return;
    }
    let flag = if looks_like_correction(&txt) {
        "correction?"
    } else {
        "note"
    };
    let snippet: String = txt.split_whitespace().collect::<Vec<_>>().join(" ");
    let snippet: String = snippet.chars().take(300).collect();
    let inbox = inbox_path_for(cwd);
    if let Some(parent) = inbox.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let line = format!("- [{}] ({flag}) {snippet}\n", iso_minute(now_secs()));
    let prev = fs::read_to_string(&inbox).unwrap_or_default();
    let _ = fs::write(&inbox, prev + &line);
}

/// SessionStart hook: nudge consolidation once enough raw signals pile up.
fn nudge() {
    let data = read_stdin_json();
    let cwd = data["cwd"].as_str().unwrap_or_default();
    let threshold: usize = env::var("CML_NUDGE_THRESHOLD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let passthrough = json!({"continue": true});
    if cwd.is_empty() {
        println!("{passthrough}");
        return;
    }
    let inbox = inbox_path_for(cwd);
    let n = fs::read_to_string(&inbox)
        .map(|s| s.lines().filter(|l| l.trim_start().starts_with("- [")).count())
        .unwrap_or(0);
    if n >= threshold {
        let msg = format!(
            "[claude-memory-light learning loop] {n} raw signals captured since last consolidation ({}). \
             Early this session, before deep work: read the inbox, distill any durable correction / \
             preference / non-obvious workflow into persistent memory (and promote anything recurring \
             into CLAUDE.md), then delete the consolidated lines. Drop the noise — most lines are nothing. \
             Full-history recall is available via `cml search`.",
            inbox.display()
        );
        println!(
            "{}",
            json!({
                "hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": msg},
                "continue": true
            })
        );
    } else {
        println!("{passthrough}");
    }
}

// ---------- map: 3D memory visualization, one static HTML file, no server ----------

/// Extract [[wikilink]] target names, lowercased. No regex dep needed.
fn wikilinks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(a) = rest.find("[[") {
        rest = &rest[a + 2..];
        match rest.find("]]") {
            Some(b) => {
                let name = rest[..b].trim().to_lowercase();
                if !name.is_empty() && name.len() < 100 {
                    out.push(name);
                }
                rest = &rest[b + 2..];
            }
            None => break,
        }
    }
    out
}

/// Deterministic pseudo-random in [-1,1] from a seed — layout jitter without an RNG dep.
fn jit(seed: u64) -> f64 {
    let x = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    ((x >> 33) as u32) as f64 / u32::MAX as f64 * 2.0 - 1.0
}

/// i-th of n points on a unit sphere (golden-angle fibonacci lattice).
fn fib_sphere(i: usize, n: usize) -> (f64, f64, f64) {
    let nf = n.max(1) as f64;
    let y = 1.0 - 2.0 * (i as f64 + 0.5) / nf;
    let r = (1.0 - y * y).max(0.0).sqrt();
    let phi = i as f64 * 2.399963229728653;
    (r * phi.cos(), y, r * phi.sin())
}

fn map(args: &[String]) -> R<()> {
    let mut limit = 6000usize;
    let mut open = true;
    let mut code_path: Option<PathBuf> = None;
    let mut no_code = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--limit" => {
                i += 1;
                limit = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(6000);
            }
            "--no-open" => open = false,
            "--no-code" => no_code = true,
            "--code" => {
                i += 1;
                code_path = args.get(i).map(PathBuf::from);
            }
            _ => {}
        }
        i += 1;
    }
    if code_path.is_none() && !no_code {
        let auto = PathBuf::from("graphify-out/graph.json");
        if auto.is_file() {
            code_path = Some(auto);
        }
    }
    let conn = open_db()?;

    // ---- pass A: collect everything ----
    struct MRow {
        rowid: i64,
        role: String,
        pidx: usize,
        sidx: Option<usize>,
        date: String,
        snip: String,
        sess8: String,
        project: String,
    }
    let mut proj_names: Vec<String> = Vec::new();
    let mut proj_of: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut sess_list: Vec<(String, usize)> = Vec::new();
    let mut sess_of: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut msgs: Vec<MRow> = Vec::new();
    let mut role_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut note_ids: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut pending_wikilinks: Vec<(String, Vec<String>)> = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT rowid, role, project, session, ts, text FROM mem ORDER BY rowid DESC LIMIT ?1",
        )?;
        let mut rows = stmt.query(params![limit as i64])?;
        while let Some(r) = rows.next()? {
            let rowid: i64 = r.get(0)?;
            let role: String = r.get(1)?;
            let project: String = r.get(2)?;
            let session: String = r.get(3)?;
            let ts: String = r.get(4)?;
            let text: String = r.get(5)?;
            // wiki pages hang off the core directly — registering their pseudo-project
            // would put an empty planet on the ring
            let pidx = if role == "wiki" {
                0
            } else {
                *proj_of.entry(project.clone()).or_insert_with(|| {
                    proj_names.push(project.clone());
                    proj_names.len() - 1
                })
            };
            let sidx = if role == "memory" || role == "wiki" {
                None
            } else {
                Some(*sess_of.entry(session.clone()).or_insert_with(|| {
                    sess_list.push((session.clone(), pidx));
                    sess_list.len() - 1
                }))
            };
            *role_counts.entry(role.clone()).or_default() += 1;
            let mid = format!("m:{rowid}");
            if role == "memory" || role == "wiki" {
                note_ids.insert(session.to_lowercase(), mid.clone());
                let found = wikilinks(&text);
                if !found.is_empty() {
                    pending_wikilinks.push((mid, found));
                }
            }
            msgs.push(MRow {
                rowid,
                role,
                pidx,
                sidx,
                date: if ts.len() >= 10 { ts.chars().take(10).collect() } else { String::new() },
                snip: text.chars().take(320).collect(),
                sess8: session.chars().take(8).collect(),
                project,
            });
        }
    }
    // code graph (optional)
    let mut code_nodes: Vec<(String, String, String)> = Vec::new();
    let mut code_edges: Vec<(String, String)> = Vec::new();
    let mut code_root_label = String::new();
    if let Some(cp) = &code_path {
        match fs::read_to_string(cp)
            .map_err(|e| e.to_string())
            .and_then(|s| serde_json::from_str::<Value>(&s).map_err(|e| e.to_string()))
        {
            Ok(g) => {
                const CODE_CAP: usize = 3000;
                code_root_label = env::current_dir()
                    .ok()
                    .and_then(|d| d.file_name().map(|n| n.to_string_lossy().to_string()))
                    .unwrap_or_else(|| "repo".into());
                let empty = Vec::new();
                let gnodes = g["nodes"].as_array().unwrap_or(&empty);
                let mut kept: std::collections::HashSet<String> = std::collections::HashSet::new();
                for n in gnodes.iter().take(CODE_CAP) {
                    let Some(id) = n["id"].as_str() else { continue };
                    let label = n["label"].as_str().unwrap_or(id);
                    let ftype = n["file_type"].as_str().unwrap_or("code");
                    let src = n["source_file"].as_str().unwrap_or("");
                    kept.insert(id.to_string());
                    code_nodes.push((
                        id.to_string(),
                        label.to_string(),
                        format!("{label}\n[{ftype}] {src}"),
                    ));
                }
                if gnodes.len() > CODE_CAP {
                    println!("code graph capped at {CODE_CAP} of {} nodes", gnodes.len());
                }
                let gedges = g["edges"].as_array().or_else(|| g["links"].as_array()).unwrap_or(&empty);
                for e in gedges {
                    let (Some(s), Some(t)) = (e["source"].as_str(), e["target"].as_str()) else {
                        continue;
                    };
                    if kept.contains(s) && kept.contains(t) {
                        code_edges.push((s.to_string(), t.to_string()));
                    }
                }
            }
            Err(e) => eprintln!("cml: could not read code graph {}: {e}", cp.display()),
        }
    }

    // ---- pass B: deterministic layout, computed here so the browser does ZERO physics ----
    let has_code = !code_nodes.is_empty();
    let slots = proj_names.len() + usize::from(has_code);
    let ring_r = 300.0;
    let slot_pos = |k: usize| -> (f64, f64, f64) {
        let th = std::f64::consts::TAU * k as f64 / slots.max(1) as f64;
        (ring_r * th.cos(), ((k % 3) as f64 - 1.0) * 46.0, ring_r * th.sin())
    };
    // sessions on shells around their project
    let mut sess_by_p: Vec<Vec<usize>> = vec![Vec::new(); proj_names.len()];
    for (si, (_, p)) in sess_list.iter().enumerate() {
        sess_by_p[*p].push(si);
    }
    let mut spos: Vec<(f64, f64, f64)> = vec![(0.0, 0.0, 0.0); sess_list.len()];
    for (p, list) in sess_by_p.iter().enumerate() {
        let base = slot_pos(p);
        let r = 110.0 + (list.len() as f64).sqrt() * 7.0;
        for (j, &si) in list.iter().enumerate() {
            let (x, y, z) = fib_sphere(j, list.len());
            spos[si] = (base.0 + x * r, base.1 + y * r * 0.72, base.2 + z * r);
        }
    }
    // messages on shells around their parent
    let mut order: std::collections::HashMap<(u8, usize), usize> = std::collections::HashMap::new();
    let mut totals: std::collections::HashMap<(u8, usize), usize> = std::collections::HashMap::new();
    let key_of = |m: &MRow| -> (u8, usize) {
        match (&m.sidx, m.role.as_str()) {
            (Some(s), _) => (0u8, *s),
            (None, "wiki") => (2u8, 0),
            (None, _) => (1u8, m.pidx),
        }
    };
    for m in &msgs {
        *totals.entry(key_of(m)).or_default() += 1;
    }
    let mut nodes: Vec<Value> = Vec::with_capacity(msgs.len() + sess_list.len() + slots + 2);
    let mut links: Vec<Value> = Vec::with_capacity(msgs.len() + sess_list.len() + slots + code_edges.len());
    nodes.push(json!({"id":"center","group":"center","label":"memory","val":34,"fx":0.0,"fy":0.0,"fz":0.0}));
    for (p, name) in proj_names.iter().enumerate() {
        let (x, y, z) = slot_pos(p);
        nodes.push(json!({"id":format!("p:{name}"),"group":"project","label":name,"val":16,"fx":x,"fy":y,"fz":z}));
        links.push(json!({"source":"center","target":format!("p:{name}"),"kind":"spine"}));
    }
    for (si, (sid, p)) in sess_list.iter().enumerate() {
        let (x, y, z) = spos[si];
        let sess8: String = sid.chars().take(8).collect();
        nodes.push(json!({"id":format!("s:{sid}"),"group":"session","label":sess8,"val":7,"fx":x,"fy":y,"fz":z}));
        links.push(json!({"source":format!("p:{}", proj_names[*p]),"target":format!("s:{sid}"),"kind":"spine"}));
    }
    for m in &msgs {
        let k = key_of(m);
        let idx = {
            let e = order.entry(k).or_default();
            let v = *e;
            *e += 1;
            v
        };
        let n = totals[&k];
        let (bx, by, bz, r) = match k.0 {
            0 => {
                let b = spos[k.1];
                (b.0, b.1, b.2, 26.0 + (n as f64).cbrt() * 7.0)
            }
            1 => {
                let b = slot_pos(k.1);
                (b.0, b.1, b.2, 78.0)
            }
            _ => (0.0, 0.0, 0.0, 140.0),
        };
        let (ux, uy, uz) = fib_sphere(idx, n);
        let s = m.rowid as u64;
        let (x, y, z) = (
            bx + ux * r + jit(s) * 4.0,
            by + uy * r + jit(s ^ 0xA5A5) * 4.0,
            bz + uz * r + jit(s ^ 0x5A5A) * 4.0,
        );
        let parent = match k.0 {
            0 => format!("s:{}", sess_list[k.1].0),
            1 => format!("p:{}", proj_names[k.1]),
            _ => "center".to_string(),
        };
        nodes.push(json!({
            "id": format!("m:{}", m.rowid), "group": m.role, "label": m.date, "snippet": m.snip,
            "project": m.project, "session": m.sess8, "ts": m.date,
            "val": if m.role == "memory" || m.role == "wiki" { 5.0 } else { 1.6 },
            "fx": x, "fy": y, "fz": z
        }));
        links.push(json!({"source":parent,"target":format!("m:{}", m.rowid),"kind":"leaf"}));
    }
    for (from, targets) in pending_wikilinks {
        for name in targets {
            if let Some(to) = note_ids.get(&name) {
                if to != &from {
                    links.push(json!({"source":from,"target":to,"kind":"wikilink"}));
                }
            }
        }
    }
    let mut n_code = 0usize;
    if has_code {
        let root = format!("code:{code_root_label}");
        let (cx, cy, cz) = slot_pos(slots - 1);
        nodes.push(json!({"id":root,"group":"coderoot","label":"code","val":18,"fx":cx,"fy":cy,"fz":cz}));
        links.push(json!({"source":"center","target":root,"kind":"spine"}));
        let cn = code_nodes.len();
        let cr = 60.0 + (cn as f64).cbrt() * 9.0;
        for (idx, (id, label, snip)) in code_nodes.iter().enumerate() {
            let (ux, uy, uz) = fib_sphere(idx, cn);
            nodes.push(json!({
                "id": format!("c:{id}"), "group": "code", "label": label, "snippet": snip,
                "project": "code", "session": "", "ts": "", "val": 2.4,
                "fx": cx + ux * cr, "fy": cy + uy * cr, "fz": cz + uz * cr
            }));
            links.push(json!({"source":root,"target":format!("c:{id}"),"kind":"tether"}));
            n_code += 1;
        }
        for (s, t) in &code_edges {
            links.push(json!({"source":format!("c:{s}"),"target":format!("c:{t}"),"kind":"code"}));
        }
    }
    let n_msgs = msgs.len();
    let n_nodes = nodes.len();
    let n_links = links.len();
    let db = data_dir().join("index.db");
    let db_mb = fs::metadata(&db)
        .map(|m| format!("{:.1}", m.len() as f64 / 1e6))
        .unwrap_or_else(|_| "?".into());
    let data = json!({
        "nodes": nodes, "links": links,
        "stats": {"rows": n_msgs, "sessions": sess_list.len(), "projects": proj_names.len(),
                  "db_mb": db_mb, "roles": role_counts}
    });
    // "</" would close the inline <script> if a snippet contains it
    let payload = data.to_string().replace("</", "<\\/");
    let html = include_str!("map.html")
        .replace("/*%%PAYLOAD%%*/ null", &payload)
        .replace("/*%%VENDOR%%*/", include_str!("vendor/3d-force-graph.min.js"));
    let out = data_dir().join("map.html");
    fs::write(&out, &html)?;
    println!(
        "map: {} ({n_nodes} nodes, {n_links} links, {n_code} code, {:.1} MB)",
        out.display(),
        html.len() as f64 / 1e6
    );
    if open {
        let _ = std::process::Command::new("xdg-open").arg(&out).spawn();
    }
    Ok(())
}

// ---------- stats / doctor ----------

fn stats() -> R<()> {
    let conn = open_db()?;
    let msgs: i64 = conn.query_row("SELECT count(*) FROM mem", [], |r| r.get(0))?;
    let sessions: i64 = conn.query_row(
        "SELECT count(DISTINCT session) FROM mem WHERE role IN ('user','assistant','summary')",
        [],
        |r| r.get(0),
    )?;
    let files: i64 = conn.query_row("SELECT count(*) FROM files", [], |r| r.get(0))?;
    let by_role: Vec<String> = conn
        .prepare("SELECT role, count(*) FROM mem GROUP BY role ORDER BY 2 DESC")?
        .query_map([], |r| {
            Ok(format!("{}={}", r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let db = data_dir().join("index.db");
    let mb = fs::metadata(&db).map(|m| m.len() as f64 / 1e6).unwrap_or(0.0);
    println!(
        "{msgs} rows ({}) | {sessions} sessions | {files} files | {mb:.1} MB at {}",
        by_role.join(", "),
        db.display()
    );
    Ok(())
}

fn doctor() -> R<()> {
    let ok = |b: bool| if b { "ok" } else { "MISSING" };
    let projects = home().join(".claude/projects");
    println!("transcripts dir : {} ({})", projects.display(), ok(projects.is_dir()));
    println!("data dir        : {} ({})", data_dir().display(), ok(data_dir().is_dir()));
    let db = data_dir().join("index.db");
    println!("index db        : {} ({})", db.display(), ok(db.is_file()));
    let graphify = which("graphify");
    println!(
        "graphify        : {} (optional structural-memory companion)",
        graphify.as_deref().unwrap_or("not installed")
    );
    {
        let conn = open_db()?;
        if vec_table_exists(&conn) {
            let v: i64 = conn.query_row("SELECT count(*) FROM vec_mem", [], |r| r.get(0))?;
            let m: i64 = conn.query_row("SELECT count(*) FROM mem", [], |r| r.get(0))?;
            println!("semantic        : {v}/{m} rows embedded ({})", embed_model_id());
        } else {
            println!("semantic        : off — run `cml embed` once to enable hybrid search");
        }
    }
    println!("hint: keep transcripts forever with \"cleanupPeriodDays\": 3650 in ~/.claude/settings.json");
    if db.is_file() {
        stats()?;
    } else {
        println!("run `cml index` to build the index");
    }
    Ok(())
}

fn which(bin: &str) -> Option<String> {
    let paths = env::var("PATH").ok()?;
    for dir in paths.split(':') {
        let p = Path::new(dir).join(bin);
        if p.is_file() {
            return Some(p.to_string_lossy().to_string());
        }
    }
    None
}

// ---------- main ----------

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str);
    // Hook-facing subcommands: never fail the session, no matter what.
    match cmd {
        Some("capture") => {
            capture();
            return;
        }
        Some("nudge") => {
            nudge();
            return;
        }
        _ => {}
    }
    let res = match cmd {
        Some("index") => index(args.iter().any(|a| a == "--all")),
        Some("search") => search(&args[1..]),
        Some("stats") => stats(),
        Some("doctor") => doctor(),
        Some("map") => map(&args[1..]),
        Some("embed") => embed_cmd(&args[1..]),
        _ => Err(
            "usage: cml index [--all] | search <terms> [--project P] [--role R] [--limit N] [--semantic|--keyword] | embed [--all] | map [--limit N] [--no-open] | stats | doctor | capture | nudge"
                .into(),
        ),
    };
    if let Err(e) = res {
        eprintln!("cml: {e}");
        std::process::exit(1);
    }
}
