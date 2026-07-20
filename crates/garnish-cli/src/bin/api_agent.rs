//! garnish-api-agent: a minimal, deterministic tool loop around an API/local
//! model (ADR-0007). Spawned and supervised by the garnish runner exactly
//! like a vendor CLI: cwd = task worktree, JSONL events on stdout, exit code
//! is the contract. Deliberately NOT a full coding harness — three file
//! tools inside the worktree, no shell, bounded iterations.
//!
//! Config via env: GARNISH_API_PROVIDER (anthropic|openai|openai-compat|fake),
//! GARNISH_API_MODEL, GARNISH_API_BASE_URL, GARNISH_API_KEY_ENV.

use garnish_providers::{model_provider_from_env, ChatRequest, ToolDef, Turn, Usage};
use std::path::{Component, Path};

const MAX_TURNS: usize = 20;
const MAX_FILE_BYTES: u64 = 512 * 1024;

fn emit(v: serde_json::Value) {
    println!("{v}");
}

fn tools() -> Vec<ToolDef> {
    let path_schema = |desc: &str| {
        serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string", "description": desc } },
            "required": ["path"]
        })
    };
    vec![
        ToolDef {
            name: "list_files".into(),
            description: "List files in the working directory (recursive, relative paths).".into(),
            input_schema: serde_json::json!({ "type": "object", "properties": {} }),
        },
        ToolDef {
            name: "read_file".into(),
            description: "Read a text file (relative path inside the working directory).".into(),
            input_schema: path_schema("relative file path"),
        },
        ToolDef {
            name: "write_file".into(),
            description: "Create or overwrite a text file (relative path inside the working directory).".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        },
    ]
}

/// Reject absolute paths, traversal, and symlink escapes from the worktree.
fn safe_path(raw: &str) -> Result<std::path::PathBuf, String> {
    let p = Path::new(raw);
    if p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(format!("path {raw:?} escapes the working directory"));
    }
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let joined = cwd.join(p);
    if let Ok(canon) = joined.canonicalize() {
        // Existing file: its real location must stay inside the worktree.
        let cwd_canon = cwd.canonicalize().map_err(|e| e.to_string())?;
        if !canon.starts_with(&cwd_canon) {
            return Err(format!("path {raw:?} resolves outside the working directory"));
        }
    }
    Ok(joined)
}

fn run_tool(name: &str, input: &serde_json::Value) -> String {
    match name {
        "list_files" => {
            let mut out = vec![];
            let mut stack = vec![std::path::PathBuf::from(".")];
            while let Some(dir) = stack.pop() {
                if let Ok(entries) = std::fs::read_dir(&dir) {
                    for e in entries.flatten() {
                        let p = e.path();
                        let name = p.strip_prefix("./").unwrap_or(&p).display().to_string();
                        if name.starts_with(".git") {
                            continue;
                        }
                        if p.is_dir() {
                            stack.push(p);
                        } else {
                            out.push(name);
                        }
                        if out.len() >= 500 {
                            out.push("[truncated at 500 entries]".into());
                            return out.join("\n");
                        }
                    }
                }
            }
            if out.is_empty() { "(empty)".into() } else { out.join("\n") }
        }
        "read_file" => match safe_path(input["path"].as_str().unwrap_or_default()) {
            Err(e) => format!("error: {e}"),
            Ok(p) => match std::fs::metadata(&p) {
                Err(e) => format!("error: {e}"),
                Ok(m) if m.len() > MAX_FILE_BYTES => "error: file too large".into(),
                Ok(_) => std::fs::read_to_string(&p).unwrap_or_else(|e| format!("error: {e}")),
            },
        },
        "write_file" => match safe_path(input["path"].as_str().unwrap_or_default()) {
            Err(e) => format!("error: {e}"),
            Ok(p) => {
                let content = input["content"].as_str().unwrap_or_default();
                if let Some(parent) = p.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match std::fs::write(&p, content) {
                    Ok(()) => format!("wrote {} bytes", content.len()),
                    Err(e) => format!("error: {e}"),
                }
            }
        },
        other => format!("error: unknown tool {other}"),
    }
}

fn main() {
    let goal = std::env::args().nth(1).unwrap_or_default();
    if goal.is_empty() {
        eprintln!("usage: garnish-api-agent <goal>");
        std::process::exit(64);
    }
    let provider = match model_provider_from_env() {
        Ok(p) => p,
        Err(e) => {
            emit(serde_json::json!({ "type": "error", "message": e.to_string() }));
            std::process::exit(69);
        }
    };
    let model = std::env::var("GARNISH_API_MODEL").unwrap_or_else(|_| "fake-model".into());
    emit(serde_json::json!({ "type": "start", "provider": provider.name(), "model": model, "goal": goal }));

    let mut turns = vec![Turn::User(goal)];
    let mut total = Usage::default();
    let system = "You are a focused coding agent working in the current directory (an isolated git worktree). \
                  Use the tools to complete the goal exactly; do not invent extra work. \
                  When the goal is complete, reply with a short summary and no tool calls."
        .to_string();

    for _ in 0..MAX_TURNS {
        let req = ChatRequest {
            model: model.clone(),
            system: system.clone(),
            turns: turns.clone(),
            tools: tools(),
            max_tokens: 4096,
        };
        let resp = match provider.complete(&req) {
            Ok(r) => r,
            Err(e) => {
                emit(serde_json::json!({ "type": "error", "message": e.to_string() }));
                std::process::exit(1);
            }
        };
        total.input_tokens += resp.usage.input_tokens;
        total.output_tokens += resp.usage.output_tokens;
        total.cache_read_tokens += resp.usage.cache_read_tokens;

        if resp.tool_calls.is_empty() {
            emit(serde_json::json!({
                "type": "result", "status": "success", "summary": resp.text,
                "provider": provider.name(), "model": model,
                "usage": total,
            }));
            return;
        }
        turns.push(Turn::Assistant { text: resp.text.clone(), tool_calls: resp.tool_calls.clone() });
        for call in &resp.tool_calls {
            let output = run_tool(&call.name, &call.input);
            emit(serde_json::json!({
                "type": "action", "tool": call.name, "input": call.input,
                "output_preview": output.chars().take(200).collect::<String>(),
            }));
            turns.push(Turn::ToolResult {
                call_id: call.id.clone(),
                name: call.name.clone(),
                output,
            });
        }
    }
    emit(serde_json::json!({
        "type": "error", "message": format!("gave up after {MAX_TURNS} turns"),
        "provider": provider.name(), "model": model, "usage": total,
    }));
    std::process::exit(1);
}
