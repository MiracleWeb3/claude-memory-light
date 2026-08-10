//! `cml map` — the whole index as one static HTML file: no server, no build step.
//!
//! Collects the index, hands it to the layout, writes one self-contained page and
//! opens it. three.js, OrbitControls and app.js are baked into the binary, so the
//! output opens from a `file://` URL on a machine with no network and no
//! node_modules.

mod layout;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::Value;

use crate::text::{gist_lookup, stable_key};
use layout::{build_payload, CodeNode, MRow, MapData};

// The page's four ingredients stay REAL FILES under `assets/`, pulled in at compile
// time. Same single binary, same single-file output — but the JS and CSS remain
// editable, diffable and greppable instead of being trapped inside a string
// literal, and none of it counts against a source file's line budget. The C++ tree
// needed an assembler stub (`.incbin`, plus two dialects of it for ELF and Mach-O)
// to avoid making the compiler chew through a 1.3 MB literal; this is that, in four
// lines, with the paths checked by the build.
const MAP_HTML: &str = include_str!("../assets/map.html");
const THREE_JS: &str = include_str!("../assets/vendor/three.module.js");
const CONTROLS_JS: &str = include_str!("../assets/vendor/OrbitControls.js");
const APP_JS: &str = include_str!("../assets/app.js");

/// Where each asset is spliced in. One table, so the test below proves the page
/// still has a slot for every one of them — a renamed placeholder is silent.
const ASSET_SLOTS: [(&str, &str); 3] = [
    ("/*%%THREE%%*/", THREE_JS),
    ("/*%%CONTROLS%%*/", CONTROLS_JS),
    ("/*%%APP%%*/", APP_JS),
];
const PAYLOAD_SLOT: &str = "/*%%PAYLOAD%%*/ null";

const CODE_CAP: usize = 3000;
const DEFAULT_LIMIT: i64 = 6000;

pub fn run(args: &[String]) -> crate::R<i32> {
    let mut limit = DEFAULT_LIMIT;
    let mut open_page = true;
    let mut knowledge_only = true;
    let mut code_path: Option<PathBuf> = None;
    let mut want_code = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--limit" => {
                i += 1;
                // A malformed limit falls back to the default rather than failing:
                // this is a viewer, and 6,000 rows is always a sane picture.
                limit = args.get(i).and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_LIMIT);
            }
            "--no-open" => open_page = false,
            // A row without a gist is search substrate, not knowledge; `--raw`
            // plots every row anyway.
            "--raw" => knowledge_only = false,
            "--no-code" => want_code = false, // now the default; still accepted
            "--code" => {
                want_code = true;
                // The path is optional: bare `--code` auto-detects graphify's output.
                if args.get(i + 1).is_some_and(|a| !a.starts_with("--")) {
                    i += 1;
                    code_path = Some(PathBuf::from(&args[i]));
                }
            }
            // The C++ ignored unknown flags, so a typo'd `--no-oepn` opened the
            // browser anyway and reported success.
            other => return Err(format!("map: unknown flag '{other}'").into()),
        }
        i += 1;
    }
    if want_code && code_path.is_none() {
        let auto = PathBuf::from("graphify-out/graph.json");
        if auto.is_file() {
            code_path = Some(auto);
        } else {
            eprintln!("cml: --code found no graphify-out/graph.json here");
        }
    }

    let conn = crate::db::open()?;
    let mut d = collect(&conn, limit, knowledge_only)?;
    if let Some(p) = &code_path {
        load_code_graph(p, &mut d);
    }
    if let Ok(md) = std::fs::metadata(crate::db::db_path()) {
        d.db_mb = Some(format!("{:.1}", md.len() as f64 / 1e6));
    }

    let payload = build_payload(&d);
    let mut html = MAP_HTML.replace(PAYLOAD_SLOT, &payload.json);
    for (slot, asset) in ASSET_SLOTS {
        html = html.replace(slot, asset);
    }

    let out = crate::db::home().join("map.html");
    std::fs::write(&out, &html)?;
    println!(
        "map: {} ({} nodes, {} links, {} code, {:.1} MB)",
        out.display(),
        payload.nodes,
        payload.links,
        payload.code,
        html.len() as f64 / 1e6
    );
    if open_page {
        // Fire and forget. Waiting would hold the terminal for as long as the page
        // is open, and the browser has no result this process wants.
        let _ = std::process::Command::new("xdg-open").arg(&out).spawn();
    }
    Ok(0)
}

fn collect(conn: &Connection, limit: i64, knowledge_only: bool) -> rusqlite::Result<MapData> {
    let gists = gist_lookup(conn);
    let mut d = MapData::default();
    let mut proj_of: HashMap<String, usize> = HashMap::new();
    let mut sess_of: HashMap<String, usize> = HashMap::new();

    let mut stmt = conn.prepare(
        "SELECT rowid, role, project, session, ts, text FROM mem ORDER BY rowid DESC LIMIT ?1",
    )?;
    let mut rows = stmt.query([limit])?;
    while let Some(r) = rows.next()? {
        let rowid: i64 = r.get(0)?;
        let role: String = r.get(1)?;
        let project: String = r.get(2)?;
        let session: String = r.get(3)?;
        let ts: String = r.get(4)?;
        let text: String = r.get(5)?;

        // A row without a gist is search substrate, not knowledge: the curator
        // either has not reached it yet or judged it unremarkable. Plotting those
        // made the map read as ~1150 facts when the distilled layer holds a
        // fraction of that. Notes are exempt — memory and wiki rows are curated
        // files, and no curator judges them.
        let is_note = role == "memory" || role == "wiki";
        let gist = gists.get(&stable_key(&session, &ts, &role, &text));
        if knowledge_only && !is_note && gist.is_none() {
            continue;
        }

        let mut m = MRow {
            rowid,
            date: if ts.len() >= 10 { take_chars(&ts, 10) } else { String::new() },
            snip: gist.cloned().unwrap_or_else(|| take_chars(&text, 320)),
            sess8: take_chars(&session, 8),
            ..Default::default()
        };
        // wiki pages hang off the core directly — registering their pseudo-project
        // would put an empty planet on the ring.
        if role != "wiki" {
            let next = d.proj_names.len();
            m.pidx = *proj_of.entry(project.clone()).or_insert(next);
            if m.pidx == next {
                d.proj_names.push(project.clone());
            }
        }
        if !is_note {
            let next = d.sess_list.len();
            let si = *sess_of.entry(session.clone()).or_insert(next);
            if si == next {
                d.sess_list.push((session.clone(), m.pidx));
            }
            m.sidx = Some(si);
        }
        *d.role_counts.entry(role.clone()).or_default() += 1;

        let mid = format!("m:{rowid}");
        if is_note {
            // A note's "session" column carries its file name — that is the key a
            // [[wikilink]] in another note points at.
            d.note_ids.insert(session.to_ascii_lowercase(), mid.clone());
            let found = wikilinks(&text);
            if !found.is_empty() {
                d.pending_wikilinks.push((mid, found));
            }
        }
        m.role = role;
        m.project = project;
        d.msgs.push(m);
    }
    Ok(d)
}

/// `[[wikilink]]` target names, trimmed and lowercased — the edges between notes.
/// No regex needed: two literal delimiters and a trim.
pub fn wikilinks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(a) = rest.find("[[") {
        rest = &rest[a + 2..];
        // An unterminated link ends the scan — the rest is prose.
        let Some(b) = rest.find("]]") else { break };
        let name = rest[..b].trim().to_ascii_lowercase();
        // A runaway name is prose that happened to contain both delimiters.
        if !name.is_empty() && name.len() < 100 {
            out.push(name);
        }
        rest = &rest[b + 2..];
    }
    out
}

/// The code graph is OPT-IN (`--code`). Auto-loading `graphify-out/graph.json` made
/// `cml map` depend on the working directory, and it renders badly: the overlay
/// gets one orbital slot — a project's worth — while holding up to [`CODE_CAP`]
/// nodes, so 3,000 orbs pack into that slot as a sphere that occludes the brain.
fn load_code_graph(path: &Path, d: &mut MapData) {
    let g = match read_json(path) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("cml: could not read code graph {}: {e}", path.display());
            return;
        }
    };
    d.code_root_label = std::env::current_dir()
        .ok()
        .and_then(|c| c.file_name().map(|f| f.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "repo".into());

    let mut kept: HashSet<&str> = HashSet::new();
    if let Some(gnodes) = g["nodes"].as_array() {
        for n in gnodes {
            if d.code_nodes.len() >= CODE_CAP {
                break;
            }
            let Some(id) = n["id"].as_str() else { continue };
            let label = n["label"].as_str().unwrap_or(id);
            let ftype = n["file_type"].as_str().unwrap_or("code");
            let src = n["source_file"].as_str().unwrap_or("");
            kept.insert(id);
            d.code_nodes.push(CodeNode {
                id: id.to_string(),
                label: label.to_string(),
                snippet: format!("{label}\n[{ftype}] {src}"),
            });
        }
        if gnodes.len() > CODE_CAP {
            println!("code graph capped at {CODE_CAP} of {} nodes", gnodes.len());
        }
    }
    // graphify writes `edges`; the d3 convention is `links`. Both are read.
    let edges = g["edges"].as_array().or_else(|| g["links"].as_array());
    for e in edges.into_iter().flatten() {
        let (Some(s), Some(t)) = (e["source"].as_str(), e["target"].as_str()) else { continue };
        // An edge to a capped-away node would draw to nothing.
        if kept.contains(s) && kept.contains(t) {
            d.code_edges.push((s.to_string(), t.to_string()));
        }
    }
}

fn read_json(path: &Path) -> crate::R<Value> {
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

/// First `n` characters, never bytes: these are exact prefixes of ids and
/// timestamps, so `text::squeeze` — which collapses whitespace, clips by byte and
/// appends an ellipsis — is the wrong tool for every caller here.
fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every asset needs a slot in the page. A renamed placeholder is invisible at
    /// build time and produces a 1.4 MB HTML file with no renderer in it — the map
    /// opens black, and nothing anywhere says why.
    #[test]
    fn every_asset_has_a_slot_in_the_page() {
        assert!(MAP_HTML.contains(PAYLOAD_SLOT), "map.html no longer has a payload slot");
        for (slot, asset) in ASSET_SLOTS {
            assert!(MAP_HTML.contains(slot), "map.html no longer has a `{slot}` slot");
            assert!(!asset.is_empty(), "`{slot}` has no asset behind it");
        }
        assert!(THREE_JS.len() > 100_000, "three.js looks truncated");
        assert!(APP_JS.contains("CML_GRAPH"), "app.js must read the payload the page defines");
    }

    #[test]
    fn wikilinks_are_the_edges_between_notes() {
        let found = wikilinks("see [[ Feedback-User-Authority ]] and [[b]] and [[");
        assert_eq!(found, ["feedback-user-authority", "b"], "trimmed, lowercased, and terminated");
        assert!(wikilinks(&format!("[[{}]]", "x".repeat(120))).is_empty(), "runaway link ignored");
        assert!(wikilinks("[[   ]]").is_empty(), "blank link ignored");
        assert!(wikilinks("no links here").is_empty());
    }

    /// A session id clipped by bytes would split a multi-byte character and label
    /// the node with a replacement glyph. (`stable_key` itself is `text.rs`'s.)
    #[test]
    fn take_chars_counts_characters_not_bytes() {
        assert_eq!(take_chars(&"é".repeat(80), 64).chars().count(), 64);
        assert_eq!(take_chars("2026-08-10T23:59", 10), "2026-08-10");
    }
}
