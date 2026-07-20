use anyhow::Result;
use std::path::PathBuf;

pub struct Invocation {
    pub argv: Vec<String>,
    pub extra_env: Vec<(String, String)>,
}

/// Versioned agent adapter (docs/contracts.md). Phase 1 carries the minimum:
/// probe, headless invocation, event parsing. Resume/fork/PTY land later.
pub trait AgentAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    /// CodexBar provider id whose subscription this adapter consumes; None =
    /// quota-exempt (deterministic local agent). Drives the routing gate.
    fn quota_provider(&self) -> Option<&'static str> {
        None
    }
    /// Err = not usable on this machine.
    fn probe(&self) -> Result<String>;
    fn build_invocation(&self, goal: &str) -> Result<Invocation>;
    /// Provider-reported usage (tokens/cost) from the run's events, if the
    /// CLI emits it. Never used to infer remaining subscription quota.
    fn extract_usage(&self, _events: &[serde_json::Value]) -> Option<serde_json::Value> {
        None
    }
    /// Best-effort structured events from captured stdout (JSONL where the
    /// CLI supports it). Non-JSON lines are wrapped as {"type":"raw"}.
    fn parse_events(&self, stdout: &str) -> Vec<serde_json::Value> {
        stdout
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str(l)
                    .unwrap_or_else(|_| serde_json::json!({ "type": "raw", "line": l }))
            })
            .collect()
    }
}

// ---------- fake agent (deterministic, quota-free) ----------

/// Drives the `fake-agent` binary built from this crate. Behaviour is set via
/// GARNISH_FAKE_MODE: ok | fail | sleep | lie (claims success, does nothing).
pub struct FakeAdapter {
    pub mode: String,
}

impl FakeAdapter {
    pub fn new(mode: &str) -> Self {
        Self { mode: mode.into() }
    }

    fn binary() -> Result<PathBuf> {
        if let Ok(p) = std::env::var("GARNISH_FAKE_AGENT_BIN") {
            let p = PathBuf::from(p);
            anyhow::ensure!(p.exists(), "GARNISH_FAKE_AGENT_BIN points at missing file: {}", p.display());
            return Ok(p);
        }
        let exe = std::env::current_exe()?;
        for ancestor in [exe.parent(), exe.parent().and_then(|p| p.parent())]
            .into_iter()
            .flatten()
        {
            let candidate = ancestor.join("fake-agent");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
        anyhow::bail!("fake-agent binary not found (set GARNISH_FAKE_AGENT_BIN)")
    }
}

impl AgentAdapter for FakeAdapter {
    fn name(&self) -> &'static str {
        "fake"
    }

    /// Quota-exempt normally; tests may force a provider id through the
    /// routing gate via GARNISH_FAKE_ADAPTER_QUOTA_PROVIDER.
    fn quota_provider(&self) -> Option<&'static str> {
        static FORCED: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();
        *FORCED.get_or_init(|| {
            std::env::var("GARNISH_FAKE_ADAPTER_QUOTA_PROVIDER")
                .ok()
                .map(|s| Box::leak(s.into_boxed_str()) as &'static str)
        })
    }

    fn probe(&self) -> Result<String> {
        Self::binary().map(|p| format!("fake-agent at {}", p.display()))
    }

    fn build_invocation(&self, goal: &str) -> Result<Invocation> {
        Ok(Invocation {
            argv: vec![Self::binary()?.to_string_lossy().into_owned(), goal.to_string()],
            extra_env: vec![("GARNISH_FAKE_MODE".into(), self.mode.clone())],
        })
    }
}

// ---------- Claude Code ----------

/// Claude Code headless adapter: `claude -p <goal> --output-format
/// stream-json`. Tested against Claude Code 2.1.x; parser fixtures in
/// fixtures/. Writes are permitted only because the cwd is an isolated task
/// worktree; the CLI's own permission mode is the second fence (ADR-0004).
pub struct ClaudeAdapter;

impl AgentAdapter for ClaudeAdapter {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    fn quota_provider(&self) -> Option<&'static str> {
        Some("claude")
    }

    fn extract_usage(&self, events: &[serde_json::Value]) -> Option<serde_json::Value> {
        let result = events.iter().rev().find(|e| e["type"] == "result")?;
        Some(serde_json::json!({
            "usage": result["usage"],
            "total_cost_usd": result["total_cost_usd"],
            "duration_ms": result["duration_ms"],
        }))
    }

    fn probe(&self) -> Result<String> {
        let out = std::process::Command::new("claude").arg("--version").output()?;
        anyhow::ensure!(out.status.success(), "claude --version failed");
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn build_invocation(&self, goal: &str) -> Result<Invocation> {
        Ok(Invocation {
            argv: vec![
                "claude".into(),
                "-p".into(),
                goal.into(),
                "--output-format".into(),
                "stream-json".into(),
                "--verbose".into(),
                "--permission-mode".into(),
                "acceptEdits".into(),
            ],
            extra_env: vec![],
        })
    }
}

// ---------- Codex CLI ----------

/// Codex headless adapter: `codex exec --json` (JSONL events). Tested against
/// codex-cli 0.144.x; `--sandbox workspace-write` keeps Codex's own fence at
/// the worktree even though garnish already isolates the checkout.
pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn quota_provider(&self) -> Option<&'static str> {
        Some("codex")
    }

    fn probe(&self) -> Result<String> {
        let out = std::process::Command::new("codex").arg("--version").output()?;
        anyhow::ensure!(out.status.success(), "codex --version failed");
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn build_invocation(&self, goal: &str) -> Result<Invocation> {
        Ok(Invocation {
            argv: vec![
                "codex".into(),
                "exec".into(),
                "--json".into(),
                "--sandbox".into(),
                "workspace-write".into(),
                goal.into(),
            ],
            extra_env: vec![],
        })
    }

    fn extract_usage(&self, events: &[serde_json::Value]) -> Option<serde_json::Value> {
        let done = events.iter().rev().find(|e| e["type"] == "turn.completed")?;
        Some(serde_json::json!({ "usage": done["usage"] }))
    }
}

// ---------- Antigravity CLI ----------

/// Antigravity headless adapter: `agy --print` (plain-text output; agy 1.1.x
/// documents no structured event stream, so events are raw lines). Its
/// `--sandbox` flag adds the CLI's own terminal restrictions.
pub struct AntigravityAdapter;

impl AgentAdapter for AntigravityAdapter {
    fn name(&self) -> &'static str {
        "antigravity"
    }

    fn quota_provider(&self) -> Option<&'static str> {
        Some("antigravity")
    }

    fn probe(&self) -> Result<String> {
        let out = std::process::Command::new("agy").arg("--version").output()?;
        anyhow::ensure!(out.status.success(), "agy --version failed");
        Ok(format!("agy {}", String::from_utf8_lossy(&out.stdout).trim()))
    }

    fn build_invocation(&self, goal: &str) -> Result<Invocation> {
        Ok(Invocation {
            argv: vec![
                "agy".into(),
                "--print".into(),
                goal.into(),
                "--sandbox".into(),
                "--print-timeout".into(),
                "30m".into(),
            ],
            extra_env: vec![],
        })
    }
}

pub const ADAPTER_NAMES: &[&str] = &["fake", "claude-code", "codex", "antigravity"];

pub fn adapter_by_name(name: &str) -> Result<Box<dyn AgentAdapter>> {
    match name {
        "fake" => Ok(Box::new(FakeAdapter::new(
            &std::env::var("GARNISH_FAKE_MODE").unwrap_or_else(|_| "ok".into()),
        ))),
        "claude-code" => Ok(Box::new(ClaudeAdapter)),
        "codex" => Ok(Box::new(CodexAdapter)),
        "antigravity" => Ok(Box::new(AntigravityAdapter)),
        other => anyhow::bail!("unknown adapter: {other} ({})", ADAPTER_NAMES.join("|")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_jsonl_and_wraps_raw() {
        let a = ClaudeAdapter;
        let events = a.parse_events("{\"type\":\"system\"}\nnot json\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["type"], "system");
        assert_eq!(events[1]["type"], "raw");
    }

    #[test]
    fn claude_argv_has_no_shell() {
        let inv = ClaudeAdapter.build_invocation("do; rm -rf /").unwrap();
        // The goal travels as a single argv element, never through a shell.
        assert_eq!(inv.argv[2], "do; rm -rf /");
    }

    #[test]
    fn codex_fixture_parses_and_yields_usage() {
        let fixture = include_str!("../fixtures/codex-exec.jsonl");
        let a = CodexAdapter;
        let events = a.parse_events(fixture);
        assert_eq!(events.len(), 6);
        assert!(events.iter().all(|e| e["type"] != "raw"), "codex JSONL drifted");
        let usage = a.extract_usage(&events).unwrap();
        assert_eq!(usage["usage"]["output_tokens"], 182);
    }

    #[test]
    fn claude_fixture_parses_and_yields_usage() {
        let fixture = include_str!("../fixtures/claude-stream.jsonl");
        let a = ClaudeAdapter;
        let events = a.parse_events(fixture);
        assert!(events.iter().all(|e| e["type"] != "raw"), "claude stream-json drifted");
        let usage = a.extract_usage(&events).unwrap();
        assert_eq!(usage["total_cost_usd"], 0.0213);
        assert_eq!(usage["usage"]["output_tokens"], 98);
    }

    #[test]
    fn quota_provider_mapping() {
        assert_eq!(ClaudeAdapter.quota_provider(), Some("claude"));
        assert_eq!(CodexAdapter.quota_provider(), Some("codex"));
        assert_eq!(AntigravityAdapter.quota_provider(), Some("antigravity"));
    }

    #[test]
    fn antigravity_goal_is_single_argv_element() {
        let inv = AntigravityAdapter.build_invocation("a $(b) `c`").unwrap();
        assert_eq!(inv.argv[2], "a $(b) `c`");
    }
}
