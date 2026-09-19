//! `cml mcp` — memory as a tool, for the harnesses that cannot inject.
//!
//! Claude Code and Codex can put recalled memory in front of the model before
//! it answers. jcode and opencode cannot: their hooks deliver data by
//! environment variable and discard stdout, so nothing a hook prints reaches
//! the prompt. For those, memory has to be something the agent *calls*, and the
//! protocol every one of them already speaks is MCP.
//!
//! # Hand-rolled JSON-RPC
//!
//! No SDK crate. The surface here is three methods over line-delimited JSON on
//! stdin, which is ~100 lines; an SDK would be a dependency tree larger than
//! this entire binary, for a protocol whose whole payload is `{"jsonrpc":"2.0"}`.
//!
//! # The one rule of a stdio server
//!
//! Nothing but JSON-RPC goes to stdout, ever. Diagnostics go to stderr. A stray
//! `println!` corrupts the stream and the client sees a parse error, not a bug.

use std::io::{BufRead, Write};

use serde_json::{json, Value};

/// cml mcp
pub fn run(_args: &[String]) -> crate::R<i32> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            // A malformed line is the client's problem, not a reason to die.
            eprintln!("cml mcp: unparsable request");
            continue;
        };
        if let Some(resp) = handle(&req) {
            writeln!(stdout, "{resp}")?;
            stdout.flush()?;
        }
    }
    // EOF: the client went away, which is the normal end of a stdio server.
    Ok(0)
}

/// `None` for a notification, which by protocol takes no reply.
fn handle(req: &Value) -> Option<String> {
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(Value::as_str).unwrap_or_default();

    let result = match method {
        "initialize" => json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "cml", "version": env!("CARGO_PKG_VERSION")}
        }),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => call(req.get("params")),
        // Notifications carry no id and get no response.
        _ if id.is_none() => return None,
        _ => {
            return Some(
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32601, "message": format!("no such method: {method}")}
                })
                .to_string(),
            )
        }
    };
    // A request without an id was a notification; initialize always has one.
    let id = id?;
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string())
}

fn tools() -> Value {
    json!([
        {
            "name": "memory_search",
            "description": "Search every past session across every agent harness on this \
                            machine - conversations, tool output, and shared memory from \
                            peers. Use before re-solving a problem that may already have \
                            been solved.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "keywords, error text, or filenames"},
                    "project": {"type": "string", "description": "optional project filter"},
                    "limit": {"type": "integer", "description": "max results, default 8"}
                },
                "required": ["query"]
            }
        }
    ])
}

fn call(params: Option<&Value>) -> Value {
    let Some(p) = params else {
        return text("no params");
    };
    if p.get("name").and_then(Value::as_str) != Some("memory_search") {
        return text("unknown tool");
    }
    let args = p.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let Some(query) = args.get("query").and_then(Value::as_str) else {
        return text("memory_search needs a query");
    };

    // Reuse the real search path rather than a second query builder: a
    // divergence here would mean the MCP tool and the CLI disagree about what
    // the index contains, which is the bug nobody would think to look for.
    let mut argv: Vec<String> = query.split_whitespace().map(str::to_string).collect();
    if let Some(project) = args.get("project").and_then(Value::as_str) {
        argv.push("--project".into());
        argv.push(project.into());
    }
    argv.push("--limit".into());
    argv.push(
        args.get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(8)
            .to_string(),
    );

    match crate::search::lines(&argv) {
        Ok(hits) if hits.is_empty() => text("no hits. try different keywords: synonyms, the \
                                             exact error text, or a filename."),
        Ok(hits) => text(&hits.join("\n")),
        Err(e) => text(&format!("search failed: {e}")),
    }
}

fn text(s: &str) -> Value {
    json!({"content": [{"type": "text", "text": s}]})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(req: Value) -> Value {
        serde_json::from_str(&handle(&req).expect("a request must be answered")).unwrap()
    }

    #[test]
    fn initialize_announces_tools_and_echoes_the_id() {
        let r = ask(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}));
        assert_eq!(r["id"], 1);
        assert_eq!(r["result"]["serverInfo"]["name"], "cml");
        assert!(r["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn tools_list_declares_a_usable_schema() {
        let r = ask(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
        let t = &r["result"]["tools"][0];
        assert_eq!(t["name"], "memory_search");
        assert_eq!(t["inputSchema"]["required"][0], "query");
    }

    #[test]
    fn a_notification_gets_no_reply() {
        assert!(
            handle(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none(),
            "replying to a notification corrupts the stream"
        );
    }

    #[test]
    fn an_unknown_method_is_an_error_not_a_crash() {
        let r = ask(json!({"jsonrpc": "2.0", "id": 3, "method": "nope"}));
        assert_eq!(r["error"]["code"], -32601);
    }

    #[test]
    fn a_call_without_a_query_answers_rather_than_panics() {
        let r = ask(json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "memory_search", "arguments": {}}
        }));
        assert!(r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("needs a query"));
    }
}
