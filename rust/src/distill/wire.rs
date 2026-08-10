//! The endpoint half of distillation: build a request, POST it, dig the answer out.
//!
//! The transport is the `curl` binary, not a crate. One POST every twenty rows does
//! not justify pulling ~100 transitive dependencies (reqwest, hyper, tokio, rustls)
//! onto a 3.6 GB machine to do what a process already installed here does. `curl` is
//! exec'd with an argv, never through a shell: the key comes from a file on disk and
//! the endpoint from the environment, and neither should ever meet `sh`.
//!
//! Nothing here knows what a rubric means — that is the seam this file sits on.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

use super::prompts::Rubric;

/// A reasoning-tier model: measured 2026-08-02 at roughly 20s a row.
pub const MODEL: &str = "deepseek-v4-pro";
pub const URL: &str = "https://api.deepseek.com/chat/completions";
/// 90s was fine for an interactive `cml distill`; it is not fine on the Stop hook,
/// where two of these stacked into a 3m24s turn on 2026-08-02. The curator now runs
/// detached, and `CML_HTTP_TIMEOUT` still bounds a call that has to fit a budget.
pub const TIMEOUT_SECS: &str = "90";

/// Everything the wire needs, read once. `key` is private: it exists to be written
/// into a 0600 file, never printed, and never placed in an argv.
pub struct Conf {
    pub url: String,
    pub model: String,
    pub timeout: String,
    key: String,
}

impl Conf {
    /// `None` means no key, which is a no-op rather than an error: the hook path must
    /// stay silent on machines that never configured a curator.
    pub fn load() -> Option<Conf> {
        Some(Conf {
            key: key()?,
            url: env_or("CML_LLM_URL", URL),
            model: env_or("CML_LLM_MODEL", MODEL),
            timeout: timeout(),
        })
    }
}

/// Curator credentials: `$CML_LLM_KEY`, else `<data_dir>/llm.key` or `deepseek.key`,
/// else `$DEEPSEEK_API_KEY`. Empty is treated as absent.
pub fn key() -> Option<String> {
    if let Some(k) = env_nonempty("CML_LLM_KEY") {
        return Some(k);
    }
    for name in ["llm.key", "deepseek.key"] {
        let text = std::fs::read_to_string(crate::db::home().join(name)).unwrap_or_default();
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    env_nonempty("DEEPSEEK_API_KEY")
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn env_or(name: &str, fallback: &str) -> String {
    env_nonempty(name).unwrap_or_else(|| fallback.to_string())
}

fn timeout() -> String {
    match env_nonempty("CML_HTTP_TIMEOUT") {
        Some(v) if v.parse::<u32>().is_ok_and(|n| n > 0) => v,
        _ => TIMEOUT_SECS.to_string(),
    }
}

/// One verdict on one row.
#[derive(Debug, PartialEq, Eq)]
pub struct Verdict {
    pub id: i64,
    pub keep: bool,
    pub gist: String,
    /// doc2query expansions: how someone would ASK for this row months later, in
    /// words the row itself does not contain. Indexed alongside the text so keyword
    /// search stops depending on the user remembering the vocabulary they used.
    pub asks: String,
}

/// L2: one working session, summarised. `id` is the index within the batch that was
/// sent — the caller maps it back to a session — not a rowid.
#[derive(Debug, PartialEq, Eq)]
pub struct Scene {
    pub id: i64,
    pub title: String,
    pub summary: String,
    pub outcome: String, // solved | abandoned | ongoing, per the rubric
}

/// The request body. Key order is alphabetical because `serde_json::Map` is a
/// `BTreeMap` — the same order the C++ tree hand-wrote to stay diffable against it.
pub fn request(model: &str, rows: &[(i64, String)], rubric: Rubric) -> String {
    let items: Vec<Value> =
        rows.iter().map(|(id, text)| json!({"id": id, "text": text})).collect();
    json!({
        "messages": [
            {"role": "system", "content": rubric.prompt()},
            {"role": "user", "content": json!({"rows": items}).to_string()},
        ],
        "model": model,
        "response_format": {"type": "json_object"},
        "temperature": 0.0,
    })
    .to_string()
}

/// POST one batch under `rubric` and hand back the raw reply.
///
/// An `Err` here means the call never left this machine. A network failure returns an
/// empty body, which the parsers report as an unreadable response — either way the
/// caller leaves those rows unjudged and the next run retries them.
pub fn post(conf: &Conf, rows: &[(i64, String)], rubric: Rubric) -> Result<String, String> {
    let dir = crate::db::home();
    // Fixed names, as in the C++ tree: a flock in the spawn path already keeps two
    // curators from running at once, and one stale file (removed before every
    // create) is a smaller exposure than a pid-suffixed pile of files holding a key.
    let scratch = Scratch { req: dir.join(".distill-req.json"), cfg: dir.join(".distill-auth") };
    std::fs::write(&scratch.req, request(&conf.model, rows, rubric))
        .map_err(|e| format!("cannot write the request: {e}"))?;

    // The key goes in a --config file, never in argv. /proc/<pid>/cmdline is world
    // readable on Linux, so passing it as `-H "Authorization: Bearer sk-..."`
    // published the key to every process on the machine for the life of the request
    // — plainly visible in `ps`. The file is created 0600 before anything is written
    // to it, so there is no window where it exists with looser permissions.
    write_private(&scratch.cfg, &format!("header = \"Authorization: Bearer {}\"\n", conf.key))
        .map_err(|_| "cannot create a private file for the API key".to_string())?;

    let out = Command::new("curl")
        .args(["-s", "--max-time"])
        .arg(&conf.timeout)
        .arg("--config")
        .arg(&scratch.cfg)
        .args(["-H", "Content-Type: application/json", "-d"])
        .arg(format!("@{}", scratch.req.display()))
        .arg(&conf.url)
        .output()
        .map_err(|e| format!("curl did not run: {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Both scratch files, removed however this function leaves — including the early
/// return when the key file cannot be written, which is the path that would otherwise
/// leave a request body behind.
struct Scratch {
    req: PathBuf,
    cfg: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.cfg);
        let _ = std::fs::remove_file(&self.req);
    }
}

fn write_private(path: &Path, line: &str) -> std::io::Result<()> {
    let _ = std::fs::remove_file(path);
    let mut f =
        std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    f.write_all(line.as_bytes())
}

/// The model's answer, dug out of the /chat/completions envelope. It is JSON inside a
/// JSON string, so each caller parses the result again under its own key.
fn reply_body(response: &str) -> Result<String, String> {
    let envelope: Value = serde_json::from_str(response)
        .map_err(|e| format!("deepseek response unreadable: {e}"))?;
    match envelope["choices"][0]["message"]["content"].as_str() {
        Some(content) => Ok(content.to_string()),
        // An API error must not read as "nothing to do" — on 2026-08-02 a key on a
        // spent account returned "Insufficient Balance" every run, and the endpoint's
        // own words are the only thing that explains a run that curated nothing.
        None => Err(format!(
            "deepseek error: {}",
            envelope["error"]["message"].as_str().unwrap_or("no content")
        )),
    }
}

pub fn parse_verdicts(response: &str) -> Result<Vec<Verdict>, String> {
    let body = reply_body(response)?;
    let parsed: Value =
        serde_json::from_str(&body).map_err(|e| format!("verdict json bad: {e}"))?;
    let mut out = Vec::new();
    for v in parsed["verdicts"].as_array().into_iter().flatten() {
        // A verdict missing `id` or `keep` is skipped rather than guessed at: it would
        // otherwise blocklist or bless whichever row happened to sort first.
        let (Some(id), Some(keep)) = (v["id"].as_i64(), v["keep"].as_bool()) else { continue };
        // asks[] is flattened to one space-joined string: it goes into a single FTS5
        // column, where the separation between phrasings carries no meaning anyway.
        let asks = v["asks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| a.as_str())
            .filter(|a| !a.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        out.push(Verdict { id, keep, gist: v["gist"].as_str().unwrap_or("").to_string(), asks });
    }
    Ok(out)
}

/// Scenes need their own parser: a scene shares no field with a verdict past `id`.
///
/// A missing `scenes` array is an ERROR here, not an empty result. The reply is
/// required to be an object (`response_format` is json_object for every rubric), and
/// a bare array reads as "this batch had nothing" on every key lookup — a whole paid
/// run reporting success with nothing written and no diagnostic anywhere.
pub fn parse_scenes(response: &str) -> Result<Vec<Scene>, String> {
    let body = reply_body(response)?;
    let parsed: Value = serde_json::from_str(&body).map_err(|e| format!("scene json bad: {e}"))?;
    let Some(arr) = parsed["scenes"].as_array() else {
        return Err("scene json has no \"scenes\" array".to_string());
    };
    let mut out = Vec::new();
    for s in arr {
        let Some(id) = s["id"].as_i64() else { continue };
        // A summary is the whole point of the row, and writing an empty one would
        // mark the session done forever — `pending` skips what is already in `scene`.
        // Skipping it instead leaves the session for the next run.
        let summary = s["summary"].as_str().unwrap_or("");
        if summary.is_empty() {
            continue;
        }
        out.push(Scene {
            id,
            title: s["title"].as_str().unwrap_or("").to_string(),
            summary: summary.to_string(),
            outcome: s["outcome"].as_str().unwrap_or("").to_string(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_is_the_shape_deepseek_expects() {
        let req = request("m-1", &[(7, "line \"one\"\nline two".to_string())], Rubric::Assistant);
        assert!(req.starts_with(r#"{"messages":[{"content":"You curate a developer's"#));
        assert!(req.contains(r#"\"verdicts\""#), "the prompt's own quotes are escaped");
        assert!(
            req.contains(
                r#"{\"rows\":[{\"id\":7,\"text\":\"line \\\"one\\\"\\nline two\"}]}"#
            ),
            "rows are a JSON string inside the JSON body"
        );
        assert!(
            req.ends_with(
                r#""role":"user"}],"model":"m-1","response_format":{"type":"json_object"},"temperature":0.0}"#
            ),
            "model, format and temperature: {req}"
        );
        assert!(
            request("m", &[], Rubric::User).contains("developer's own messages"),
            "the user prompt reaches the wire"
        );
    }

    /// A recorded reply, verbatim in the shape the endpoint sends it.
    #[test]
    fn verdicts_survive_a_real_reply() {
        let good = r#"{"choices":[{"message":{"content":"{\"verdicts\":[{\"id\":1,\"keep\":true,\"gist\":\"a fact\",\"asks\":[\"why is it slow\",\"\",\"what broke the build\"]},{\"id\":2,\"keep\":false},{\"keep\":true}]}"}}]}"#;
        let v = parse_verdicts(good).expect("reply parses");
        assert_eq!(v.len(), 2, "the id-less verdict is skipped, not guessed at");
        assert_eq!(v[0], Verdict {
            id: 1,
            keep: true,
            gist: "a fact".to_string(),
            asks: "why is it slow what broke the build".to_string(),
        });
        assert_eq!(v[1], Verdict { id: 2, keep: false, gist: String::new(), asks: String::new() });
    }

    /// Every failure shape must stay a failure: these rows get retried, and a run that
    /// reports "kept 0, dropped 0" with no cause is how a dead key stayed invisible.
    #[test]
    fn broken_replies_are_errors_not_empty_batches() {
        let err = parse_verdicts(r#"{"error":{"message":"Authentication Fails"}}"#).unwrap_err();
        assert_eq!(err, "deepseek error: Authentication Fails", "the endpoint's own words");
        assert!(parse_verdicts("").unwrap_err().starts_with("deepseek response unreadable"));
        let prose = r#"{"choices":[{"message":{"content":"sorry, no JSON today"}}]}"#;
        assert!(parse_verdicts(prose).unwrap_err().starts_with("verdict json bad"));
        // An answer with an empty verdict list is not an error — nothing to retry.
        let none = r#"{"choices":[{"message":{"content":"{\"verdicts\":[]}"}}]}"#;
        assert_eq!(parse_verdicts(none).unwrap(), vec![]);
    }

    #[test]
    fn scenes_parse_and_a_bare_array_is_refused() {
        let reply = r#"{"choices":[{"message":{"content":"{\"scenes\":[{\"id\":0,\"title\":\"DAITA vs 1280 path MTU\",\"summary\":\"DAITA padding cannot fit under a 1280-byte path MTU on patro1a; disabling DAITA fixed it.\",\"outcome\":\"solved\"}]}"}}]}"#;
        let scenes = parse_scenes(reply).expect("scenes parse");
        assert_eq!(scenes.len(), 1);
        assert_eq!(scenes[0].outcome, "solved");
        assert!(scenes[0].title.starts_with("DAITA vs"));

        // The shape that costs real money. Read as an empty batch, a whole paid run
        // reports success with nothing written and no diagnostic anywhere.
        let bare = r#"{"choices":[{"message":{"content":"[{\"id\":0,\"summary\":\"s\",\"outcome\":\"solved\"}]"}}]}"#;
        let err = parse_scenes(bare).unwrap_err();
        assert!(err.contains("scenes"), "the error names what was missing: {err}");

        // A summary-less row is skipped, not written hollow: it would be permanent.
        let thin = r#"{"choices":[{"message":{"content":"{\"scenes\":[{\"id\":1,\"title\":\"t\"}]}"}}]}"#;
        assert!(parse_scenes(thin).unwrap().is_empty());
    }

    #[test]
    fn timeout_falls_back_on_junk() {
        // Whatever the environment says, curl must get a number it accepts.
        assert!(timeout().parse::<u32>().is_ok());
    }
}
