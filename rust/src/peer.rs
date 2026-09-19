//! Who you are, and how a chat reaches someone else without a file in between.
//!
//! Sharing worked and nobody would use it: export a bundle, find it in your
//! downloads, attach it to something, have them save it and remember a command.
//! Five steps and two of them happen outside the tool. This is the same thing
//! addressed to a name.
//!
//! # Two secrets, and only one of them is shared
//!
//! Your **handle** (`amber-fox-4821`) is public. You give it out; it is how
//! someone addresses a chat to you, and knowing it is exactly the permission to
//! send you something.
//!
//! Your **key** is not. It is what proves the inbox is yours when you collect.
//! A handle alone lets a stranger send to you, never read what others sent.
//! That asymmetry is the whole security model, and it is the right one: the
//! failure this must prevent is a third party *reading* your memory, not a
//! third party mailing you theirs.
//!
//! # The relay sees what you send it
//!
//! A bundle passes through whatever relay both sides point at. It is already
//! redacted before it leaves ([`crate::share::redact`]), but "redacted" is not
//! "encrypted", and this file does not pretend otherwise. `cml relay` exists so
//! the honest answer to "who is holding my conversation" can be "my own box".

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;

use serde_json::{json, Value};

/// Where the handle and key live. Not in the index: the index is disposable and
/// rebuilds from transcripts, and losing your identity on a reindex would mean
/// everyone who knows your handle can no longer reach you.
fn identity_path() -> PathBuf {
    crate::db::home().join("identity.json")
}

/// The relay both ends must agree on.
///
/// `CML_RELAY` wins so a pair of friends can point at their own box without
/// editing anything. There is no baked-in default host: a memory tool that
/// silently ships your conversations to an address you never chose would be
/// exactly the thing this project spent a README arguing against.
pub fn relay() -> Option<String> {
    if let Ok(v) = std::env::var("CML_RELAY") {
        let v = v.trim().trim_end_matches('/').to_string();
        if !v.is_empty() {
            return Some(v);
        }
    }
    let p = crate::db::home().join("relay");
    let v = std::fs::read_to_string(p).ok()?;
    let v = v.trim().trim_end_matches('/').to_string();
    (!v.is_empty()).then_some(v)
}

pub struct Identity {
    pub handle: String,
    pub key: String,
}

/// Read the identity, minting one on first use.
pub fn identity() -> crate::R<Identity> {
    let path = identity_path();
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            let handle = v["handle"].as_str().unwrap_or_default().to_string();
            let key = v["key"].as_str().unwrap_or_default().to_string();
            if !handle.is_empty() && !key.is_empty() {
                return Ok(Identity { handle, key });
            }
        }
    }
    let id = Identity { handle: mint_handle(), key: mint_key() };
    write_identity(&id)?;
    Ok(id)
}

fn write_identity(id: &Identity) -> crate::R {
    let path = identity_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        &path,
        json!({"handle": id.handle, "key": id.key}).to_string(),
    )?;
    // The key is a credential. On a shared box, 0600 is the difference between
    // "my inbox" and "anyone with a shell".
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Words, not hex. A handle gets read aloud, typed from memory, and pasted into
/// a chat window; `amber-fox-4821` survives all three and `7f3a91c2` does not.
fn mint_handle() -> String {
    const ADJ: [&str; 16] = [
        "amber", "quiet", "rapid", "clever", "iron", "solar", "lunar", "brave",
        "sharp", "still", "north", "vivid", "plain", "swift", "grave", "warm",
    ];
    const NOUN: [&str; 16] = [
        "fox", "heron", "otter", "cedar", "flint", "raven", "ember", "birch",
        "wren", "moth", "pike", "lynx", "reed", "hawk", "vole", "kite",
    ];
    let n = seed();
    format!(
        "{}-{}-{:04}",
        ADJ[(n % 16) as usize],
        NOUN[((n / 16) % 16) as usize],
        (n / 256) % 10_000
    )
}

fn mint_key() -> String {
    // Two clock reads and the pid, hashed the same way the rest of this crate
    // hashes things. Not a CSPRNG, and the threat is a stranger guessing a key
    // to read one redacted inbox, not a funded attacker.
    let a = seed();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let b = seed();
    format!("{:016x}{:016x}", hash64(a), hash64(b ^ 0x9e37_79b9_7f4a_7c15))
}

fn seed() -> u64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    t ^ (u64::from(std::process::id()) << 32)
}

fn hash64(mut x: u64) -> u64 {
    // splitmix64: short, and it actually mixes, which a wrapping multiply alone
    // does not.
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// cml whoami [--set NAME]
pub fn whoami(args: &[String]) -> crate::R<i32> {
    let mut id = identity()?;
    if let Some(name) = crate::share::flag(args, "--set") {
        let clean = clean_handle(&name);
        if clean.len() < 3 {
            return Err("a handle needs at least 3 letters, digits or dashes".into());
        }
        id.handle = clean;
        write_identity(&id)?;
    }
    println!("{}", id.handle);
    if args.iter().any(|a| a == "--full") {
        println!("key:   {}", id.key);
        println!(
            "relay: {}",
            relay().unwrap_or_else(|| "(none set — cml relay --use <url>)".into())
        );
    } else {
        println!("\ngive that handle to anyone who should be able to send you a chat.");
        println!("your key stays here and is what makes `cml inbox` yours.");
    }
    Ok(0)
}

fn clean_handle(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(40)
        .collect()
}

/// cml send <handle> [--chat N | --session ID] [--dry-run]
pub fn send(args: &[String]) -> crate::R<i32> {
    let Some(to) = args.iter().find(|a| !a.starts_with("--")) else {
        return Err("usage: cml send <handle> --chat <#>   (see `cml chats`)".into());
    };
    let to = clean_handle(to);
    let Some(relay) = relay() else {
        return Err(
            "no relay set. `cml relay --use https://host:port`, or run your own \
             with `cml relay --serve`"
                .into(),
        );
    };
    let me = identity()?;

    // Resolve the chat exactly the way `cml share` does, so a number means the
    // same row in both commands.
    let session = match crate::share::flag(args, "--session") {
        Some(s) => s,
        None => {
            let n = crate::share::flag(args, "--chat")
                .and_then(|v| v.parse::<usize>().ok())
                .ok_or("which chat? run `cml chats`, then `cml send <handle> --chat <#>`")?;
            let conn = crate::db::open_ro()?;
            let chat = crate::chats::nth(&conn, n, None)?;
            println!(
                "chat #{n}: {} · {} · {} rows\n  {}\n",
                chat.last.get(..10).unwrap_or("-"),
                chat.project,
                chat.rows,
                chat.opener
            );
            chat.session
        }
    };

    // Build the bundle through the ordinary path: redaction is not optional and
    // not reimplemented here.
    let tmp = std::env::temp_dir().join(format!("cml-send-{}.cmlpack", std::process::id()));
    let tmp_s = tmp.to_string_lossy().into_owned();
    let mut share_args: Vec<String> = vec![
        "--session".into(),
        session,
        "--to".into(),
        me.handle.clone(),
        "--out".into(),
        tmp_s.clone(),
    ];
    if args.iter().any(|a| a == "--dry-run") {
        share_args.push("--dry-run".into());
        crate::share::share(&share_args)?;
        println!("\nnothing was sent. drop --dry-run to deliver it to {to}.");
        return Ok(0);
    }
    crate::share::share(&share_args)?;

    let blob = std::fs::read(&tmp)?;
    let n = blob.len();
    let out = post_bundle(&relay, &me, &to, &blob);
    let _ = std::fs::remove_file(&tmp);
    out?;

    println!("\ndelivered to {to} ({} KB)", n / 1024);
    println!("they run:  cml inbox");
    Ok(0)
}

/// cml inbox [--get N] [--all]
pub fn inbox(args: &[String]) -> crate::R<i32> {
    let Some(relay) = relay() else {
        return Err("no relay set. `cml relay --use <url>`".into());
    };
    let me = identity()?;
    let listing = get_json(&relay, &format!("/inbox?to={}&key={}", me.handle, me.key))?;
    let items = listing["items"].as_array().cloned().unwrap_or_default();

    if items.is_empty() {
        println!("nothing waiting for {}.", me.handle);
        println!("give that handle to whoever should send you a chat.");
        return Ok(0);
    }

    let want = crate::share::flag(args, "--get").and_then(|v| v.parse::<usize>().ok());
    let all = args.iter().any(|a| a == "--all");

    if want.is_none() && !all {
        println!("{:>3}  {:<18}  {:>8}  sent", "#", "from", "size");
        for (i, it) in items.iter().enumerate() {
            println!(
                "{:>3}  {:<18}  {:>7}K  {}",
                i + 1,
                it["from"].as_str().unwrap_or("?"),
                it["bytes"].as_i64().unwrap_or(0) / 1024,
                it["at"].as_str().unwrap_or("")
            );
        }
        println!("\ntake one:  cml inbox --get <#>        take all:  cml inbox --all");
        return Ok(0);
    }

    let chosen: Vec<&Value> = match want {
        Some(n) if n >= 1 && n <= items.len() => vec![&items[n - 1]],
        Some(n) => return Err(format!("no item #{n} — {} waiting", items.len()).into()),
        None => items.iter().collect(),
    };

    for it in chosen {
        let id = it["id"].as_str().unwrap_or_default();
        let from = it["from"].as_str().unwrap_or("peer");
        let blob = get_bytes(&relay, &format!("/pull?to={}&key={}&id={id}", me.handle, me.key))?;
        let tmp = std::env::temp_dir().join(format!("cml-inbox-{from}-{id}.cmlpack"));
        std::fs::write(&tmp, &blob)?;
        // Import through the ordinary command: dedup, attribution and
        // `forget --from` all come with it for free.
        crate::share::import(&[tmp.to_string_lossy().into_owned(), "--as".into(), from.into()])?;
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(0)
}

/// cml relay --use <url> | --serve [--port N] | (bare: print the current one)
pub fn relay_cmd(args: &[String]) -> crate::R<i32> {
    if let Some(url) = crate::share::flag(args, "--use") {
        let url = url.trim().trim_end_matches('/').to_string();
        let p = crate::db::home().join("relay");
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&p, &url)?;
        println!("relay set: {url}");
        println!("your handle: {}", identity()?.handle);
        return Ok(0);
    }
    if args.iter().any(|a| a == "--serve") {
        return crate::relay::serve(args);
    }
    match relay() {
        Some(r) => println!("{r}"),
        None => {
            println!("no relay set.");
            println!("  point at one:  cml relay --use http://host:8787");
            println!("  or run one:    cml relay --serve --port 8787");
        }
    }
    Ok(0)
}

// ----------------------------------------------------------------- transport
//
// Hand-rolled HTTP again, and for the same reason `ui.rs` is: three requests
// against one host. `reqwest` would pull tokio, hyper, rustls and a hundred
// crates behind them into a binary whose entire dependency list is three names.

#[derive(Debug)]
struct Url {
    host: String,
    port: u16,
    path: String,
}

impl Url {
    /// The path a route hangs off, with no trailing slash and never empty of
    /// meaning: a bare `http://host` has prefix `""`, so `{prefix}/send` is
    /// `/send`, while `http://host/cml` gives `/cml/send`.
    ///
    /// Trimming the slash off `/` used to leave `""` and the request line then
    /// read `POST send?... HTTP/1.1` with no leading slash, which every server
    /// answers 404. That was the whole bug.
    fn prefix(&self) -> &str {
        self.path.trim_end_matches('/')
    }
}

fn split_url(u: &str) -> crate::R<Url> {
    let (scheme, rest) = u.split_once("://").unwrap_or(("http", u));
    let tls = scheme == "https";
    let (hostport, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => {
            (h.to_string(), p.parse().unwrap_or(80))
        }
        _ => (hostport.to_string(), if tls { 443 } else { 80 }),
    };
    if tls {
        // Honest failure beats a silent downgrade to plaintext. This client
        // speaks no TLS, so an https URL must stop here rather than quietly
        // send a conversation in the clear to port 443.
        return Err(
            "https relays need a TLS terminator in front (nginx/caddy); point cml at \
             the http address behind it, or use http:// on a private network"
                .into(),
        );
    }
    Ok(Url { host, port, path: format!("/{path}") })
}

fn connect(u: &Url) -> crate::R<TcpStream> {
    let s = TcpStream::connect((u.host.as_str(), u.port))
        .map_err(|e| format!("cannot reach {}:{} ({e})", u.host, u.port))?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
    s.set_write_timeout(Some(std::time::Duration::from_secs(30)))?;
    Ok(s)
}

fn read_response(mut s: TcpStream) -> crate::R<(u16, Vec<u8>)> {
    let mut reader = BufReader::new(&mut s);
    let mut status = String::new();
    reader.read_line(&mut status)?;
    let code: u16 = status
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
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
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    Ok((code, body))
}

fn post_bundle(relay: &str, me: &Identity, to: &str, blob: &[u8]) -> crate::R {
    let base = split_url(relay)?;
    let path = format!(
        "{}/send?from={}&to={to}&key={}",
        base.prefix(),
        me.handle,
        me.key
    );
    let mut s = connect(&base)?;
    write!(
        s,
        "POST {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/octet-stream\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        base.host,
        blob.len()
    )?;
    s.write_all(blob)?;
    s.flush()?;
    let (code, body) = read_response(s)?;
    if code != 200 {
        return Err(format!(
            "relay refused it ({code}): {}",
            String::from_utf8_lossy(&body).trim()
        )
        .into());
    }
    Ok(())
}

fn get_bytes(relay: &str, path: &str) -> crate::R<Vec<u8>> {
    let base = split_url(relay)?;
    let full = format!("{}{}", base.prefix(), path);
    let mut s = connect(&base)?;
    write!(
        s,
        "GET {full} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        base.host
    )?;
    s.flush()?;
    let (code, body) = read_response(s)?;
    if code != 200 {
        return Err(format!(
            "relay said {code}: {}",
            String::from_utf8_lossy(&body).trim()
        )
        .into());
    }
    Ok(body)
}

fn get_json(relay: &str, path: &str) -> crate::R<Value> {
    let body = get_bytes(relay, path)?;
    Ok(serde_json::from_slice(&body).unwrap_or_else(|_| json!({"items": []})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handle_is_readable_and_typeable() {
        let h = mint_handle();
        let parts: Vec<&str> = h.split('-').collect();
        assert_eq!(parts.len(), 3, "{h}");
        assert_eq!(parts[2].len(), 4, "the number is padded: {h}");
        assert!(
            h.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "a handle gets typed from memory: {h}"
        );
    }

    #[test]
    fn a_key_is_long_and_not_the_handle() {
        let k = mint_key();
        assert_eq!(k.len(), 32);
        assert!(k.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(mint_key(), mint_key(), "two mints must not collide");
    }

    #[test]
    fn handles_are_cleaned_not_trusted() {
        assert_eq!(clean_handle("  Amber-Fox-4821 "), "amber-fox-4821");
        // The characters that would let a handle escape a query string.
        assert_eq!(clean_handle("a/b?c=d&e"), "abcde");
        assert_eq!(clean_handle("../../etc/passwd"), "etcpasswd");
        assert!(clean_handle("!!!").is_empty());
    }

    /// The 404 bug: `/` trimmed to `""`, so the request line lost its leading
    /// slash and every route missed.
    #[test]
    fn a_bare_host_still_builds_an_absolute_path() {
        assert_eq!(split_url("http://box:8787").unwrap().prefix(), "");
        assert_eq!(split_url("http://box:8787/").unwrap().prefix(), "");
        assert_eq!(split_url("http://box:8787/cml").unwrap().prefix(), "/cml");
        // What the caller actually writes on the wire.
        let u = split_url("http://box:8787").unwrap();
        assert_eq!(format!("{}/send", u.prefix()), "/send");
    }

    #[test]
    fn urls_split_into_host_port_and_path() {
        let u = split_url("http://box.local:8787").unwrap();
        assert_eq!(u.host, "box.local");
        assert_eq!(u.port, 8787);
        assert_eq!(u.path, "/");

        let u = split_url("http://1.2.3.4").unwrap();
        assert_eq!(u.port, 80, "http defaults to 80");

        let u = split_url("http://box:8787/cml").unwrap();
        assert_eq!(u.path, "/cml");
    }

    /// A silent downgrade to plaintext on an https URL would be the worst kind
    /// of bug: the user asked for TLS and got none, with no error to notice.
    #[test]
    fn https_is_refused_rather_than_silently_downgraded() {
        let e = split_url("https://box.example").unwrap_err().to_string();
        assert!(e.contains("TLS"), "{e}");
    }
}
