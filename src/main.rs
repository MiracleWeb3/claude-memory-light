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

use rusqlite::{params, params_from_iter, Connection};
use serde_json::{json, Value};
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
    if dirname == fh {
        "home".to_string()
    } else if let Some(rest) = dirname.strip_prefix(&format!("{fh}-")) {
        rest.to_string()
    } else {
        dirname.trim_start_matches('-').to_string()
    }
}

fn open_db() -> R<Connection> {
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
    println!(
        "indexed {} file(s), {} row(s)  [transcripts {tf}/{tr}, memory {mf}/{mr}, wiki {wf}/{wr}]",
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
            t => terms.push(t.to_string()),
        }
        i += 1;
    }
    if terms.is_empty() {
        return Err("usage: cml search <terms> [--project P] [--role user|assistant|summary|memory|wiki] [--limit N]".into());
    }
    let q = terms
        .iter()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ");
    let mut wheres = vec!["mem MATCH ?".to_string()];
    let mut binds: Vec<String> = vec![q.clone()];
    if let Some(p) = &project {
        wheres.push("project LIKE ?".to_string());
        binds.push(format!("%{p}%"));
    }
    if let Some(r) = &role {
        wheres.push("role = ?".to_string());
        binds.push(r.clone());
    }
    let conn = open_db()?;
    let sql = format!(
        "SELECT ts, role, project, session, snippet(mem, 0, '[', ']', ' … ', 16)
         FROM mem WHERE {} ORDER BY rank LIMIT {}",
        wheres.join(" AND "),
        limit
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<String> = stmt
        .query_map(params_from_iter(binds.iter()), |r| {
            let ts: String = r.get(0)?;
            let role: String = r.get(1)?;
            let proj: String = r.get(2)?;
            let sess: String = r.get(3)?;
            let snip: String = r.get(4)?;
            let snip = snip.split_whitespace().collect::<Vec<_>>().join(" ");
            let date = if ts.len() >= 10 {
                ts.chars().take(10).collect::<String>()
            } else {
                "no-date   ".to_string()
            };
            let sid: String = sess.chars().take(8).collect();
            Ok(format!("{date} {role:<9} {proj:<14} {sid} | {snip}"))
        })?
        .collect::<rusqlite::Result<_>>()?;
    if rows.is_empty() {
        println!("no hits for: {q}");
    }
    for row in &rows {
        println!("{row}");
    }
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
    // graphify integration: auto-detect the cwd's graph unless told otherwise
    if code_path.is_none() && !no_code {
        let auto = PathBuf::from("graphify-out/graph.json");
        if auto.is_file() {
            code_path = Some(auto);
        }
    }
    let conn = open_db()?;
    let mut nodes: Vec<Value> =
        vec![json!({"id":"center","group":"center","label":"memory","val":34})];
    let mut links: Vec<Value> = Vec::new();
    let mut seen_p = std::collections::HashSet::new();
    let mut seen_s = std::collections::HashSet::new();
    let mut note_ids: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut pending_wikilinks: Vec<(String, Vec<String>)> = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT rowid, role, project, session, ts, text FROM mem ORDER BY rowid DESC LIMIT ?1",
    )?;
    let mut rows = stmt.query(params![limit as i64])?;
    let mut n_msgs = 0usize;
    while let Some(r) = rows.next()? {
        let rowid: i64 = r.get(0)?;
        let role: String = r.get(1)?;
        let project: String = r.get(2)?;
        let session: String = r.get(3)?;
        let ts: String = r.get(4)?;
        let text: String = r.get(5)?;
        let snip: String = text.chars().take(180).collect();
        let date: String = if ts.len() >= 10 {
            ts.chars().take(10).collect()
        } else {
            String::new()
        };
        let sess8: String = session.chars().take(8).collect();
        let pid = format!("p:{project}");
        if seen_p.insert(project.clone()) {
            nodes.push(json!({"id":pid,"group":"project","label":project,"val":16}));
            links.push(json!({"source":"center","target":pid,"kind":"spine"}));
        }
        let mid = format!("m:{rowid}");
        let parent = if role == "memory" {
            pid.clone()
        } else if role == "wiki" {
            "center".to_string()
        } else {
            let sid = format!("s:{session}");
            if seen_s.insert(sid.clone()) {
                nodes.push(json!({"id":sid,"group":"session","label":sess8,"val":7}));
                links.push(json!({"source":pid,"target":sid,"kind":"spine"}));
            }
            sid
        };
        nodes.push(json!({
            "id": mid, "group": role, "label": date, "snippet": snip,
            "project": project, "session": sess8, "ts": date,
            "val": if role == "memory" || role == "wiki" { 5.0 } else { 1.6 }
        }));
        links.push(json!({"source":parent,"target":mid,"kind":"leaf"}));
        if role == "memory" || role == "wiki" {
            note_ids.insert(session.to_lowercase(), mid.clone());
            let found = wikilinks(&text);
            if !found.is_empty() {
                pending_wikilinks.push((mid, found));
            }
        }
        n_msgs += 1;
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
    if let Some(cp) = &code_path {
        match fs::read_to_string(cp).map_err(|e| e.to_string()).and_then(|s| {
            serde_json::from_str::<Value>(&s).map_err(|e| e.to_string())
        }) {
            Ok(g) => {
                const CODE_CAP: usize = 3000;
                let root = format!(
                    "code:{}",
                    env::current_dir()
                        .ok()
                        .and_then(|d| d.file_name().map(|n| n.to_string_lossy().to_string()))
                        .unwrap_or_else(|| "repo".into())
                );
                nodes.push(json!({"id":root,"group":"coderoot","label":"code","val":18}));
                links.push(json!({"source":"center","target":root,"kind":"spine"}));
                let mut kept: std::collections::HashSet<String> = std::collections::HashSet::new();
                let empty = Vec::new();
                let gnodes = g["nodes"].as_array().unwrap_or(&empty);
                for n in gnodes.iter().take(CODE_CAP) {
                    let Some(id) = n["id"].as_str() else { continue };
                    let label = n["label"].as_str().unwrap_or(id);
                    let ftype = n["file_type"].as_str().unwrap_or("code");
                    let src = n["source_file"].as_str().unwrap_or("");
                    kept.insert(id.to_string());
                    nodes.push(json!({
                        "id": format!("c:{id}"), "group": "code", "label": label,
                        "snippet": format!("{label}\n[{ftype}] {src}"),
                        "project": "code", "session": "", "ts": "", "val": 2.4
                    }));
                    links.push(json!({"source":root,"target":format!("c:{id}"),"kind":"tether"}));
                    n_code += 1;
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
                        links.push(json!({"source":format!("c:{s}"),"target":format!("c:{t}"),"kind":"code"}));
                    }
                }
            }
            Err(e) => eprintln!("cml: could not read code graph {}: {e}", cp.display()),
        }
    }
    let n_nodes = nodes.len();
    let n_links = links.len();
    let db = data_dir().join("index.db");
    let db_mb = fs::metadata(&db)
        .map(|m| format!("{:.1}", m.len() as f64 / 1e6))
        .unwrap_or_else(|_| "?".into());
    let data = json!({
        "nodes": nodes, "links": links,
        "stats": {"rows": n_msgs, "sessions": seen_s.len(), "projects": seen_p.len(), "db_mb": db_mb}
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
        _ => Err(
            "usage: cml index [--all] | search <terms> [--project P] [--role R] [--limit N] | map [--limit N] [--no-open] | stats | doctor | capture | nudge"
                .into(),
        ),
    };
    if let Err(e) = res {
        eprintln!("cml: {e}");
        std::process::exit(1);
    }
}
