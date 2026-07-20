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
    /// Err = not usable on this machine.
    fn probe(&self) -> Result<String>;
    fn build_invocation(&self, goal: &str) -> Result<Invocation>;
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
            return Ok(PathBuf::from(p));
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

pub fn adapter_by_name(name: &str) -> Result<Box<dyn AgentAdapter>> {
    match name {
        "fake" => Ok(Box::new(FakeAdapter::new(
            &std::env::var("GARNISH_FAKE_MODE").unwrap_or_else(|_| "ok".into()),
        ))),
        "claude-code" => Ok(Box::new(ClaudeAdapter)),
        other => anyhow::bail!("unknown adapter: {other} (fake|claude-code)"),
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
}
