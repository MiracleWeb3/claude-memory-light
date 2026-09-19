//! What must not leave the machine.
//!
//! A shared conversation is a transcript of real work, and real work touches
//! credentials. The rows being exported were never written with an audience in
//! mind: an `.env` was catted, a token appeared in a curl, an error message
//! quoted an Authorization header. This pass runs over the *copy* that leaves,
//! never over the local index.
//!
//! # No regex crate
//!
//! Every detector here is a prefix scan over ASCII with a shape check. `regex`
//! is 1.5 MB of dependency for patterns that are all "a known marker, then N
//! characters from a known alphabet" — which is a loop. The one genuinely
//! fuzzy case, an opaque high-entropy blob, is not a regex problem at all.
//!
//! # The false-positive budget
//!
//! A redactor that eats git hashes and base64 images is one the user turns off,
//! and a redactor that is off protects nothing. So the entropy fallback only
//! fires on a token that is *assigned* (`KEY=value`, `"token": "value"`), and
//! the fixed detectors all require a vendor marker. Hashes, UUIDs and minified
//! JS survive on purpose, and there is a test that says so.

/// What a scrub found, per class, so `--dry-run` can report it.
#[derive(Debug, Default, Clone, Copy)]
pub struct Report {
    pub vendor_keys: usize,
    pub private_keys: usize,
    pub jwts: usize,
    pub auth_headers: usize,
    pub assigned_secrets: usize,
    pub home_paths: usize,
}

impl Report {
    pub fn total(&self) -> usize {
        self.vendor_keys
            + self.private_keys
            + self.jwts
            + self.auth_headers
            + self.assigned_secrets
            + self.home_paths
    }

    pub fn print(&self) {
        if self.total() == 0 {
            println!("  redaction: nothing matched");
            return;
        }
        println!("  redaction: {} replacement(s)", self.total());
        for (n, what) in [
            (self.vendor_keys, "vendor api key"),
            (self.private_keys, "private key block"),
            (self.jwts, "jwt"),
            (self.auth_headers, "authorization header"),
            (self.assigned_secrets, "assigned high-entropy value"),
            (self.home_paths, "home path"),
        ] {
            if n > 0 {
                println!("    {n:>4}  {what}");
            }
        }
    }
}

const MASK: &str = "[redacted]";

/// Vendor markers. Each is a literal prefix followed by an opaque tail; the
/// tail's alphabet is what bounds the cut.
const VENDOR: &[&str] = &[
    "sk-ant-",     // Anthropic
    "sk-proj-",    // OpenAI project
    "sk-",         // OpenAI classic
    "AKIA",        // AWS access key id
    "ASIA",        // AWS temporary
    "ghp_",        // GitHub personal
    "gho_",        // GitHub oauth
    "ghs_",        // GitHub server
    "ghu_",        // GitHub user
    "github_pat_", // GitHub fine-grained
    "glpat-",      // GitLab
    "xoxb-",       // Slack bot
    "xoxp-",       // Slack user
    "AIza",        // Google
    "hf_",         // HuggingFace
    "shpat_",      // Shopify
];

/// Scrub in place, counting what was replaced.
pub fn scrub(text: &mut String, report: &mut Report) {
    if text.is_empty() {
        return;
    }
    let mut s = std::mem::take(text);
    s = private_key_blocks(s, report);
    s = vendor_keys(&s, report);
    s = jwts(&s, report);
    s = auth_headers(&s, report);
    s = assigned_secrets(&s, report);
    s = home_paths(&s, report);
    *text = s;
}

/// A PEM block is the one case where the marker is a whole line and the secret
/// is everything up to the matching footer.
fn private_key_blocks(s: String, report: &mut Report) -> String {
    const BEGIN: &str = "-----BEGIN";
    const END: &str = "-----END";
    if !s.contains(BEGIN) || !s.contains("PRIVATE KEY") {
        return s;
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    while let Some(start) = rest.find(BEGIN) {
        // Only a *private* key block; a certificate is not a secret.
        let header_end = rest[start..].find('\n').map_or(rest.len(), |i| start + i);
        if !rest[start..header_end].contains("PRIVATE KEY") {
            out.push_str(&rest[..header_end]);
            rest = &rest[header_end..];
            continue;
        }
        out.push_str(&rest[..start]);
        out.push_str(MASK);
        report.private_keys += 1;
        rest = match rest[start..].find(END) {
            Some(i) => {
                let after = start + i;
                let tail = rest[after..].find('\n').map_or(rest.len(), |j| after + j + 1);
                &rest[tail..]
            }
            // An unterminated block: everything after it is the key.
            None => "",
        };
    }
    out.push_str(rest);
    out
}

fn vendor_keys(s: &str, report: &mut Report) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let b = s.as_bytes();
    'outer: while i < b.len() {
        for marker in VENDOR {
            if !s.is_char_boundary(i) || !s[i..].starts_with(marker) {
                continue;
            }
            let tail = token_len(&b[i + marker.len()..]);
            // A marker with nothing opaque after it is prose, not a key.
            if tail < 12 {
                continue;
            }
            out.push_str(MASK);
            report.vendor_keys += 1;
            i += marker.len() + tail;
            continue 'outer;
        }
        let ch = next_char(s, i);
        out.push_str(ch);
        i += ch.len();
    }
    out
}

/// `header.payload.signature`, all base64url, each part long enough to be real.
fn jwts(s: &str, report: &mut Report) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s.is_char_boundary(i) && s[i..].starts_with("eyJ") {
            let len = token_len(&s.as_bytes()[i..]);
            let cand = &s[i..i + len];
            if cand.matches('.').count() == 2 && cand.split('.').all(|p| p.len() >= 8) {
                out.push_str(MASK);
                report.jwts += 1;
                i += len;
                continue;
            }
        }
        let ch = next_char(s, i);
        out.push_str(ch);
        i += ch.len();
    }
    out
}

/// `Authorization: Bearer <anything>` — the value is the secret whatever its
/// shape, so the cut runs to end of line.
fn auth_headers(s: &str, report: &mut Report) -> String {
    let mut out = String::with_capacity(s.len());
    for (n, line) in s.split_inclusive('\n').enumerate() {
        let lower = line.to_ascii_lowercase();
        let hit = ["authorization:", "authorization\":", "x-api-key:", "proxy-authorization:"]
            .iter()
            .find_map(|m| lower.find(m).map(|i| i + m.len()));
        match hit {
            Some(cut) if line[cut..].trim().len() > 3 => {
                out.push_str(&line[..cut]);
                out.push(' ');
                out.push_str(MASK);
                if line.ends_with('\n') {
                    out.push('\n');
                }
                report.auth_headers += 1;
            }
            _ => out.push_str(line),
        }
        let _ = n;
    }
    out
}

/// `KEY=value` / `"key": "value"` where the key *names* a secret and the value
/// is opaque. Both halves are required: `PATH=/usr/bin` keeps its value, and a
/// random base64 blob with no key naming it is left alone.
///
/// The value is the first whitespace-delimited token after the separator, not
/// the rest of the line. A transcript is prose with assignments *embedded* in
/// it — "cat .env showed DB_PASSWORD=hunter2 in ~/app" — and taking the rest of
/// the line would make the value contain a space, which `opaque` rejects, so
/// the secret would sail straight through. That was a real leak, caught by the
/// end-to-end test.
fn assigned_secrets(s: &str, report: &mut Report) -> String {
    const NAMES: &[&str] = &[
        "secret", "token", "password", "passwd", "apikey", "api_key", "access_key",
        "private_key", "client_secret", "auth", "credential",
    ];
    let mut out = String::with_capacity(s.len());
    for line in s.split_inclusive('\n') {
        let mut rest = line;
        // A line can carry more than one assignment, and the first separator on
        // it is often innocent (`config:`), so this walks every one.
        while let Some(eq) = rest.find(['=', ':']) {
            // The key is the word immediately before the separator, not the
            // whole prefix: "cat .env showed DB_PASSWORD" is one word here.
            // The trailing quote of a JSON key (`"api_token":`) is stripped
            // first, or rsplit on `"` would yield an empty word and every
            // JSON-shaped secret would walk straight through.
            let key_word = rest[..eq]
                .trim_end_matches(['"', '\'', ' ', '\t'])
                .rsplit([' ', '\t', '"', '\'', ',', '{'])
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();

            // Everything up to and including the separator is kept either way.
            out.push_str(&rest[..=eq]);
            rest = &rest[eq + 1..];

            if !NAMES.iter().any(|n| key_word.contains(n)) {
                continue;
            }
            // Skip the quoting and whitespace between `:` and the value, then
            // take one token. The rest of the line is prose, not the secret.
            let lead = rest.len() - rest.trim_start_matches(['"', '\'', ' ']).len();
            let after_lead = &rest[lead..];
            let taken = after_lead
                .find([' ', '\t', '\n', '"', '\'', ',', '}'])
                .unwrap_or(after_lead.len());
            let value = &after_lead[..taken];

            if value.len() >= 12 && opaque(value) {
                out.push_str(&rest[..lead]);
                out.push_str(MASK);
                report.assigned_secrets += 1;
                rest = &after_lead[taken..];
            }
        }
        out.push_str(rest);
    }
    out
}

/// `/home/alice/...` -> `~/...`. Not a secret, but it is a name, and a shared
/// transcript full of a stranger's absolute paths is the pollution that makes
/// people stop importing.
fn home_paths(s: &str, report: &mut Report) -> String {
    let home = crate::paths::home_dir().to_string_lossy().into_owned();
    let mut out = s.to_string();
    if !home.is_empty() && home != "/" && out.contains(&home) {
        report.home_paths += out.matches(&home).count();
        out = out.replace(&home, "~");
    }
    // Other people's homes, by shape rather than by name.
    for root in ["/home/", "/Users/"] {
        let mut result = String::with_capacity(out.len());
        let mut rest = out.as_str();
        while let Some(i) = rest.find(root) {
            let after = i + root.len();
            let name_len = rest[after..]
                .find(['/', ' ', '"', '\'', ':', '\n'])
                .unwrap_or(rest.len() - after);
            if name_len == 0 || name_len > 32 {
                result.push_str(&rest[..after]);
                rest = &rest[after..];
                continue;
            }
            result.push_str(&rest[..i]);
            result.push('~');
            report.home_paths += 1;
            rest = &rest[after + name_len..];
        }
        result.push_str(rest);
        out = result;
    }
    out
}

/// How many bytes of key-ish alphabet start here.
fn token_len(b: &[u8]) -> usize {
    b.iter()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
        .count()
}

/// Opaque enough to be a secret: mixed classes, and not a plain word, path or
/// version. Deliberately conservative.
fn opaque(v: &str) -> bool {
    if v.contains('/') || v.contains(' ') {
        return false;
    }
    let digits = v.chars().filter(char::is_ascii_digit).count();
    let upper = v.chars().filter(|c| c.is_ascii_uppercase()).count();
    let lower = v.chars().filter(|c| c.is_ascii_lowercase()).count();
    let classes = usize::from(digits > 0) + usize::from(upper > 0) + usize::from(lower > 0);
    classes >= 2 && digits + upper + lower >= v.len() * 3 / 4
}

fn next_char(s: &str, i: usize) -> &str {
    let mut end = i + 1;
    while end < s.len() && !s.is_char_boundary(end) {
        end += 1;
    }
    &s[i..end.min(s.len())]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(s: &str) -> (String, Report) {
        let mut t = s.to_string();
        let mut r = Report::default();
        scrub(&mut t, &mut r);
        (t, r)
    }

    /// Every synthetic. Real secrets never appear in a test.
    #[test]
    fn each_detector_fires_on_its_own_class() {
        let (out, r) = run("export ANTHROPIC_API_KEY=sk-ant-api03-AAAAAAAAAAAAAAAAAAAA");
        assert!(!out.contains("sk-ant-api03"), "{out}");
        assert_eq!(r.vendor_keys, 1);

        let (out, r) = run("aws_key AKIAIOSFODNN7EXAMPLE done");
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"), "{out}");
        assert_eq!(r.vendor_keys, 1);

        let (out, r) = run("token ghp_0123456789abcdefghijklmnopqrstuvwxyz");
        assert!(!out.contains("ghp_0123"), "{out}");
        assert_eq!(r.vendor_keys, 1);

        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let (out, r) = run(&format!("cookie={jwt}"));
        assert!(!out.contains("eyJhbGciOi"), "{out}");
        assert_eq!(r.jwts, 1);

        let (out, r) = run("Authorization: Bearer abcdef123456xyz\nnext line\n");
        assert!(!out.contains("abcdef123456xyz"), "{out}");
        assert!(out.contains("next line"), "the next line must survive");
        assert_eq!(r.auth_headers, 1);

        let (out, r) = run("-----BEGIN OPENSSH PRIVATE KEY-----\nAAAAB3Nza\nmore\n-----END OPENSSH PRIVATE KEY-----\ntail");
        assert!(!out.contains("AAAAB3Nza"), "{out}");
        assert!(out.contains("tail"), "content after the block must survive");
        assert_eq!(r.private_keys, 1);

        let (out, r) = run("DB_PASSWORD=hunter2Zx91QQpLmA0\n");
        assert!(!out.contains("hunter2Zx91QQpLmA0"), "{out}");
        assert_eq!(r.assigned_secrets, 1);
    }

    /// The leak the end-to-end test caught: an assignment quoted inside prose.
    /// The value is one token, not the rest of the sentence.
    #[test]
    fn an_assignment_embedded_in_prose_is_still_redacted() {
        let (out, r) = run("cat .env showed DB_PASSWORD=hunter2Zx91QQpLmA0 in the output");
        assert!(!out.contains("hunter2Zx91QQpLmA0"), "{out}");
        assert!(out.contains("in the output"), "the sentence must survive: {out}");
        assert_eq!(r.assigned_secrets, 1);

        let (out, _) = run("config: {\"api_token\": \"abcdefZZ123456\", \"port\": 8080}");
        assert!(!out.contains("abcdefZZ123456"), "{out}");
        assert!(out.contains("8080"), "the next field must survive: {out}");
    }

    #[test]
    fn home_paths_become_a_tilde() {
        let (out, r) = run("see /home/alice/dev/app/main.rs and /Users/bob/x");
        assert!(!out.contains("alice"), "{out}");
        assert!(!out.contains("bob"), "{out}");
        assert!(out.contains("~/dev/app/main.rs"), "{out}");
        assert!(r.home_paths >= 2);
    }

    /// The corpus that decides whether anyone leaves this on.
    #[test]
    fn ordinary_text_survives_untouched() {
        for benign in [
            "commit 9f2c1ab4e5d6789012345678901234567890abcd landed",
            "id 550e8400-e29b-41d4-a716-446655440000 is the row",
            "PATH=/usr/local/bin:/usr/bin",
            "function e(t){return t.map(function(n){return n*2})}",
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk",
            "the sk- prefix identifies an OpenAI key",
            "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----",
            "version=1.2.3",
            "author: someone reasonable",
        ] {
            let (out, r) = run(benign);
            assert_eq!(out, benign, "false positive on: {benign}");
            assert_eq!(r.total(), 0, "false positive counted on: {benign}");
        }
    }

    #[test]
    fn multibyte_text_is_not_corrupted() {
        let (out, _) = run("héllo wörld — em dash and 日本語 ok");
        assert_eq!(out, "héllo wörld — em dash and 日本語 ok");
    }

    #[test]
    fn a_scrub_is_idempotent() {
        let once = run("KEY_SECRET=abcdef123456ZZZ\n").0;
        let twice = run(&once).0;
        assert_eq!(once, twice);
    }
}
