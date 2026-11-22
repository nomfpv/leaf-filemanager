//! leaf-mcp: a small Model Context Protocol server over stdio (one JSON message per line). It talks to the leaf that is running
//! through the hand-over channel (`leaf_sys::handover`). Its tools:
//!
//! - `show_file(path)`: leaf opens the file's folder with the file selected.
//! - `pick_files(prompt)`: leaf asks the user to choose files and the chosen paths come back.
//! - `search(query)`, `list_folder(path)`: read-only, inside the project folder of this session.
//! - `copy`, `move`, `rename`, `create_folder`, `trash`: only when the user turned on "Let agents move and delete files through
//!   leaf" (Settings > Developer); otherwise they are not listed. They run in leaf, as Status Center jobs that can be undone.
//!
//! The project folder of the session is `--root <folder>` (or `LEAF_MCP_ROOT`, or the folder leaf-mcp was started in), given to
//! leaf with every request; leaf resolves it once and keeps it for the session. leaf checks every request itself: this program
//! only shapes them. It never starts leaf and answers an error when leaf is not running. See `docs/mcp.md`.

use std::io::{BufRead, Write};
use std::time::Duration;

use serde_json::{Value, json};

/// The requests leaf's `mcp_host` module takes.
const PREFIX: &str = "\u{1}mcp:";
/// A show is quick; a pick waits for a person.
const SHOW_WAIT: Duration = Duration::from_secs(15);
const PICK_WAIT: Duration = Duration::from_secs(600);
/// Asking leaf whether the changing tools are on.
const CAPS_WAIT: Duration = Duration::from_secs(3);
/// A search or a listing.
const READ_WAIT: Duration = Duration::from_secs(30);
/// A change: the user's consent (up to a minute) and then the job itself.
const CHANGE_WAIT: Duration = Duration::from_secs(30 * 60);

/// The tools that change files: listed only with the switch on.
const CHANGING: &[&str] = &["copy", "move", "rename", "create_folder", "trash"];

/// This session: an id of its own, the agent's name for itself, the project folder.
struct Ctx {
    session: String,
    agent: String,
    root: String,
}

fn session_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    // RandomState is seeded from the system's random source per process
    let mut out = String::new();
    for i in 0..2u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u32(std::process::id());
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

fn root_arg() -> String {
    let mut args = std::env::args().skip(1);
    let mut root = None;
    while let Some(a) = args.next() {
        if a == "--root" {
            root = args.next();
        } else if let Some(r) = a.strip_prefix("--root=") {
            root = Some(r.to_string());
        }
    }
    let root = root.or_else(|| std::env::var("LEAF_MCP_ROOT").ok().filter(|r| !r.trim().is_empty())).map(std::path::PathBuf::from);
    let root = match root {
        Some(r) => r,
        None => std::env::current_dir().unwrap_or_default(),
    };
    std::path::absolute(&root).unwrap_or(root).to_string_lossy().into_owned()
}

fn string_prop(desc: &str) -> Value {
    json!({ "type": "string", "description": desc })
}

fn paths_prop(desc: &str, max: usize) -> Value {
    json!({ "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": max, "description": desc })
}

/// The tools; the changing ones only when `changes` (the switch in leaf is on).
fn tools(changes: bool) -> Value {
    let mut list = vec![
        json!({
            "name": "show_file",
            "description": "Show a file in the leaf file manager that is running: its folder opens with the file selected. Changes nothing.",
            "inputSchema": { "type": "object", "properties": { "path": string_prop("Absolute path of the file or folder to show.") }, "required": ["path"] },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "pick_files",
            "description": "Ask the user, in leaf, to choose one or more files, and return the chosen paths (an empty list when the user cancels).",
            "inputSchema": { "type": "object", "properties": { "prompt": string_prop("What the user is asked to choose; shown as the title of the dialog.") } },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "search",
            "description": "Find files and folders by name inside this session's project folder with leaf's name index (or a bounded walk where the index does not cover it). With contents=true, Windows Search is asked for names and contents where it indexes the folder. Returns paths only, at most 200 (default 50); items protected by the project's agent rules are left out.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": string_prop("Part of a name; * and ? work with leaf's index."),
                    "contents": { "type": "boolean", "description": "Also match file contents (Windows Search only)." },
                    "max": { "type": "integer", "minimum": 1, "maximum": 200 }
                },
                "required": ["query"]
            },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "list_folder",
            "description": "List a folder inside this session's project folder: names, kind (folder, file, link) and size, at most 1000 entries. Reads no file contents.",
            "inputSchema": { "type": "object", "properties": { "path": string_prop("The folder, absolute or relative to the project folder; \".\" is the project folder.") }, "required": ["path"] },
            "annotations": { "readOnlyHint": true }
        }),
    ];
    if changes {
        let note = " Runs in leaf as a Status Center job the user can cancel and undo. Inside the project folder only (no \"..\", no links, no folder with a link inside; .claude and .git are never changed); an existing target is never overwritten (the call fails and names it). The first change of a session asks the user in leaf.";
        list.extend([
            json!({
                "name": "copy",
                "description": format!("Copy files or folders into a folder.{note}"),
                "inputSchema": { "type": "object", "properties": { "sources": paths_prop("What to copy (at most 200).", 200), "destination": string_prop("The folder to copy into.") }, "required": ["sources", "destination"] },
                "annotations": { "destructiveHint": false, "idempotentHint": false }
            }),
            json!({
                "name": "move",
                "description": format!("Move files or folders into a folder.{note}"),
                "inputSchema": { "type": "object", "properties": { "sources": paths_prop("What to move (at most 200).", 200), "destination": string_prop("The folder to move into.") }, "required": ["sources", "destination"] },
                "annotations": { "destructiveHint": false, "idempotentHint": false }
            }),
            json!({
                "name": "rename",
                "description": format!("Give a file or folder a new name in the same folder.{note}"),
                "inputSchema": { "type": "object", "properties": { "path": string_prop("The item to rename."), "new_name": string_prop("The new name only (no folder).") }, "required": ["path", "new_name"] },
                "annotations": { "destructiveHint": false, "idempotentHint": false }
            }),
            json!({
                "name": "create_folder",
                "description": format!("Create one new folder; missing parent folders (up to 8 levels) are made too, each checked like a single new folder.{note}"),
                "inputSchema": { "type": "object", "properties": { "path": string_prop("The new folder.") }, "required": ["path"] },
                "annotations": { "destructiveHint": false, "idempotentHint": false }
            }),
            json!({
                "name": "trash",
                "description": format!("Move files or folders to the Recycle Bin / Trash (never a permanent delete), at most 50 items and 2000 files / 1 GB in all per call, never the project folder itself, .claude or .git.{note}"),
                "inputSchema": { "type": "object", "properties": { "paths": paths_prop("What to move to the Recycle Bin / Trash (at most 50).", 50) }, "required": ["paths"] },
                "annotations": { "destructiveHint": true, "idempotentHint": false }
            }),
        ]);
    }
    Value::Array(list)
}

/// Whether leaf has the changing tools on (`false` when leaf is not running or says nothing).
fn changes_on() -> bool {
    leaf_sys::handover::ask(&format!("{PREFIX}caps"), CAPS_WAIT).is_ok_and(|lines| lines.iter().any(|l| l == "files=on"))
}

fn text_result(text: String, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn str_arg(args: &Value, key: &str) -> Value {
    args.get(key).and_then(Value::as_str).map_or(Value::Null, |s| json!(s))
}

fn list_arg(args: &Value, key: &str) -> Value {
    match args.get(key) {
        Some(Value::Array(a)) => Value::Array(a.iter().filter_map(Value::as_str).map(|s| json!(s)).collect()),
        Some(Value::String(s)) => json!([s]),
        _ => json!([]),
    }
}

/// The operation of a file tool, in the shape leaf parses (`rf_core::agent_ops::Op`); only the known arguments are taken.
fn op_of(name: &str, args: &Value) -> Option<Value> {
    Some(match name {
        "search" => {
            let mut v = json!({ "tool": "search", "query": str_arg(args, "query") });
            if let Some(c) = args.get("contents").and_then(Value::as_bool) {
                v["contents"] = json!(c);
            }
            if let Some(m) = args.get("max").and_then(Value::as_u64) {
                v["max"] = json!(m.min(u64::from(u32::MAX)));
            }
            v
        }
        "list_folder" => json!({ "tool": "list_folder", "path": str_arg(args, "path") }),
        "copy" | "move" => json!({ "tool": name, "sources": list_arg(args, "sources"), "destination": str_arg(args, "destination") }),
        "rename" => json!({ "tool": "rename", "path": str_arg(args, "path"), "new_name": str_arg(args, "new_name") }),
        "create_folder" => json!({ "tool": "create_folder", "path": str_arg(args, "path") }),
        "trash" => json!({ "tool": "trash", "paths": list_arg(args, "paths") }),
        _ => return None,
    })
}

/// Sends a file tool's request to leaf and turns its answer into a tool result.
fn file_tool(ctx: &Ctx, name: &str, op: Value) -> Value {
    let req = json!({ "session": ctx.session, "agent": ctx.agent, "root": ctx.root, "op": op });
    let wait = if CHANGING.contains(&name) { CHANGE_WAIT } else { READ_WAIT };
    match leaf_sys::handover::ask(&format!("{PREFIX}op:{req}"), wait) {
        Ok(lines) => {
            let Some(v) = lines.first().and_then(|l| serde_json::from_str::<Value>(l).ok()) else {
                return text_result("leaf gave no answer to this request (an older leaf?)".into(), true);
            };
            if v.get("ok").and_then(Value::as_bool) == Some(true) {
                let text = v.get("text").and_then(Value::as_str).unwrap_or("done").to_string();
                let mut r = text_result(text, false);
                let mut data = v.clone();
                if let Some(o) = data.as_object_mut() {
                    o.remove("ok");
                    o.remove("text");
                }
                r["structuredContent"] = data;
                r
            } else {
                text_result(v.get("error").and_then(Value::as_str).unwrap_or("leaf refused the request").to_string(), true)
            }
        }
        Err(e) if e == "no answer in time" && CHANGING.contains(&name) => {
            text_result("No answer from leaf in time; the operation may still be running in leaf's Status Center.".into(), true)
        }
        Err(e) => text_result(e, true),
    }
}

fn call(ctx: &Ctx, name: &str, args: &Value) -> Value {
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
        other => match op_of(other, args) {
            Some(op) => file_tool(ctx, other, op),
            None => text_result(format!("unknown tool: {other}"), true),
        },
    }
}

/// The answer to one request; `None` for a notification (no id).
fn respond(ctx: &mut Ctx, msg: &Value) -> Option<Value> {
    let id = msg.get("id")?.clone();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            if let Some(n) = params.pointer("/clientInfo/name").and_then(Value::as_str) {
                ctx.agent = n.chars().filter(|c| c.is_alphanumeric() || " -_.".contains(*c)).take(40).collect();
            }
            let version = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2025-06-18");
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": true } },
                "serverInfo": { "name": "leaf-mcp", "version": env!("CARGO_PKG_VERSION") }
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools(changes_on()) }),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            call(ctx, name, params.get("arguments").unwrap_or(&Value::Null))
        }
        _ => return Some(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("method not found: {method}") } })),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

/// How often leaf is asked whether the switch for the changing tools flipped (a notification goes out when it did).
const WATCH_EVERY: Duration = Duration::from_secs(10);

fn send(out: &std::sync::Mutex<std::io::Stdout>, msg: &Value) -> bool {
    let Ok(mut o) = out.lock() else { return false };
    writeln!(o, "{msg}").is_ok() && o.flush().is_ok()
}

fn main() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    let mut ctx = Ctx { session: session_id(), agent: String::new(), root: root_arg() };
    let stdin = std::io::stdin();
    let out = Arc::new(Mutex::new(std::io::stdout()));
    // set when the client has said `initialized`; the last switch state a list was made for
    let ready = Arc::new(AtomicBool::new(false));
    let listed = Arc::new(AtomicBool::new(false));
    {
        let (out, ready, listed) = (out.clone(), ready.clone(), listed.clone());
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(WATCH_EVERY);
                if !ready.load(Ordering::Relaxed) {
                    continue;
                }
                let now = changes_on();
                if now != listed.swap(now, Ordering::Relaxed) && !send(&out, &json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" })) {
                    break;
                }
            }
        });
    }
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => {
                if msg.get("method").and_then(Value::as_str) == Some("notifications/initialized") {
                    listed.store(changes_on(), Ordering::Relaxed);
                    ready.store(true, Ordering::Relaxed);
                }
                respond(&mut ctx, &msg)
            }
            Err(_) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "parse error" } })),
        };
        if let Some(r) = reply
            && !send(&out, &r)
        {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &Value) -> Vec<String> {
        v.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn mcp_changing_tools_absent_with_the_switch_off() {
        let off = names(&tools(false));
        for n in CHANGING {
            assert!(!off.contains(&n.to_string()), "{n} listed with the switch off");
        }
        assert!(off.contains(&"search".to_string()) && off.contains(&"list_folder".to_string()));
        let on = names(&tools(true));
        for n in CHANGING {
            assert!(on.contains(&n.to_string()), "{n} missing with the switch on");
        }
        // no permanent delete, no program to run, under any name
        for n in on {
            assert!(!["delete", "remove", "run", "exec", "write", "read_file"].contains(&n.as_str()), "{n}");
        }
    }
}
