//! leaf-mcp: a small Model Context Protocol server over stdio (one JSON message per line). It talks to the leaf that is running
//! through the hand-over channel (`leaf_sys::handover`), and has two tools, both read-only:
//!
//! - `show_file(path)`: leaf opens the file's folder with the file selected.
//! - `pick_files(prompt)`: leaf asks the user to choose files and the chosen paths come back.
//!
//! It never starts leaf, never changes a file, and answers an error when leaf is not running. See `docs/mcp.md`.

use std::io::{BufRead, Write};
use std::time::Duration;

use serde_json::{Value, json};

/// The requests leaf's `mcp_host` module takes.
const PREFIX: &str = "\u{1}mcp:";
/// A show is quick; a pick waits for a person.
const SHOW_WAIT: Duration = Duration::from_secs(15);
const PICK_WAIT: Duration = Duration::from_secs(600);

fn tools() -> Value {
    json!([
        {
            "name": "show_file",
            "description": "Show a file in the leaf file manager that is running: its folder opens with the file selected. Changes nothing.",
            "inputSchema": {
                "type": "object",
                "properties": { "path": { "type": "string", "description": "Absolute path of the file or folder to show." } },
                "required": ["path"]
            },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "pick_files",
            "description": "Ask the user, in leaf, to choose one or more files, and return the chosen paths (an empty list when the user cancels).",
            "inputSchema": {
                "type": "object",
                "properties": { "prompt": { "type": "string", "description": "What the user is asked to choose; shown as the title of the dialog." } }
            },
            "annotations": { "readOnlyHint": true }
        }
    ])
}

fn text_result(text: String, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn call(name: &str, args: &Value) -> Value {
    match name {
        "show_file" => {
            let Some(path) = args.get("path").and_then(Value::as_str).filter(|p| !p.trim().is_empty()) else {
                return text_result("path is required".into(), true);
            };
            let path = std::path::absolute(path.trim()).unwrap_or_else(|_| path.trim().into());
            if !path.exists() {
                return text_result(format!("{} does not exist", path.display()), true);
            }
            match leaf_sys::handover::ask(&format!("{PREFIX}show:{}", path.display()), SHOW_WAIT) {
                Ok(_) => text_result(format!("Shown in leaf: {}", path.display()), false),
                Err(e) => text_result(e, true),
            }
        }
        "pick_files" => {
            let prompt = args.get("prompt").and_then(Value::as_str).unwrap_or("");
            match leaf_sys::handover::ask(&format!("{PREFIX}pick:{}", prompt.replace(['\r', '\n'], " ")), PICK_WAIT) {
                Ok(paths) => {
                    let n = paths.len();
                    let mut r = text_result(serde_json::to_string(&paths).unwrap_or_default(), false);
                    r["structuredContent"] = json!({ "paths": paths, "count": n });
                    r
                }
                Err(e) => text_result(e, true),
            }
        }
        other => text_result(format!("unknown tool: {other}"), true),
    }
}

/// The answer to one request; `None` for a notification (no id).
fn respond(msg: &Value) -> Option<Value> {
    let id = msg.get("id")?.clone();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let version = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2025-06-18");
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "leaf-mcp", "version": env!("CARGO_PKG_VERSION") }
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            call(name, params.get("arguments").unwrap_or(&Value::Null))
        }
        _ => return Some(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("method not found: {method}") } })),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => respond(&msg),
            Err(_) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "parse error" } })),
        };
        if let Some(r) = reply
            && (writeln!(out, "{r}").is_err() || out.flush().is_err())
        {
            break;
        }
    }
}
