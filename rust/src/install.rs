//! `cml install` — wire this tool into every agent harness on the machine.
//!
//! The plugin story only ever covered Claude Code. Everything else needed a
//! human to read a doc and hand-edit a config, which is the step that does not
//! happen. This detects what is installed and patches each config in its own
//! format.
//!
//! # Parse, modify, write — never a regex
//!
//! These are files a user has hand-edited: a settings.json with their own
//! hooks, an opencode.json with their permission rules. A regex over that is
//! how a memory tool becomes the reason someone's editor stopped starting. Each
//! config is parsed, the cml entries are added or removed by identity, and the
//! result is written next to a timestamped `.bak`.
//!
//! # Idempotence is defined by a marker
//!
//! Every command this writes contains [`MARK`]. Installing twice replaces those
//! entries rather than appending a second copy, and uninstalling removes
//! exactly them and nothing a user wrote by hand.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::harness::Harness;

/// The needle that says "cml wrote this". Present in every command string.
const MARK: &str = "cml-hook";

/// What a harness supports, honestly.
///
/// `recall` is the column that matters and the one where the honest answer is
/// often "no": a harness with no prompt-injection hook cannot have memory
/// arrive before the model answers, and saying otherwise in a README is how a
/// support matrix starts lying.
pub struct Support {
    pub id: &'static str,
    pub index: bool,
    pub recall: bool,
    pub capture: bool,
}

pub const MATRIX: &[Support] = &[
    Support { id: "claude", index: true, recall: true, capture: true },
    // jcode hooks deliver data by env var and discard stdout entirely, so no
    // hook of any event can inject text into the prompt. Indexing and capture
    // work; recall is reachable only by the agent calling `cml search`.
    Support { id: "jcode", index: true, recall: false, capture: true },
    Support { id: "opencode", index: true, recall: false, capture: true },
    Support { id: "codex", index: true, recall: true, capture: true },
];

/// cml install [--harness ID] [--dry-run]
pub fn install(args: &[String]) -> crate::R<i32> {
    run(args, Mode::Install)
}

/// cml uninstall [--harness ID] [--dry-run]
pub fn uninstall(args: &[String]) -> crate::R<i32> {
    run(args, Mode::Uninstall)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Install,
    Uninstall,
}

fn run(args: &[String], mode: Mode) -> crate::R<i32> {
    let dry = args.iter().any(|a| a == "--dry-run");
    let only = crate::share::flag(args, "--harness");
    let bin = self_path();

    let mut touched = 0usize;
    for target in targets() {
        if let Some(want) = &only {
            if want != target.id {
                continue;
            }
        }
        if !target.present() {
            continue;
        }
        match target.apply(&bin, mode, dry) {
            Ok(true) => {
                let verb = match (mode, dry) {
                    (_, true) => "would change",
                    (Mode::Install, _) => "wired",
                    (Mode::Uninstall, _) => "removed from",
                };
                println!("{verb} {} ({})", target.id, target.config.display());
                touched += 1;
            }
            Ok(false) => println!("{} already up to date", target.id),
            // A harness that cannot be patched must not abort the others: a
            // machine with a corrupt opencode.json still deserves a working
            // Claude install.
            Err(e) => eprintln!("cml: {} not changed: {e}", target.id),
        }
    }

    if touched == 0 {
        println!("nothing changed");
    } else if mode == Mode::Install && !dry {
        println!("\nrun `cml index --all` once to backfill history");
    }
    Ok(0)
}

/// One harness's config file and how to edit it.
struct Target {
    id: &'static str,
    config: PathBuf,
    kind: Kind,
}

#[derive(Clone, Copy)]
enum Kind {
    /// Claude Code's `settings.json`: `hooks.<Event>[].hooks[]`.
    ClaudeJson,
    /// jcode's `config.toml`: `[hooks]` with one command per event.
    JcodeToml,
    /// opencode's `opencode.json`. No lifecycle hook exists in the config, so
    /// this registers the MCP server instead — which is why `recall` is false
    /// in the matrix but memory is still reachable as a tool.
    OpencodeJson,
    /// Codex's `config.toml`.
    CodexToml,
}

impl Target {
    fn present(&self) -> bool {
        self.config.exists() || self.config.parent().is_some_and(Path::exists)
    }

    fn apply(&self, bin: &str, mode: Mode, dry: bool) -> crate::R<bool> {
        let before = std::fs::read_to_string(&self.config).unwrap_or_default();
        let after = match self.kind {
            Kind::ClaudeJson => claude_json(&before, bin, mode)?,
            Kind::JcodeToml => toml_hooks(&before, bin, mode, "jcode"),
            Kind::CodexToml => toml_hooks(&before, bin, mode, "codex"),
            Kind::OpencodeJson => opencode_json(&before, bin, mode)?,
        };
        if after == before {
            return Ok(false);
        }
        if dry {
            return Ok(true);
        }
        if !before.is_empty() {
            // A timestamped backup, not `.bak`: the second install must not
            // overwrite the copy that predates the first.
            let bak = self
                .config
                .with_extension(format!("bak-cml-{}", crate::paths::now_secs()));
            std::fs::copy(&self.config, &bak)?;
        }
        if let Some(dir) = self.config.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&self.config, after)?;
        Ok(true)
    }
}

fn targets() -> Vec<Target> {
    let home = crate::paths::home_dir();
    vec![
        Target {
            id: "claude",
            config: std::env::var_os("CLAUDE_CONFIG_DIR")
                .map_or_else(|| home.join(".claude"), PathBuf::from)
                .join("settings.json"),
            kind: Kind::ClaudeJson,
        },
        Target {
            id: "jcode",
            config: home.join(".jcode").join("config.toml"),
            kind: Kind::JcodeToml,
        },
        Target {
            id: "opencode",
            config: home.join(".config").join("opencode").join("opencode.json"),
            kind: Kind::OpencodeJson,
        },
        Target {
            id: "codex",
            config: home.join(".codex").join("config.toml"),
            kind: Kind::CodexToml,
        },
    ]
}

/// Claude Code: four events, each a command that must never break a session.
fn claude_json(src: &str, bin: &str, mode: Mode) -> crate::R<String> {
    let mut root: Value = if src.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(src)?
    };
    if !root.is_object() {
        return Err("settings.json is not an object".into());
    }
    let hooks = root
        .as_object_mut()
        .expect("checked above")
        .entry("hooks")
        .or_insert_with(|| json!({}));

    for (event, cmd) in [
        ("SessionStart", "nudge"),
        ("UserPromptSubmit", "recall"),
        ("PostToolUse", "offload"),
        ("Stop", "index"),
    ] {
        let Some(list) = hooks
            .as_object_mut()
            .map(|h| h.entry(event).or_insert_with(|| json!([])))
        else {
            continue;
        };
        let Some(arr) = list.as_array_mut() else { continue };

        // Remove ours wherever it sits, then re-add. That is what makes a
        // second install a replace and an uninstall exact.
        arr.retain(|group| !group_is_ours(group));
        if mode == Mode::Install {
            arr.push(json!({
                "hooks": [{
                    "type": "command",
                    "command": hook_command(bin, cmd),
                    "timeout": 10
                }]
            }));
        }
        if arr.is_empty() {
            if let Some(h) = hooks.as_object_mut() {
                h.remove(event);
            }
        }
    }
    if hooks.as_object().is_some_and(serde_json::Map::is_empty) {
        if let Some(o) = root.as_object_mut() {
            o.remove("hooks");
        }
    }
    Ok(format!("{}\n", serde_json::to_string_pretty(&root)?))
}

fn group_is_ours(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hs| {
            hs.iter().any(|h| {
                h.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|c| c.contains(MARK))
            })
        })
}

/// opencode has no lifecycle hook in its config, so cml registers as an MCP
/// server: the agent can call `memory_search` even though nothing can inject
/// on its behalf.
fn opencode_json(src: &str, bin: &str, mode: Mode) -> crate::R<String> {
    let mut root: Value = if src.trim().is_empty() {
        json!({"$schema": "https://opencode.ai/config.json"})
    } else {
        serde_json::from_str(src)?
    };
    let Some(obj) = root.as_object_mut() else {
        return Err("opencode.json is not an object".into());
    };
    match mode {
        Mode::Install => {
            let mcp = obj.entry("mcp").or_insert_with(|| json!({}));
            if let Some(m) = mcp.as_object_mut() {
                m.insert(
                    "cml".to_string(),
                    json!({
                        "type": "local",
                        "command": [bin, "mcp"],
                        "enabled": true
                    }),
                );
            }
        }
        Mode::Uninstall => {
            if let Some(m) = obj.get_mut("mcp").and_then(Value::as_object_mut) {
                m.remove("cml");
                if m.is_empty() {
                    obj.remove("mcp");
                }
            }
        }
    }
    Ok(format!("{}\n", serde_json::to_string_pretty(&root)?))
}

/// A `[hooks]` table in a TOML file, edited by line rather than by parser.
///
/// No `toml` crate. The edit is "own the lines inside one table that carry our
/// marker", which is a line filter; pulling in a parser and a serialiser would
/// also mean *rewriting* the user's file, losing their comments and ordering.
/// A hand-edited config that comes back without its comments is a config the
/// user does not forgive.
fn toml_hooks(src: &str, bin: &str, mode: Mode, harness: &str) -> String {
    let events: &[(&str, &str)] = match harness {
        // jcode: turn_end indexes, session_start briefs. There is no
        // prompt-submit event, which is exactly why recall is false for it.
        "jcode" => &[("session_start", "nudge"), ("turn_end", "index")],
        _ => &[("session_start", "nudge"), ("stop", "index")],
    };

    let mut out: Vec<String> = Vec::new();
    let mut in_hooks = false;
    let mut seen_hooks = false;

    for line in src.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            // Leaving the hooks table: this is where our lines go.
            if in_hooks && mode == Mode::Install {
                for (event, cmd) in events {
                    out.push(toml_line(event, bin, cmd));
                }
            }
            in_hooks = t == "[hooks]";
            seen_hooks |= in_hooks;
        }
        // Drop any line we previously wrote, wherever it is.
        if line.contains(MARK) {
            continue;
        }
        out.push(line.to_string());
    }

    // The table was last in the file, or absent entirely.
    if mode == Mode::Install {
        if in_hooks {
            for (event, cmd) in events {
                out.push(toml_line(event, bin, cmd));
            }
        } else if !seen_hooks {
            out.push(String::new());
            out.push("[hooks]".to_string());
            for (event, cmd) in events {
                out.push(toml_line(event, bin, cmd));
            }
        }
    }

    let mut s = out.join("\n");
    if mode == Mode::Uninstall {
        s = drop_empty_hooks_table(&s);
    }
    while s.ends_with("\n\n") {
        s.truncate(s.len() - 1);
    }
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

/// Remove a `[hooks]` header that our own uninstall just emptied.
///
/// Leaving it behind is not a parse error, but it means uninstall does not
/// return the file to what it was, and "exactly and only what we added" is the
/// promise the command makes.
fn drop_empty_hooks_table(s: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut lines = s.lines().peekable();
    while let Some(line) = lines.next() {
        if line.trim() == "[hooks]" {
            // Look ahead: if nothing but blanks and comments stands between
            // here and the next table, this header owns nothing.
            let empty = lines
                .clone()
                .take_while(|l| !l.trim_start().starts_with('['))
                .all(|l| l.trim().is_empty());
            if empty {
                // Drop the header and the blank run that followed it.
                while lines.peek().is_some_and(|l| l.trim().is_empty()) {
                    lines.next();
                }
                continue;
            }
        }
        out.push(line);
    }
    out.join("\n")
}

/// One `event = 'command'` line, in a TOML *literal* string.
///
/// Single quotes, not double. The command contains `"` around the binary path,
/// and a basic TOML string would need those escaped — the first version of this
/// wrote `key = "[ -x "/path" ]..."`, which is not valid TOML and would have
/// broken the config of every harness it touched. A literal string takes the
/// body verbatim; the only character it cannot hold is `'`, which is why the
/// path is checked for one.
fn toml_line(event: &str, bin: &str, cmd: &str) -> String {
    let body = hook_command(bin, cmd);
    if body.contains('\'') {
        // A path with a quote in it is rare enough to be worth refusing rather
        // than mis-escaping. The marker keeps it removable.
        return format!("# {MARK}: path contains a quote, {event} not wired");
    }
    format!("{event} = '{body}'")
}

/// The command every harness runs, and the reason it is a shell one-liner.
///
/// `|| true` and the `-x` guard are the design law this repo already states: a
/// memory hook must never block or break a session. A missing binary, a corrupt
/// index, a full disk — all of them exit 0 here.
fn hook_command(bin: &str, sub: &str) -> String {
    format!("[ -x \"{bin}\" ] && \"{bin}\" {sub} 2>/dev/null || true # {MARK}")
}

/// Where this binary lives, for writing into someone else's config.
fn self_path() -> String {
    std::env::current_exe()
        .ok()
        .filter(|p| p.is_absolute())
        .map_or_else(|| "cml".to_string(), |p| p.to_string_lossy().into_owned())
}

/// cml harnesses — what is installed, and what cml can do with each.
pub fn report(_args: &[String]) -> crate::R<i32> {
    println!("{:<10} {:>9}  {:>6} {:>7} {:>7}", "harness", "detected", "index", "recall", "capture");
    for s in MATRIX {
        let detected = if s.id == "claude" {
            crate::db::transcripts_dir().is_dir()
        } else {
            Harness::parse(s.id).is_some_and(Harness::detect)
        };
        println!(
            "{:<10} {:>9}  {:>6} {:>7} {:>7}",
            s.id,
            if detected { "yes" } else { "-" },
            mark(s.index),
            mark(s.recall),
            mark(s.capture)
        );
    }
    println!("\nrecall = memory arrives before the model answers, with no command run.");
    println!("where it is '-', the harness has no prompt-injection hook; use `cml mcp`.");
    Ok(0)
}

const fn mark(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "-"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_install_is_idempotent_and_reversible() {
        let original = r#"{
  "model": "opus",
  "hooks": {
    "SessionStart": [
      {"hooks": [{"type": "command", "command": "python3 /home/u/mine.py"}]}
    ]
  }
}"#;
        let once = claude_json(original, "/bin/cml", Mode::Install).unwrap();
        assert!(once.contains(MARK), "the hook was not written");
        assert!(once.contains("mine.py"), "a hand-written hook must survive");
        assert!(once.contains("\"model\": \"opus\""), "unrelated keys must survive");

        // Twice must equal once.
        let twice = claude_json(&once, "/bin/cml", Mode::Install).unwrap();
        assert_eq!(once, twice, "a second install must not append a second copy");

        // And uninstall must take out exactly ours.
        let gone = claude_json(&twice, "/bin/cml", Mode::Uninstall).unwrap();
        assert!(!gone.contains(MARK), "uninstall left our hook behind");
        assert!(gone.contains("mine.py"), "uninstall removed a hand-written hook");
        let reparsed: Value = serde_json::from_str(&gone).unwrap();
        assert_eq!(reparsed["model"], "opus");
    }

    #[test]
    fn an_empty_or_absent_settings_file_is_created_not_rejected() {
        let made = claude_json("", "/bin/cml", Mode::Install).unwrap();
        assert!(made.contains(MARK));
        let v: Value = serde_json::from_str(&made).unwrap();
        assert!(v["hooks"]["UserPromptSubmit"].is_array());
    }

    #[test]
    fn toml_hooks_land_inside_the_table_and_leave_comments_alone() {
        let original = "# my config\n[agents]\nmodel = \"x\"\n\n[hooks]\npre_tool_timeout_ms = 5000\n\n[ambient]\nenabled = false\n";
        let once = toml_hooks(original, "/bin/cml", Mode::Install, "jcode");
        assert!(once.contains("# my config"), "comments must survive");
        assert!(once.contains("pre_tool_timeout_ms = 5000"), "existing keys must survive");
        assert!(once.contains(MARK));
        // Our lines belong to [hooks], not to [ambient].
        let hooks_at = once.find("[hooks]").unwrap();
        let ambient_at = once.find("[ambient]").unwrap();
        let ours = once.find(MARK).unwrap();
        assert!(hooks_at < ours && ours < ambient_at, "hook landed in the wrong table:\n{once}");

        let twice = toml_hooks(&once, "/bin/cml", Mode::Install, "jcode");
        assert_eq!(once, twice, "a second install must be a no-op");

        let gone = toml_hooks(&twice, "/bin/cml", Mode::Uninstall, "jcode");
        assert!(!gone.contains(MARK));
        assert!(gone.contains("pre_tool_timeout_ms = 5000"));
        assert!(gone.contains("enabled = false"));
    }

    /// The bug the fake-HOME run caught: the command carries `"` around the
    /// binary path, so a double-quoted TOML value produced a file no TOML
    /// parser accepts — it would have broken the config of every harness it
    /// touched.
    #[test]
    fn a_written_toml_value_is_parseable() {
        let out = toml_hooks("[hooks]\n", "/home/u/.local/bin/cml", Mode::Install, "jcode");
        for line in out.lines().filter(|l| l.contains(MARK) && !l.starts_with('#')) {
            let (_, value) = line.split_once(" = ").expect("a key and a value");
            assert!(
                value.starts_with('\'') && value.ends_with('\''),
                "a value holding `\"` must be a TOML literal string: {line}"
            );
            // A literal string ends at its first closing quote, so there must
            // be exactly two in the whole value.
            assert_eq!(
                value.matches('\'').count(),
                2,
                "an embedded quote would truncate the value: {line}"
            );
        }
    }

    #[test]
    fn a_toml_without_a_hooks_table_gets_one() {
        let gained = toml_hooks("[agents]\nmodel = \"x\"\n", "/bin/cml", Mode::Install, "jcode");
        assert!(gained.contains("[hooks]"));
        assert!(gained.contains(MARK));
        assert_eq!(
            gained,
            toml_hooks(&gained, "/bin/cml", Mode::Install, "jcode"),
            "idempotent even when it created the table"
        );
    }

    #[test]
    fn opencode_keeps_the_users_permission_rules() {
        let original = r#"{"$schema":"https://opencode.ai/config.json","permission":{"bash":{"rm -rf *":"ask"}}}"#;
        let once = opencode_json(original, "/bin/cml", Mode::Install).unwrap();
        assert!(once.contains("\"cml\""));
        assert!(once.contains("rm -rf *"), "permission rules must survive");
        assert_eq!(once, opencode_json(&once, "/bin/cml", Mode::Install).unwrap());

        let gone = opencode_json(&once, "/bin/cml", Mode::Uninstall).unwrap();
        assert!(!gone.contains("\"cml\""));
        assert!(gone.contains("rm -rf *"));
    }

    /// Uninstall must not leave a `[hooks]` header it emptied: "exactly and
    /// only what we added" includes the table we created.
    #[test]
    fn uninstall_removes_a_table_it_created() {
        let plain = "model = \"gpt-5\"\n";
        let wired = toml_hooks(plain, "/bin/cml", Mode::Install, "codex");
        assert!(wired.contains("[hooks]"));
        let gone = toml_hooks(&wired, "/bin/cml", Mode::Uninstall, "codex");
        assert_eq!(gone, plain, "uninstall left a husk behind:\n{gone}");
    }

    /// But a table the user already had must survive, keys and all.
    #[test]
    fn uninstall_keeps_a_table_the_user_owns() {
        let mine = "[hooks]\npre_tool_timeout_ms = 5000\n";
        let wired = toml_hooks(mine, "/bin/cml", Mode::Install, "jcode");
        let gone = toml_hooks(&wired, "/bin/cml", Mode::Uninstall, "jcode");
        assert!(gone.contains("[hooks]"), "removed a table we did not create");
        assert!(gone.contains("pre_tool_timeout_ms = 5000"));
    }

    /// The stated design law: a hook must never break a session.
    #[test]
    fn every_written_command_fails_open() {
        let cmd = hook_command("/bin/cml", "recall");
        assert!(cmd.contains("|| true"), "{cmd}");
        assert!(cmd.contains("-x"), "a missing binary must not error: {cmd}");
        assert!(cmd.contains(MARK), "every command must be identifiable: {cmd}");
    }
}
