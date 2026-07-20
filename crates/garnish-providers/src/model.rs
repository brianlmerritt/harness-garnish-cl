//! API/local model providers (ADR-0007). One internal message shape; each
//! provider maps it to its wire format. API billing is tracked in the cost
//! ledger and never mixed with subscription quota.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

/// Provider-neutral conversation turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Turn {
    User(String),
    Assistant { text: String, tool_calls: Vec<ToolCall> },
    ToolResult { call_id: String, name: String, output: String },
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub system: String,
    pub turns: Vec<Turn>,
    pub tools: Vec<ToolDef>,
    pub max_tokens: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
}

pub trait ModelProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn complete(&self, req: &ChatRequest) -> Result<ChatResponse>;
}

/// Provider selection for the API agent, from environment:
/// GARNISH_API_PROVIDER = anthropic | openai | openai-compat | fake
/// GARNISH_API_BASE_URL   (openai-compat: Ollama/llama.cpp/OpenRouter etc.)
/// GARNISH_API_KEY_ENV    (name of the env var holding the key; defaults
///                         ANTHROPIC_API_KEY / OPENAI_API_KEY)
pub fn model_provider_from_env() -> Result<Box<dyn ModelProvider>> {
    let which = std::env::var("GARNISH_API_PROVIDER").unwrap_or_default();
    match which.as_str() {
        "anthropic" => Ok(Box::new(AnthropicProvider::from_env())),
        "openai" => Ok(Box::new(OpenAiProvider::openai())),
        "openai-compat" => {
            let base = std::env::var("GARNISH_API_BASE_URL")
                .context("openai-compat needs GARNISH_API_BASE_URL")?;
            Ok(Box::new(OpenAiProvider::compat(&base)))
        }
        "fake" => Ok(Box::new(FakeModelProvider)),
        other => anyhow::bail!(
            "GARNISH_API_PROVIDER must be anthropic|openai|openai-compat|fake (got {other:?})"
        ),
    }
}

fn key_from_env(default_var: &str) -> Result<String> {
    let var = std::env::var("GARNISH_API_KEY_ENV").unwrap_or_else(|_| default_var.to_string());
    std::env::var(&var).with_context(|| format!("API key env var {var} not set"))
}

// ---------- Anthropic ----------

pub struct AnthropicProvider {
    pub base_url: String,
}

impl AnthropicProvider {
    pub fn from_env() -> Self {
        Self {
            base_url: std::env::var("GARNISH_API_BASE_URL")
                .unwrap_or_else(|_| "https://api.anthropic.com".into()),
        }
    }

    pub fn build_body(req: &ChatRequest) -> serde_json::Value {
        let mut messages = vec![];
        for turn in &req.turns {
            match turn {
                Turn::User(text) => messages.push(serde_json::json!({ "role": "user", "content": text })),
                Turn::Assistant { text, tool_calls } => {
                    let mut content = vec![];
                    if !text.is_empty() {
                        content.push(serde_json::json!({ "type": "text", "text": text }));
                    }
                    for c in tool_calls {
                        content.push(serde_json::json!({
                            "type": "tool_use", "id": c.id, "name": c.name, "input": c.input
                        }));
                    }
                    messages.push(serde_json::json!({ "role": "assistant", "content": content }));
                }
                Turn::ToolResult { call_id, output, .. } => messages.push(serde_json::json!({
                    "role": "user",
                    "content": [{ "type": "tool_result", "tool_use_id": call_id, "content": output }]
                })),
            }
        }
        serde_json::json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "system": req.system,
            "messages": messages,
            "tools": req.tools.iter().map(|t| serde_json::json!({
                "name": t.name, "description": t.description, "input_schema": t.input_schema
            })).collect::<Vec<_>>(),
        })
    }

    pub fn parse_response(body: &serde_json::Value) -> Result<ChatResponse> {
        let mut text = String::new();
        let mut tool_calls = vec![];
        for block in body["content"].as_array().cloned().unwrap_or_default() {
            match block["type"].as_str() {
                Some("text") => text.push_str(block["text"].as_str().unwrap_or_default()),
                Some("tool_use") => tool_calls.push(ToolCall {
                    id: block["id"].as_str().unwrap_or_default().into(),
                    name: block["name"].as_str().unwrap_or_default().into(),
                    input: block["input"].clone(),
                }),
                _ => {}
            }
        }
        Ok(ChatResponse {
            text,
            tool_calls,
            usage: Usage {
                input_tokens: body["usage"]["input_tokens"].as_u64().unwrap_or(0),
                output_tokens: body["usage"]["output_tokens"].as_u64().unwrap_or(0),
                cache_read_tokens: body["usage"]["cache_read_input_tokens"].as_u64().unwrap_or(0),
            },
        })
    }
}

impl ModelProvider for AnthropicProvider {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn complete(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let key = key_from_env("ANTHROPIC_API_KEY")?;
        let resp = ureq::post(&format!("{}/v1/messages", self.base_url))
            .set("x-api-key", &key)
            .set("anthropic-version", "2023-06-01")
            .set("content-type", "application/json")
            .timeout(std::time::Duration::from_secs(180))
            .send_json(Self::build_body(req));
        let body: serde_json::Value = match resp {
            Ok(r) => r.into_json()?,
            Err(ureq::Error::Status(code, r)) => {
                let text = r.into_string().unwrap_or_default();
                anyhow::bail!("anthropic API {code}: {text}");
            }
            Err(e) => return Err(e).context("anthropic API transport error"),
        };
        Self::parse_response(&body)
    }
}

// ---------- OpenAI and OpenAI-compatible ----------

pub struct OpenAiProvider {
    pub base_url: String,
    pub label: &'static str,
    pub default_key_var: &'static str,
}

impl OpenAiProvider {
    pub fn openai() -> Self {
        Self {
            base_url: std::env::var("GARNISH_API_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".into()),
            label: "openai",
            default_key_var: "OPENAI_API_KEY",
        }
    }

    /// Ollama, llama.cpp server, OpenRouter — same wire format, different
    /// base URL; key optional for local endpoints.
    pub fn compat(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            label: "openai-compat",
            default_key_var: "OPENAI_API_KEY",
        }
    }

    pub fn build_body(req: &ChatRequest) -> serde_json::Value {
        let mut messages = vec![serde_json::json!({ "role": "system", "content": req.system })];
        for turn in &req.turns {
            match turn {
                Turn::User(text) => messages.push(serde_json::json!({ "role": "user", "content": text })),
                Turn::Assistant { text, tool_calls } => {
                    let mut m = serde_json::json!({ "role": "assistant", "content": text });
                    if !tool_calls.is_empty() {
                        m["tool_calls"] = tool_calls.iter().map(|c| serde_json::json!({
                            "id": c.id, "type": "function",
                            "function": { "name": c.name, "arguments": c.input.to_string() }
                        })).collect();
                    }
                    messages.push(m);
                }
                Turn::ToolResult { call_id, output, .. } => messages.push(serde_json::json!({
                    "role": "tool", "tool_call_id": call_id, "content": output
                })),
            }
        }
        serde_json::json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "messages": messages,
            "tools": req.tools.iter().map(|t| serde_json::json!({
                "type": "function",
                "function": { "name": t.name, "description": t.description, "parameters": t.input_schema }
            })).collect::<Vec<_>>(),
        })
    }

    pub fn parse_response(body: &serde_json::Value) -> Result<ChatResponse> {
        let msg = &body["choices"][0]["message"];
        let tool_calls = msg["tool_calls"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|c| ToolCall {
                id: c["id"].as_str().unwrap_or_default().into(),
                name: c["function"]["name"].as_str().unwrap_or_default().into(),
                input: serde_json::from_str(c["function"]["arguments"].as_str().unwrap_or("{}"))
                    .unwrap_or(serde_json::Value::Null),
            })
            .collect();
        Ok(ChatResponse {
            text: msg["content"].as_str().unwrap_or_default().to_string(),
            tool_calls,
            usage: Usage {
                input_tokens: body["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
                output_tokens: body["usage"]["completion_tokens"].as_u64().unwrap_or(0),
                cache_read_tokens: body["usage"]["prompt_tokens_details"]["cached_tokens"]
                    .as_u64()
                    .unwrap_or(0),
            },
        })
    }
}

impl ModelProvider for OpenAiProvider {
    fn name(&self) -> &'static str {
        self.label
    }

    fn complete(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let mut call = ureq::post(&format!("{}/chat/completions", self.base_url))
            .set("content-type", "application/json")
            .timeout(std::time::Duration::from_secs(180));
        // Local endpoints (Ollama, llama.cpp) usually need no key.
        if let Ok(key) = key_from_env(self.default_key_var) {
            call = call.set("authorization", &format!("Bearer {key}"));
        }
        let body: serde_json::Value = match call.send_json(Self::build_body(req)) {
            Ok(r) => r.into_json()?,
            Err(ureq::Error::Status(code, r)) => {
                let text = r.into_string().unwrap_or_default();
                anyhow::bail!("{} API {code}: {text}", self.label);
            }
            Err(e) => return Err(e).context("openai-family API transport error"),
        };
        Self::parse_response(&body)
    }
}

// ---------- deterministic fake (tests) ----------

/// Scripted provider: call 1 issues a write_file tool call derived from a
/// `write-file:<name>:<content>` goal in the last user turn; call 2 finishes.
pub struct FakeModelProvider;

impl ModelProvider for FakeModelProvider {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn complete(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let already_wrote = req.turns.iter().any(|t| matches!(t, Turn::ToolResult { .. }));
        let usage = Usage { input_tokens: 120, output_tokens: 30, cache_read_tokens: 0 };
        if already_wrote {
            return Ok(ChatResponse { text: "Done.".into(), tool_calls: vec![], usage });
        }
        let goal = req
            .turns
            .iter()
            .find_map(|t| match t {
                Turn::User(u) => Some(u.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let (name, content) = goal
            .strip_prefix("write-file:")
            .and_then(|r| r.split_once(':'))
            .unwrap_or(("GARNISH_API_FAKE.txt", "fake"));
        Ok(ChatResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                name: "write_file".into(),
                input: serde_json::json!({ "path": name, "content": content }),
            }],
            usage,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> ChatRequest {
        ChatRequest {
            model: "m".into(),
            system: "sys".into(),
            turns: vec![
                Turn::User("hello".into()),
                Turn::Assistant {
                    text: "".into(),
                    tool_calls: vec![ToolCall {
                        id: "c1".into(),
                        name: "write_file".into(),
                        input: serde_json::json!({"path": "a"}),
                    }],
                },
                Turn::ToolResult { call_id: "c1".into(), name: "write_file".into(), output: "ok".into() },
            ],
            tools: vec![ToolDef {
                name: "write_file".into(),
                description: "d".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }],
            max_tokens: 100,
        }
    }

    #[test]
    fn anthropic_wire_roundtrip() {
        let body = AnthropicProvider::build_body(&req());
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "c1");

        let resp = AnthropicProvider::parse_response(&serde_json::json!({
            "content": [
                {"type": "text", "text": "hi"},
                {"type": "tool_use", "id": "t1", "name": "write_file", "input": {"path": "x"}}
            ],
            "usage": {"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 3}
        }))
        .unwrap();
        assert_eq!(resp.text, "hi");
        assert_eq!(resp.tool_calls[0].name, "write_file");
        assert_eq!(resp.usage.cache_read_tokens, 3);
    }

    #[test]
    fn openai_wire_roundtrip() {
        let body = OpenAiProvider::build_body(&req());
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][3]["role"], "tool");
        assert_eq!(body["tools"][0]["type"], "function");

        let resp = OpenAiProvider::parse_response(&serde_json::json!({
            "choices": [{"message": {"content": null, "tool_calls": [
                {"id": "t1", "type": "function", "function": {"name": "write_file", "arguments": "{\"path\":\"x\"}"}}
            ]}}],
            "usage": {"prompt_tokens": 9, "completion_tokens": 4}
        }))
        .unwrap();
        assert_eq!(resp.tool_calls[0].input["path"], "x");
        assert_eq!(resp.usage.input_tokens, 9);
    }

    #[test]
    fn fake_provider_scripts_write_then_done() {
        let f = FakeModelProvider;
        let mut r = req();
        r.turns = vec![Turn::User("write-file:hello.txt:hi".into())];
        let first = f.complete(&r).unwrap();
        assert_eq!(first.tool_calls[0].input["path"], "hello.txt");
        r.turns.push(Turn::ToolResult { call_id: "call_1".into(), name: "write_file".into(), output: "ok".into() });
        let second = f.complete(&r).unwrap();
        assert!(second.tool_calls.is_empty());
    }
}
