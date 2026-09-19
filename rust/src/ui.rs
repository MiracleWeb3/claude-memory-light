//! `cml ui` — the share window, served to whatever browser this machine has.
//!
//! A GTK window would have been Linux-only, and this tool now indexes four
//! harnesses across three operating systems. The browser is the one UI toolkit
//! every target already has, so the binary serves one page to it and exits when
//! the tab closes.
//!
//! # Hand-rolled HTTP, and why that is not madness here
//!
//! The whole surface is four routes on `127.0.0.1`, single-user, no
//! concurrency to speak of, no TLS, no uploads. `axum` and its tokio tree would
//! be an order of magnitude more dependency than the three this crate has, to
//! parse a request line this file handles in twenty. The repo's whole claim is
//! one small binary; a web framework would retire that claim to save an
//! afternoon.
//!
//! # What keeps it safe
//!
//! Bound to loopback only, never `0.0.0.0`. Every request must carry a token
//! minted at startup and printed in the URL, so another process on the machine
//! cannot drive your memory by guessing a port. The page is embedded in the
//! binary rather than read from disk, so there is no path to traverse.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};

use serde_json::{json, Value};

/// The page, compiled in. No CDN, no build step, no file to lose.
const PAGE: &str = include_str!("ui/share.html");

/// cml ui [--port N] [--no-open]
pub fn run(args: &[String]) -> crate::R<i32> {
    let port: u16 = crate::share::flag(args, "--port")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    // Port 0 lets the OS pick a free one, which is the difference between
    // "works" and "works unless something else took 7777".
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
    let port = listener.local_addr()?.port();
    let token = token();
    let url = format!("http://127.0.0.1:{port}/?k={token}");

    println!("cml ui  →  {url}");
    println!("(this window serves only your own machine; close the tab or press ctrl-c)");

    if !args.iter().any(|a| a == "--no-open") {
        open_browser(&url);
    }

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let token = token.clone();
        // One thread per connection, which a serial loop cannot be replaced by.
        //
        // Browsers open a second, speculative connection alongside the first and
        // then send nothing on it. A serial accept loop blocks inside that
        // socket's first `read_line` forever, and the page hangs on its loading
        // skeleton while `curl` against the same server works perfectly - which
        // is exactly how this presented. The read timeout below is the other
        // half: it reaps the speculative socket instead of leaking a thread that
        // waits for a request that is never coming.
        std::thread::spawn(move || {
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(15)));
            if let Err(e) = serve(stream, &token) {
                // A dead socket is the normal end of a page load, not news.
                let msg = e.to_string();
                if !msg.contains("Broken pipe") && !msg.contains("timed out") {
                    eprintln!("cml ui: {msg}");
                }
            }
        });
    }
    Ok(0)
}

/// A URL token, from the clock and the pid.
///
/// Not a CSPRNG: this defends against another *process* stumbling onto the
/// port, not against an attacker who can already read your memory index and
/// therefore has everything the page could show them.
fn token() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{:x}{:x}", t, std::process::id())
}

fn open_browser(url: &str) {
    // `CML_BROWSER` first, because "my browser" is a preference the OS default
    // routinely gets wrong: a machine can have four Chrome profiles and only
    // one of them is the user's. The value is a command line, so the profile
    // flag rides along with it:
    //
    //   CML_BROWSER='google-chrome --profile-directory="Profile 1"'
    //
    if let Ok(spec) = std::env::var("CML_BROWSER") {
        let mut parts = split_args(&spec).into_iter();
        if let Some(bin) = parts.next() {
            let args: Vec<String> = parts.collect();
            if std::process::Command::new(bin)
                .args(&args)
                .arg(url)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .is_ok()
            {
                return;
            }
        }
    }
    // One of these exists on each target; the rest fail silently and the user
    // still has the printed URL.
    for (cmd, args) in [
        ("xdg-open", vec![url]),
        ("open", vec![url]),
        ("cmd", vec!["/C", "start", url]),
    ] {
        if std::process::Command::new(cmd)
            .args(&args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok()
        {
            return;
        }
    }
}

/// Split a command line on spaces, except inside quotes.
///
/// A plain `split_whitespace` would tear `--profile-directory="Profile 1"` in
/// half and launch the wrong Chrome profile — which is precisely the setting
/// this variable exists to get right. Not a shell: no expansion, no escapes,
/// no operators. Quotes group, and that is all.
fn split_args(spec: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in spec.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => quote = Some(c),
            (None, c) if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn serve(mut stream: TcpStream, token: &str) -> crate::R {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(());
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();

    // Headers, only for the content length the POST body needs.
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
    let params = parse_query(query);

    // The page itself is public on loopback; everything that reads or writes
    // memory is not.
    if path != "/" && params.get("k").map(String::as_str) != Some(token) {
        return reply(&mut stream, 403, "text/plain", b"forbidden");
    }

    match (method.as_str(), path) {
        ("GET", "/") => reply(&mut stream, 200, "text/html; charset=utf-8", PAGE.as_bytes()),
        ("GET", "/api/me") => {
            let body = match crate::peer::identity() {
                Ok(id) => json!({
                    "handle": id.handle,
                    "relay": crate::peer::relay(),
                })
                .to_string(),
                Err(e) => json!({"error": e.to_string()}).to_string(),
            };
            reply(&mut stream, 200, "application/json", body.as_bytes())
        }
        ("GET", "/api/inbox") => {
            let body = match cli(&["inbox"]) {
                Ok(text) => json!({"text": text}).to_string(),
                Err(e) => json!({"error": e}).to_string(),
            };
            reply(&mut stream, 200, "application/json", body.as_bytes())
        }
        ("GET", "/api/chats") => {
            let out = cli(&["chats", "--limit", "400", "--json"]);
            let body = out.unwrap_or_else(|e| json!({"error": e}).to_string());
            reply(&mut stream, 200, "application/json", body.as_bytes())
        }
        ("GET", "/api/dry") => {
            let Some(session) = params.get("session") else {
                return reply(&mut stream, 400, "application/json", b"{\"error\":\"no session\"}");
            };
            let body = match cli(&["share", "--session", session, "--dry-run"]) {
                Ok(text) => dry_json(&text).to_string(),
                Err(e) => json!({"error": e}).to_string(),
            };
            reply(&mut stream, 200, "application/json", body.as_bytes())
        }
        ("POST", "/api/send") => {
            let mut buf = vec![0u8; len];
            reader.read_exact(&mut buf)?;
            let req: Value = serde_json::from_slice(&buf).unwrap_or_else(|_| json!({}));
            let session = req["session"].as_str().unwrap_or_default().to_string();
            let to = req["to"].as_str().unwrap_or_default().trim().to_string();
            let body = if to.is_empty() {
                json!({"ok": false, "error": "who to? type their handle"}).to_string()
            } else {
                match cli(&["send", &to, "--session", &session]) {
                    Ok(text) => json!({"ok": true, "to": to, "log": text}).to_string(),
                    Err(e) => json!({"ok": false, "error": e}).to_string(),
                }
            };
            reply(&mut stream, 200, "application/json", body.as_bytes())
        }
        ("POST", "/api/relay") => {
            let mut buf = vec![0u8; len];
            reader.read_exact(&mut buf)?;
            let req: Value = serde_json::from_slice(&buf).unwrap_or_else(|_| json!({}));
            let url = req["url"].as_str().unwrap_or_default().trim().to_string();
            let body = match cli(&["relay", "--use", &url]) {
                Ok(_) => json!({"ok": true}).to_string(),
                Err(e) => json!({"ok": false, "error": e}).to_string(),
            };
            reply(&mut stream, 200, "application/json", body.as_bytes())
        }
        ("POST", "/api/share") => {
            let mut buf = vec![0u8; len];
            reader.read_exact(&mut buf)?;
            let req: Value = serde_json::from_slice(&buf).unwrap_or_else(|_| json!({}));
            let session = req["session"].as_str().unwrap_or_default().to_string();
            let who = match req["who"].as_str().unwrap_or("").trim() {
                "" => "me".to_string(),
                w => w.to_string(),
            };
            let out = req["out"].as_str().unwrap_or_default().to_string();
            let body = match cli(&["share", "--session", &session, "--to", &who, "--out", &out]) {
                Ok(text) => {
                    let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or_default();
                    json!({"ok": true, "path": out, "bytes": size, "log": text}).to_string()
                }
                Err(e) => json!({"ok": false, "error": e}).to_string(),
            };
            reply(&mut stream, 200, "application/json", body.as_bytes())
        }
        _ => reply(&mut stream, 404, "text/plain", b"not found"),
    }
}

/// Run this same binary's own subcommand, in-process.
///
/// The page never sees SQL. Every number it shows came from the CLI the user
/// could have typed, which is what stops the UI and the terminal from ever
/// disagreeing about what is in the index.
fn cli(args: &[&str]) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let out = std::process::Command::new(exe)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// `cml share --dry-run` prose -> the numbers the page draws.
///
/// Parsed here rather than in JavaScript so the page holds no knowledge of the
/// CLI's wording, and a reworded line breaks one Rust test instead of silently
/// showing "0 secrets found" in a window whose whole job is that number.
fn dry_json(text: &str) -> Value {
    let mut rows = 0i64;
    let mut talk = 0i64;
    let mut tool = 0i64;
    let mut found: Vec<Value> = Vec::new();

    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("would share ") {
            rows = rest
                .split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
        } else if let Some(rest) = t.strip_prefix("lanes: ") {
            let mut it = rest.split(',');
            talk = leading_number(it.next().unwrap_or_default());
            tool = leading_number(it.next().unwrap_or_default());
        } else if t.starts_with(char::is_numeric) {
            // "     141  vendor api key"
            let n = leading_number(t);
            let what = t.trim_start_matches(char::is_numeric).trim();
            if n > 0 && !what.is_empty() {
                found.push(json!({"n": n, "what": what}));
            }
        }
    }
    json!({"rows": rows, "talk": talk, "tool": tool, "found": found})
}

fn leading_number(s: &str) -> i64 {
    s.split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

pub fn parse_query(q: &str) -> std::collections::HashMap<String, String> {
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), percent_decode(v)))
        .collect()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn reply(stream: &mut TcpStream, code: u16, mime: &str, body: &[u8]) -> crate::R {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        _ => "Not Found",
    };
    write!(
        stream,
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dry_run_prose_becomes_numbers() {
        let text = "\
would share 19721 row(s) as 'me' -> x.cmlpack
  lanes: 441 conversation, 19280 tool
  redaction: 8690 replacement(s)
     141  vendor api key
     117  authorization header
    8357  home path

nothing was written. drop --dry-run to make the file.";
        let v = dry_json(text);
        assert_eq!(v["rows"], 19721);
        assert_eq!(v["talk"], 441);
        assert_eq!(v["tool"], 19280);
        // The `redaction: 8690 replacement(s)` summary line must not become a
        // detector class of its own; only the itemised lines below it count.
        let found = v["found"].as_array().unwrap();
        assert_eq!(found.len(), 3, "only the itemised lines, not the summary: {found:?}");
        assert_eq!(found[0]["n"], 141);
        assert_eq!(found[0]["what"], "vendor api key");
        assert_eq!(found[2]["n"], 8357);
        assert_eq!(found[2]["what"], "home path");
    }

    #[test]
    fn a_clean_chat_reports_no_findings() {
        let v = dry_json("would share 12 row(s) as 'me' -> y.cmlpack\n  lanes: 12 conversation, 0 tool\n  redaction: nothing matched\n");
        assert_eq!(v["rows"], 12);
        assert!(v["found"].as_array().unwrap().is_empty());
    }

    /// The setting exists to launch the right Chrome profile, and
    /// `--profile-directory="Profile 1"` has a space inside one argument.
    /// A naive whitespace split would open the wrong profile every time.
    #[test]
    fn a_browser_command_keeps_quoted_arguments_whole() {
        let got = split_args(r#"google-chrome --profile-directory="Profile 1""#);
        assert_eq!(got, vec!["google-chrome", "--profile-directory=Profile 1"]);

        assert_eq!(split_args("firefox"), vec!["firefox"]);
        assert_eq!(split_args("  spaced   out  "), vec!["spaced", "out"]);
        assert_eq!(split_args("a 'b c' d"), vec!["a", "b c", "d"]);
        assert!(split_args("").is_empty());
    }

    #[test]
    fn query_strings_decode_including_escapes() {
        let p = parse_query("k=abc&session=a%2Db&name=two+words");
        assert_eq!(p["k"], "abc");
        assert_eq!(p["session"], "a-b");
        assert_eq!(p["name"], "two words");
        assert!(parse_query("").is_empty());
    }

    /// The page is embedded, so it can never be missing at runtime, and the
    /// test that proves it is the one that would fail at compile time anyway.
    #[test]
    fn the_page_is_compiled_in_and_complete() {
        assert!(PAGE.contains("<!doctype html>"), "the page must be a document");
        assert!(PAGE.contains("/api/chats"), "the page must call the api");
        assert!(!PAGE.contains("http://"), "no CDN: everything ships in the file");
    }
}
