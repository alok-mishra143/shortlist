//! A local MCP server for the Shortlist candidate library.
//!
//! OpenCode starts this executable over stdio and exposes its tools to the
//! model through native tool calling. The text protocol in `shortlist-harness`
//! remains a fallback for models that cannot call MCP tools.
//!
//! The server keeps the handle table across calls, so a model can call
//! `screen`, then `get` with the same `a1` handles. Candidate names also work.

use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, OnceLock};

use anyhow::Result;
use serde_json::{Value, json};
use shortlist_embed::Embedder;
use shortlist_harness::{Budget, Call, Refs, Registry, ToolContext};
use shortlist_store::Store;

const KNOWN_TOOLS: &[&str] = &[
    "stats",
    "find",
    "shortlist",
    "get",
    "read",
    "grep",
    "note",
    "compare",
];

const INSTRUCTIONS: &str = "\
Shortlist exposes the recruiter's private candidate library. \
Use `screen` (semantic shortlist) to find candidates, then `get` or `read` \
their roles and experience before judging. The shortlist `pre` column is a \
retrieval pre-score, not your verdict. For best/top/compare questions, inspect \
at least the top three profiles and explain gaps. Never invent a candidate. \
`get`/`read`/`compare` accept a handle (`a1`) or the candidate's full name. \
Handles are local to this server; when mixing with a Shortlist text-harness \
transcript, use full names.";

struct Server {
    store: Store,
    registry: Registry,
    refs: Refs,
    embedder: OnceLock<Option<Arc<Embedder>>>,
}

impl Server {
    fn new(store: Store) -> Self {
        Self {
            store,
            registry: Registry::builtin(),
            refs: Refs::default(),
            embedder: OnceLock::new(),
        }
    }

    fn embedder(&self) -> Option<Arc<Embedder>> {
        self.embedder
            .get_or_init(|| Embedder::load().ok().map(Arc::new))
            .clone()
    }

    fn run_tool(&mut self, name: &str, arguments: Option<&Value>) -> Result<String, String> {
        let call = to_call(name, arguments)?;
        let embedder = if call.tool == "shortlist" {
            self.embedder()
        } else {
            None
        };
        let refs = {
            let mut ctx = ToolContext::new(&self.store, embedder.as_deref(), Budget::new(20_000));
            ctx.refs = self.refs.clone();
            let result = self.registry.run(&mut ctx, &call);
            (ctx.refs.clone(), result)
        };
        self.refs = refs.0;
        refs.1
            .map(|result| present_body(&call.tool, &result.body, &self.refs))
            .map_err(|error| error.to_string())
    }

    fn handle(&mut self, message: &Value) -> Option<Value> {
        let id = message.get("id").cloned();
        let id = id?;
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let params = message.get("params").unwrap_or(&Value::Null).clone();
        let response = match method {
            "initialize" => Ok(json!({
                "protocolVersion": params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("2024-11-05"),
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": {
                    "name": "shortlist",
                    "title": "Shortlist candidate library",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "instructions": INSTRUCTIONS
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tool_definitions() })),
            "resources/list" => Ok(json!({ "resources": [] })),
            "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
            "roots/list" => Ok(json!({ "roots": [] })),
            "prompts/list" => Ok(json!({ "prompts": [] })),
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let arguments = params.get("arguments");
                match self.run_tool(name, arguments) {
                    Ok(text) => Ok(json!({
                        "content": [{ "type": "text", "text": text }],
                        "isError": false
                    })),
                    Err(error) => Ok(json!({
                        "content": [{ "type": "text", "text": error }],
                        "isError": true
                    })),
                }
            }
            "shutdown" => Ok(Value::Null),
            other => Err(format!("unknown method: {other}")),
        };
        match response {
            Ok(result) => Some(json!({ "jsonrpc": "2.0", "id": id, "result": result })),
            Err(error) => Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": error }
            })),
        }
    }
}

fn main() -> Result<()> {
    let store = Store::open(&shortlist_core::db_path())?;
    let mut server = Server::new(store);
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in BufReader::new(stdin.lock()).lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Value::Array(batch) = message {
            let responses: Vec<Value> = batch
                .iter()
                .filter_map(|message| server.handle(message))
                .collect();
            if !responses.is_empty() {
                writeln!(stdout, "{}", serde_json::to_string(&responses)?)?;
                stdout.flush()?;
            }
            continue;
        }
        if let Some(response) = server.handle(&message) {
            writeln!(stdout, "{}", serde_json::to_string(&response)?)?;
            stdout.flush()?;
        }
    }
    Ok(())
}

/// Native MCP results use candidate names, not per-process handles. This avoids
/// a model mixing a harness handle from the app prompt with an MCP handle from a
/// different server process.
fn present_body(tool: &str, body: &str, refs: &Refs) -> String {
    match tool {
        "find" | "shortlist" | "compare" => rewrite_table(body),
        "get" | "read" | "note" => {
            let mut lines = body.lines();
            let Some(first) = lines.next() else {
                return body.into();
            };
            let (handle, rest) = first.split_once(' ').unwrap_or((first, ""));
            let name = refs.name(handle).unwrap_or(handle);
            let mut out = if rest == name || rest.is_empty() {
                name.to_string()
            } else {
                format!("{name} {rest}")
            };
            for line in lines {
                out.push('\n');
                out.push_str(line);
            }
            out
        }
        "grep" => body
            .lines()
            .map(|line| {
                line.split_once(": ")
                    .and_then(|(handle, rest)| {
                        refs.name(handle).map(|name| format!("{name}: {rest}"))
                    })
                    .unwrap_or_else(|| line.to_string())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => body.to_string(),
    }
}

/// Drop the handle column and the duplicate name column from candidate tables.
fn rewrite_table(body: &str) -> String {
    body.lines()
        .map(|line| {
            if line.starts_with("cols: ") || line.starts_with("compare cols: ") {
                let prefix = if line.starts_with("compare cols: ") {
                    "compare cols: "
                } else {
                    "cols: "
                };
                let columns: Vec<&str> = line[prefix.len()..].split('|').collect();
                return format!("{prefix}{}", columns[1..].join("|"));
            }
            if !line.starts_with('a') || !line.contains('|') {
                return line.to_string();
            }
            let columns: Vec<&str> = line.split('|').collect();
            if columns.len() < 3 {
                return line.to_string();
            }
            columns[1..].join("|")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn to_call(name: &str, arguments: Option<&Value>) -> Result<Call, String> {
    let tool = name.strip_prefix("shortlist_").unwrap_or(name).to_string();
    let tool = if tool == "screen" {
        "shortlist".to_string()
    } else {
        tool
    };
    if !KNOWN_TOOLS.contains(&tool.as_str()) {
        return Err(format!("unknown Shortlist tool: {name}"));
    }
    let mut args = Vec::new();
    if let Some(Value::Object(map)) = arguments {
        for (key, value) in map {
            if value.is_null() {
                continue;
            }
            let value = match value {
                Value::String(value) => value.clone(),
                Value::Bool(value) => {
                    if *value {
                        "1".into()
                    } else {
                        "0".into()
                    }
                }
                Value::Number(value) => value.to_string(),
                Value::Array(items) => items
                    .iter()
                    .map(|item| match item {
                        Value::String(value) => Ok(value.clone()),
                        Value::Number(value) => Ok(value.to_string()),
                        _ => Err(format!("{key} must be an array of strings")),
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .join(","),
                Value::Object(_) => return Err(format!("{key} must be a scalar")),
                Value::Null => continue,
            };
            args.push((key.clone(), value));
        }
    }
    Ok(Call { tool, args })
}

fn tool_definitions() -> Vec<Value> {
    let text = |description: &str| json!({ "type": "string", "description": description });
    let integer = |description: &str| json!({ "type": "integer", "description": description });
    let number = |description: &str| json!({ "type": "number", "description": description });
    let strings = |description: &str| {
        json!({
            "type": "array",
            "items": { "type": "string" },
            "description": description
        })
    };
    vec![
        json!({
            "name": "stats",
            "description": "Library overview: candidate count, experience range, top skills, locations and file types. No resume bodies.",
            "inputSchema": {
                "type": "object",
                "properties": { "n": integer("How many top skills/locations to include (default 12).") },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "find",
            "description": "Lexical browse/search. Exact query match count, paged. Returns name|title|yrs|loc|skills rows. Use for lists; use screen for semantic screening.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "q": text("Search text."),
                    "skills": strings("Required skills (all must match)."),
                    "any": strings("Nice-to-have skills (any match)."),
                    "miny": number("Minimum years."),
                    "maxy": number("Maximum years."),
                    "loc": text("Location contains this text."),
                    "n": integer("Rows to return, 1–30."),
                    "p": integer("Page number, starting at 1."),
                    "sort": { "type": "string", "enum": ["rel", "yrs", "name"], "description": "Sort order." }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "screen",
            "description": "Semantic shortlist. Applies skills, years and location, adds seniority/recency/role relevance, and returns name|pre|req|any|yrs|loc|skills|why. `pre` is a retrieval pre-score: inspect profiles with get before deciding.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "q": text("What the role needs, e.g. \"rust payments backend\"."),
                    "skills": strings("Required skills."),
                    "any": strings("Nice-to-have skills."),
                    "miny": number("Minimum years."),
                    "loc": text("Location contains this text."),
                    "n": integer("Rows to return, 1–25.")
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "get",
            "description": "One candidate's compact profile, including roles/experience, education, certifications and recruiter notes. Never the full resume. Prefer the full candidate name for id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": text("Candidate full name (preferred) or handle."),
                    "f": strings("Fields: headline, yrs, loc, skills, summary, experience, education, certifications, email, phone, notes, text."),
                    "chars": integer("Character cap for summary/text.")
                },
                "required": ["id"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "read",
            "description": "Matching windows from one resume. Use after screen/get when a specific claim needs evidence.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": text("Candidate full name (preferred) or handle."),
                    "q": text("Phrase to find, case-insensitive."),
                    "w": integer("Window size in characters (default 120)."),
                    "n": integer("Number of windows (default 3).")
                },
                "required": ["id", "q"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "grep",
            "description": "Matching snippets across candidates. Narrow with ids/handles when possible.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "q": text("Phrase to find."),
                    "ids": strings("Optional candidate full names."),
                    "w": integer("Window size in characters."),
                    "n": integer("Maximum snippets.")
                },
                "required": ["q"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "note",
            "description": "Read the recruiter's note for a candidate. Model writes to notes are disabled.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": text("Candidate handle or full name.") },
                "required": ["id"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "compare",
            "description": "One aligned row per candidate for 2–6 people. Fields can include yrs, loc, skills, experience, summary, education, certifications, email, notes.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ids": strings("Candidate full names, 2–6."),
                    "f": strings("Fields to compare."),
                    "len": integer("Character cap per field.")
                },
                "required": ["ids"],
                "additionalProperties": false
            }
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_native_names_and_arrays() {
        let call = to_call(
            "shortlist_screen",
            Some(&json!({ "q": "rust", "skills": ["Rust", "Go"], "n": 3 })),
        )
        .unwrap();
        assert_eq!(call.tool, "shortlist");
        assert_eq!(call.arg("skills"), Some("Rust,Go"));
        assert_eq!(call.arg("n"), Some("3"));
    }

    #[test]
    fn exposes_eight_tools_including_screen() {
        let tools = tool_definitions();
        assert_eq!(tools.len(), 8);
        assert!(tools.iter().any(|tool| tool["name"] == "screen"));
        assert!(tools.iter().any(|tool| tool["name"] == "get"));
    }

    #[test]
    fn tools_run_and_resolve_names() {
        let store = Store::open_in_memory().unwrap();
        store.seed_demo().unwrap();
        let mut server = Server::new(store);
        let found = server
            .run_tool("find", Some(&json!({ "q": "sarah" })))
            .unwrap();
        assert!(found.contains("Sarah Chen"), "{found}");
        let profile = server
            .run_tool("get", Some(&json!({ "id": "Sarah Chen" })))
            .unwrap();
        assert!(profile.contains("experience="), "{profile}");
    }
}
