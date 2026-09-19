//! `cml relay --serve` — the smallest thing two people can point at.
//!
//! A mailbox, not a service. It accepts a blob addressed to a handle, holds it
//! on disk, hands it to whoever proves that handle is theirs, and forgets it.
//! No accounts, no registration, no database: a directory per handle, a file per
//! bundle. Run it on a VPS, a Pi, or a laptop on the same network.
//!
//! # What it deliberately does not do
//!
//! It does not read bundles, index them, or keep them after collection. It does
//! not know who you are beyond the handle you claim. The first `send` to a
//! handle binds it to the sender-supplied key; after that the handle is taken.
//! That is trust-on-first-use, which is weak against someone racing you to your
//! own handle and useless against someone who already has your key — and it is
//! the right amount of ceremony for a mailbox two friends share.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

use serde_json::json;

/// How long an uncollected bundle lives. A mailbox that grows forever is a
/// disk-full outage waiting for the day nobody is watching.
const KEEP_DAYS: i64 = 14;

/// The ceiling on one bundle. Big enough for a long session with tool output,
/// small enough that a stranger cannot fill the disk in one request.
const MAX_BYTES: usize = 64 * 1024 * 1024;

fn spool() -> PathBuf {
    std::env::var_os("CML_RELAY_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::db::home().join("relay-spool"))
}

pub fn serve(args: &[String]) -> crate::R<i32> {
    let port: u16 = crate::share::flag(args, "--port")
        .and_then(|v| v.parse().ok())
        .unwrap_or(8787);
    // 0.0.0.0 here, unlike `cml ui`: a mailbox nobody else can reach is not a
    // mailbox. That is the whole point of this command, so it is not a default
    // that leaked, it is the requirement.
    let listener = TcpListener::bind(("0.0.0.0", port))?;
    let dir = spool();
    std::fs::create_dir_all(&dir)?;

    println!("cml relay listening on 0.0.0.0:{port}");
    println!("spool: {}", dir.display());
    println!("\nthe other side runs:  cml relay --use http://<this-host>:{port}");
    println!("bundles are deleted after collection, or after {KEEP_DAYS} days.");

    sweep(&dir);
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        std::thread::spawn(move || {
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(60)));
            if let Err(e) = handle(stream) {
                let m = e.to_string();
                if !m.contains("Broken pipe") && !m.contains("timed out") {
                    eprintln!("cml relay: {m}");
                }
            }
        });
    }
    Ok(0)
}

fn handle(mut stream: TcpStream) -> crate::R {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(());
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();

    let mut len = 0usize;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
    }

    let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
    let q = crate::ui::parse_query(query);
    let dir = spool();

    let to = safe(q.get("to").map(String::as_str).unwrap_or_default());
    let key = q.get("key").cloned().unwrap_or_default();

    match (method.as_str(), path) {
        ("POST", "/send") => {
            let from = safe(q.get("from").map(String::as_str).unwrap_or_default());
            if to.is_empty() || from.is_empty() {
                return text(&mut stream, 400, "need ?from= and ?to=");
            }
            if len == 0 || len > MAX_BYTES {
                return text(&mut stream, 413, "bundle missing or too large");
            }
            let mut blob = vec![0u8; len];
            reader.read_exact(&mut blob)?;

            // The sender's key binds *their* handle, not the recipient's, so
            // that a stranger cannot post as someone the recipient trusts.
            if let Err(e) = claim(&dir, &from, &key) {
                return text(&mut stream, 403, &e);
            }

            let box_dir = dir.join(&to).join("in");
            std::fs::create_dir_all(&box_dir)?;
            let id = format!("{}-{}", crate::paths::now_secs(), blob.len());
            std::fs::write(box_dir.join(format!("{from}__{id}.cmlpack")), &blob)?;
            text(&mut stream, 200, "ok")
        }
        ("GET", "/inbox") => {
            if let Err(e) = owns(&dir, &to, &key) {
                return text(&mut stream, 403, &e);
            }
            let mut items = Vec::new();
            if let Ok(rd) = std::fs::read_dir(dir.join(&to).join("in")) {
                let mut files: Vec<_> = rd.flatten().map(|e| e.path()).collect();
                files.sort();
                for p in files {
                    let Some((from, id)) = split_name(&p) else { continue };
                    let bytes = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                    let at = id
                        .split('-')
                        .next()
                        .and_then(|s| s.parse::<i64>().ok())
                        .map(crate::harness::ts_from_secs)
                        .unwrap_or_default();
                    items.push(json!({"id": id, "from": from, "bytes": bytes, "at": at}));
                }
            }
            let body = json!({"items": items}).to_string();
            reply(&mut stream, 200, "application/json", body.as_bytes())
        }
        ("GET", "/pull") => {
            if let Err(e) = owns(&dir, &to, &key) {
                return text(&mut stream, 403, &e);
            }
            let want = safe(q.get("id").map(String::as_str).unwrap_or_default());
            let Some(path) = find(&dir.join(&to).join("in"), &want) else {
                return text(&mut stream, 404, "no such item");
            };
            let blob = std::fs::read(&path)?;
            // Collected means delivered. A mailbox that keeps a copy after
            // handing it over is a mailbox nobody should trust with a
            // conversation.
            let _ = std::fs::remove_file(&path);
            reply(&mut stream, 200, "application/octet-stream", &blob)
        }
        ("GET", "/") => text(&mut stream, 200, "cml relay\n"),
        _ => text(&mut stream, 404, "not found"),
    }
}

/// Trust on first use: the first key seen for a handle owns it thereafter.
fn claim(dir: &Path, handle: &str, key: &str) -> Result<(), String> {
    if key.len() < 16 {
        return Err("missing or too-short key".into());
    }
    let path = dir.join(handle).join("key");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        return if existing.trim() == key {
            Ok(())
        } else {
            Err(format!("handle '{handle}' is already claimed by another key"))
        };
    }
    let _ = std::fs::create_dir_all(dir.join(handle));
    std::fs::write(&path, key).map_err(|e| e.to_string())
}

fn owns(dir: &Path, handle: &str, key: &str) -> Result<(), String> {
    if handle.is_empty() || key.len() < 16 {
        return Err("need ?to= and ?key=".into());
    }
    match std::fs::read_to_string(dir.join(handle).join("key")) {
        // An unclaimed handle claims itself on first read, so a fresh recipient
        // can check an empty inbox before anyone has written to them.
        Err(_) => claim(dir, handle, key),
        Ok(k) if k.trim() == key => Ok(()),
        Ok(_) => Err("that key does not own this handle".into()),
    }
}

/// `from__id.cmlpack` -> (from, id).
fn split_name(p: &Path) -> Option<(String, String)> {
    let stem = p.file_stem()?.to_string_lossy().into_owned();
    let (from, id) = stem.split_once("__")?;
    Some((from.to_string(), id.to_string()))
}

fn find(dir: &Path, id: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| split_name(p).is_some_and(|(_, got)| got == id))
}

/// Drop anything nobody came for.
fn sweep(dir: &Path) {
    let cutoff = crate::paths::now_secs() - KEEP_DAYS * 86_400;
    let Ok(handles) = std::fs::read_dir(dir) else { return };
    for h in handles.flatten() {
        let Ok(files) = std::fs::read_dir(h.path().join("in")) else { continue };
        for f in files.flatten() {
            let stale = split_name(&f.path())
                .and_then(|(_, id)| id.split('-').next()?.parse::<i64>().ok())
                .is_some_and(|t| t < cutoff);
            if stale {
                let _ = std::fs::remove_file(f.path());
            }
        }
    }
}

/// A handle from the wire is a path component, so it is filtered, not trusted.
/// `../../etc` must never become a directory traversal.
fn safe(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(64)
        .collect()
}

fn text(stream: &mut TcpStream, code: u16, msg: &str) -> crate::R {
    reply(stream, code, "text/plain", msg.as_bytes())
}

fn reply(stream: &mut TcpStream, code: u16, mime: &str, body: &[u8]) -> crate::R {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Payload Too Large",
    };
    write!(
        stream,
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "cml-relay-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// The bug this prevents is a handle escaping its directory.
    #[test]
    fn a_handle_cannot_traverse_the_spool() {
        assert_eq!(safe("../../etc/passwd"), "etcpasswd");
        assert_eq!(safe("amber-fox-4821"), "amber-fox-4821");
        assert_eq!(safe("a b\0c"), "abc");
        assert_eq!(safe(&"x".repeat(200)).len(), 64);
    }

    #[test]
    fn first_key_claims_a_handle_and_a_second_is_refused() {
        let d = tmp("claim");
        let k1 = "0123456789abcdef0123";
        let k2 = "ffffffffffffffff9999";
        assert!(claim(&d, "amber-fox-1", k1).is_ok());
        assert!(claim(&d, "amber-fox-1", k1).is_ok(), "the same key stays valid");
        let e = claim(&d, "amber-fox-1", k2).unwrap_err();
        assert!(e.contains("already claimed"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ownership_is_required_to_read_an_inbox() {
        let d = tmp("own");
        let mine = "0123456789abcdef0123";
        claim(&d, "amber-fox-2", mine).unwrap();
        assert!(owns(&d, "amber-fox-2", mine).is_ok());
        assert!(owns(&d, "amber-fox-2", "ffffffffffffffff0000").is_err());
        // A short key is never a credential.
        assert!(owns(&d, "amber-fox-2", "abc").is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_fresh_handle_can_check_an_empty_inbox() {
        let d = tmp("fresh");
        assert!(
            owns(&d, "never-seen-9", "0123456789abcdef0123").is_ok(),
            "a new user must be able to look before anyone has written"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn names_round_trip_and_the_sweep_only_takes_stale_ones() {
        let d = tmp("sweep");
        let box_dir = d.join("me").join("in");
        std::fs::create_dir_all(&box_dir).unwrap();
        let old = crate::paths::now_secs() - (KEEP_DAYS + 1) * 86_400;
        let new = crate::paths::now_secs();
        std::fs::write(box_dir.join(format!("alice__{old}-10.cmlpack")), b"x").unwrap();
        std::fs::write(box_dir.join(format!("bob__{new}-10.cmlpack")), b"y").unwrap();

        let p = box_dir.join(format!("alice__{old}-10.cmlpack"));
        assert_eq!(split_name(&p).unwrap().0, "alice");

        sweep(&d);
        let left: Vec<_> = std::fs::read_dir(&box_dir).unwrap().flatten().collect();
        assert_eq!(left.len(), 1, "only the stale bundle goes");
        assert!(left[0].file_name().to_string_lossy().starts_with("bob__"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
