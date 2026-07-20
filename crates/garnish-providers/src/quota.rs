use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    Session,
    Weekly,
}

impl Window {
    pub fn as_str(&self) -> &'static str {
        match self {
            Window::Session => "session",
            Window::Weekly => "weekly",
        }
    }
}

/// One quota observation. `remaining_pct == None` means unknown, with the
/// reason attached — never silently 100% (ADR-0003).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaSnapshot {
    pub provider: String,
    pub window: String,
    pub remaining_pct: Option<f64>,
    pub resets_at: Option<String>,
    pub source: String,
    pub updated_at: Option<String>,
    pub confidence: String, // high | low | unknown
    pub unknown_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub enum GuardDecision {
    Safe { remaining_pct: f64 },
    Below { remaining_pct: Option<f64>, resets_at: Option<String> },
    Unknown { reason: String },
}

pub trait QuotaProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn guard(&self, provider: &str, window: Window, min_remaining_pct: f64) -> Result<GuardDecision>;
    fn snapshot(&self, provider: &str) -> Result<Vec<QuotaSnapshot>>;
}

/// Select the quota source: GARNISH_QUOTA = codexbar | fake | off.
/// Default: codexbar when the binary exists, otherwise none (quota state is
/// then `unknown` and per-project policy decides).
pub fn provider_from_env() -> Option<Box<dyn QuotaProvider>> {
    match std::env::var("GARNISH_QUOTA").as_deref() {
        Ok("off") => None,
        Ok("fake") => Some(Box::new(FakeQuota)),
        Ok("codexbar") => Some(Box::new(CodexBar)),
        _ => {
            let found = Command::new("codexbar").arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
            found.then(|| Box::new(CodexBar) as Box<dyn QuotaProvider>)
        }
    }
}

// ---------- CodexBar (ADR-0003) ----------

/// Shells to `codexbar guard`/`codexbar usage`. Stable guard exit codes:
/// 0 = safe, 1 = below threshold, 64 = bad args, 69 = unavailable.
pub struct CodexBar;

impl QuotaProvider for CodexBar {
    fn name(&self) -> &'static str {
        "codexbar"
    }

    fn guard(&self, provider: &str, window: Window, min_remaining_pct: f64) -> Result<GuardDecision> {
        let out = Command::new("codexbar")
            .args([
                "guard", "--provider", provider,
                "--min-remaining", &format!("{}", min_remaining_pct.round() as i64),
                "--window", window.as_str(),
                "--json", "--timeout", "45",
            ])
            .output()
            .context("running codexbar guard")?;
        let body: serde_json::Value =
            serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null);
        let remaining = body["remainingPercent"].as_f64();
        match out.status.code() {
            Some(0) => Ok(GuardDecision::Safe { remaining_pct: remaining.unwrap_or(min_remaining_pct) }),
            Some(1) => Ok(GuardDecision::Below {
                remaining_pct: remaining,
                resets_at: self
                    .snapshot(provider)
                    .ok()
                    .and_then(|snaps| snaps.into_iter().find(|s| s.window == window.as_str()).and_then(|s| s.resets_at)),
            }),
            Some(69) => Ok(GuardDecision::Unknown {
                reason: body["unavailableReason"].as_str().unwrap_or("quota unavailable").to_string(),
            }),
            other => Ok(GuardDecision::Unknown { reason: format!("codexbar guard exit {other:?}") }),
        }
    }

    fn snapshot(&self, provider: &str) -> Result<Vec<QuotaSnapshot>> {
        let out = Command::new("codexbar")
            .args(["usage", "--provider", provider, "--json"])
            .output()
            .context("running codexbar usage")?;
        let body: serde_json::Value = serde_json::from_slice(&out.stdout).context("parsing codexbar usage JSON")?;
        Ok(parse_usage(provider, &body))
    }
}

/// Parse the `codexbar usage --json` array (fixture-tested; the parser is
/// deliberately tolerant — formats drift, ADR-0003).
pub fn parse_usage(provider: &str, body: &serde_json::Value) -> Vec<QuotaSnapshot> {
    let mut snaps = vec![];
    let entries = body.as_array().cloned().unwrap_or_default();
    for entry in entries {
        if let Some(err) = entry.get("error").filter(|e| !e.is_null()) {
            snaps.push(QuotaSnapshot {
                provider: provider.into(),
                window: "session".into(),
                remaining_pct: None,
                resets_at: None,
                source: "codexbar".into(),
                updated_at: None,
                confidence: "unknown".into(),
                unknown_reason: Some(err["message"].as_str().unwrap_or("provider error").to_string()),
            });
            continue;
        }
        let usage = &entry["usage"];
        let updated = usage["updatedAt"].as_str().map(String::from);
        for (window_name, key) in [("session", "primary"), ("weekly", "secondary")] {
            let w = &usage[key];
            if w.is_null() {
                continue;
            }
            let used = w["usedPercent"].as_f64();
            snaps.push(QuotaSnapshot {
                provider: provider.into(),
                window: window_name.into(),
                remaining_pct: used.map(|u| (100.0 - u).max(0.0)),
                resets_at: w["resetsAt"].as_str().map(String::from),
                source: "codexbar".into(),
                updated_at: updated.clone(),
                confidence: if used.is_some() { "high".into() } else { "unknown".into() },
                unknown_reason: used.is_none().then(|| "no usedPercent in window".into()),
            });
        }
    }
    if snaps.is_empty() {
        snaps.push(QuotaSnapshot {
            provider: provider.into(),
            window: "session".into(),
            remaining_pct: None,
            resets_at: None,
            source: "codexbar".into(),
            updated_at: None,
            confidence: "unknown".into(),
            unknown_reason: Some("empty usage output".into()),
        });
    }
    snaps
}

// ---------- fake provider (tests) ----------

/// Configured via GARNISH_FAKE_QUOTA, e.g.
/// {"claude": {"session": {"remaining": 40, "resets_in_secs": 600},
///             "weekly":  {"remaining": 80}}}
/// A provider mapped to "unknown" (or absent) yields Unknown.
/// {"claude": {"session": {"remaining": 40, "stale": true}}} -> Unknown(stale).
pub struct FakeQuota;

impl FakeQuota {
    fn config() -> serde_json::Value {
        std::env::var("GARNISH_FAKE_QUOTA")
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(serde_json::Value::Null)
    }
}

impl QuotaProvider for FakeQuota {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn guard(&self, provider: &str, window: Window, min_remaining_pct: f64) -> Result<GuardDecision> {
        let cfg = Self::config();
        let w = &cfg[provider][window.as_str()];
        if w.is_null() || cfg[provider] == serde_json::json!("unknown") {
            return Ok(GuardDecision::Unknown { reason: format!("no fake quota for {provider}/{}", window.as_str()) });
        }
        if w["stale"].as_bool().unwrap_or(false) {
            return Ok(GuardDecision::Unknown { reason: "snapshot stale".into() });
        }
        let remaining = w["remaining"].as_f64().unwrap_or(0.0);
        let resets_at = w["resets_in_secs"].as_i64().map(|s| {
            (chrono::Utc::now() + chrono::Duration::seconds(s)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        });
        if remaining >= min_remaining_pct {
            Ok(GuardDecision::Safe { remaining_pct: remaining })
        } else {
            Ok(GuardDecision::Below { remaining_pct: Some(remaining), resets_at })
        }
    }

    fn snapshot(&self, provider: &str) -> Result<Vec<QuotaSnapshot>> {
        let cfg = Self::config();
        let mut snaps = vec![];
        for window in ["session", "weekly"] {
            let w = &cfg[provider][window];
            snaps.push(QuotaSnapshot {
                provider: provider.into(),
                window: window.into(),
                remaining_pct: w["remaining"].as_f64(),
                resets_at: None,
                source: "fake".into(),
                updated_at: Some(chrono::Utc::now().to_rfc3339()),
                confidence: if w["remaining"].is_null() { "unknown".into() } else { "high".into() },
                unknown_reason: w["remaining"].is_null().then(|| "not configured".into()),
            });
        }
        Ok(snaps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const USAGE_FIXTURE: &str = include_str!("../fixtures/codexbar-usage-claude.json");
    const USAGE_ERROR_FIXTURE: &str =
        r#"[{"source":"auto","error":{"code":1,"kind":"provider","message":"authentication required"},"provider":"codex"}]"#;

    #[test]
    fn parses_usage_fixture() {
        let body: serde_json::Value = serde_json::from_str(USAGE_FIXTURE).unwrap();
        let snaps = parse_usage("claude", &body);
        let session = snaps.iter().find(|s| s.window == "session").unwrap();
        assert_eq!(session.remaining_pct, Some(56.0));
        assert_eq!(session.resets_at.as_deref(), Some("2026-07-20T20:09:00Z"));
        assert_eq!(session.confidence, "high");
    }

    #[test]
    fn error_entry_becomes_unknown_not_full() {
        let body: serde_json::Value = serde_json::from_str(USAGE_ERROR_FIXTURE).unwrap();
        let snaps = parse_usage("codex", &body);
        assert_eq!(snaps[0].remaining_pct, None, "unknown must never read as remaining quota");
        assert!(snaps[0].unknown_reason.as_deref().unwrap().contains("authentication"));
    }

    #[test]
    fn fake_guard_states() {
        std::env::set_var(
            "GARNISH_FAKE_QUOTA",
            r#"{"p1": {"session": {"remaining": 50}, "weekly": {"remaining": 5}},
                "p2": {"session": {"remaining": 90, "stale": true}}}"#,
        );
        let f = FakeQuota;
        assert!(matches!(f.guard("p1", Window::Session, 20.0).unwrap(), GuardDecision::Safe { .. }));
        assert!(matches!(f.guard("p1", Window::Weekly, 20.0).unwrap(), GuardDecision::Below { .. }));
        assert!(matches!(f.guard("p2", Window::Session, 20.0).unwrap(), GuardDecision::Unknown { .. }));
        assert!(matches!(f.guard("absent", Window::Session, 20.0).unwrap(), GuardDecision::Unknown { .. }));
        std::env::remove_var("GARNISH_FAKE_QUOTA");
    }
}
