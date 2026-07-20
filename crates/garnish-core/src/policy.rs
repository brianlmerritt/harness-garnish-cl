use chrono::{Datelike, Timelike};
use serde::{Deserialize, Serialize};

/// Per-project policy (projects.policy_json). See docs/policy-model.md.
/// Unknown keys are rejected at parse time (deny_unknown_fields) rather than
/// silently ignored.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ProjectPolicy {
    pub schedule: SchedulePolicy,
    pub quota: QuotaPolicy,
    pub git: GitPolicy,
    pub autonomy: AutonomyPolicy,
    pub agents: AgentPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SchedulePolicy {
    /// Seven chars, Monday..Sunday. W = working hours only, O = off-hours
    /// only, B = any time, - = never.
    pub week: String,
    /// "HH:MM" 24h local time.
    pub work_start: String,
    pub work_end: String,
}

impl Default for SchedulePolicy {
    fn default() -> Self {
        Self {
            week: "BBBBBBB".into(),
            work_start: "09:00".into(),
            work_end: "17:30".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct QuotaPolicy {
    pub reserve_pct_session: f64,
    pub reserve_pct_weekly: f64,
    pub max_task_minutes: u32,
    /// fail_closed | fail_open
    pub unknown_quota: String,
}

impl Default for QuotaPolicy {
    fn default() -> Self {
        Self {
            reserve_pct_session: 15.0,
            reserve_pct_weekly: 20.0,
            max_task_minutes: 45,
            unknown_quota: "fail_closed".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GitPolicy {
    pub task_branches: bool,
    /// never | approval
    pub push: String,
    pub pr: String,
    pub merge: String,
    pub branch_prefix: String,
}

impl Default for GitPolicy {
    fn default() -> Self {
        Self {
            task_branches: true,
            push: "never".into(),
            pr: "never".into(),
            merge: "never".into(),
            branch_prefix: "garnish/".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AutonomyPolicy {
    /// Risk classes <= this run unattended (inside the sandbox).
    pub auto_class_max: u8,
    /// off | allowlist
    pub network_default: String,
}

impl Default for AutonomyPolicy {
    fn default() -> Self {
        Self {
            auto_class_max: 1,
            network_default: "off".into(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AgentPolicy {
    /// Adapter names, e.g. ["fake", "claude-code", "codex"]. Empty = any.
    pub allowed: Vec<String>,
    pub pinned: Option<String>,
}

impl ProjectPolicy {
    pub fn parse(json: &str) -> anyhow::Result<Self> {
        if json.trim().is_empty() || json.trim() == "{}" {
            return Ok(Self::default());
        }
        let p: Self = serde_json::from_str(json)?;
        p.validate()?;
        Ok(p)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        let week = &self.schedule.week;
        if week.len() != 7 || !week.chars().all(|c| matches!(c, 'W' | 'O' | 'B' | '-')) {
            anyhow::bail!(
                "schedule.week must be 7 chars of W/O/B/- (Mon..Sun), got {week:?}"
            );
        }
        parse_hhmm(&self.schedule.work_start)?;
        parse_hhmm(&self.schedule.work_end)?;
        if !matches!(self.quota.unknown_quota.as_str(), "fail_closed" | "fail_open") {
            anyhow::bail!("quota.unknown_quota must be fail_closed or fail_open");
        }
        for (name, v) in [
            ("git.push", &self.git.push),
            ("git.pr", &self.git.pr),
            ("git.merge", &self.git.merge),
        ] {
            if !matches!(v.as_str(), "never" | "approval") {
                anyhow::bail!("{name} must be never or approval");
            }
        }
        Ok(())
    }

    /// May a task for this project START at `t` (local time)?
    /// Returns Ok(()) or an explanation of why not.
    pub fn schedule_allows(&self, t: chrono::DateTime<chrono::Local>) -> Result<(), String> {
        let day_idx = t.weekday().num_days_from_monday() as usize;
        let code = self.schedule.week.chars().nth(day_idx).unwrap_or('B');
        let minutes = (t.hour() * 60 + t.minute()) as i64;
        let start = parse_hhmm(&self.schedule.work_start).unwrap_or(9 * 60);
        let end = parse_hhmm(&self.schedule.work_end).unwrap_or(17 * 60 + 30);
        let in_work = minutes >= start && minutes < end;
        match code {
            'B' => Ok(()),
            'W' if in_work => Ok(()),
            'W' => Err(format!(
                "schedule {}: day {} allows working hours only ({}-{})",
                self.schedule.week, day_idx + 1, self.schedule.work_start, self.schedule.work_end
            )),
            'O' if !in_work => Ok(()),
            'O' => Err(format!(
                "schedule {}: day {} allows off-hours only (outside {}-{})",
                self.schedule.week, day_idx + 1, self.schedule.work_start, self.schedule.work_end
            )),
            '-' => Err(format!("schedule {}: day {} disallows all work", self.schedule.week, day_idx + 1)),
            _ => Ok(()),
        }
    }

    /// Does this risk tier need a human approval before running?
    pub fn needs_approval(&self, risk_tier: u8) -> bool {
        risk_tier > self.autonomy.auto_class_max
    }

    pub fn agent_allowed(&self, adapter: &str) -> bool {
        self.agents.allowed.is_empty() || self.agents.allowed.iter().any(|a| a == adapter)
    }
}

fn parse_hhmm(s: &str) -> anyhow::Result<i64> {
    let (h, m) = s
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("bad time {s:?}, want HH:MM"))?;
    let h: i64 = h.parse()?;
    let m: i64 = m.parse()?;
    if !(0..24).contains(&h) || !(0..60).contains(&m) {
        anyhow::bail!("bad time {s:?}");
    }
    Ok(h * 60 + m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(weekday_offset_from_mon: i64, hh: u32, mm: u32) -> chrono::DateTime<chrono::Local> {
        // 2026-07-20 is a Monday.
        chrono::Local
            .with_ymd_and_hms(2026, 7, 20 + weekday_offset_from_mon as u32, hh, mm, 0)
            .unwrap()
    }

    #[test]
    fn wob_schedule() {
        let mut p = ProjectPolicy::default();
        p.schedule.week = "WWWOOBB".into();
        // Monday 10:00 — W day, working hours: allowed.
        assert!(p.schedule_allows(at(0, 10, 0)).is_ok());
        // Monday 20:00 — W day, off hours: denied.
        assert!(p.schedule_allows(at(0, 20, 0)).is_err());
        // Thursday 10:00 — O day, working hours: denied.
        assert!(p.schedule_allows(at(3, 10, 0)).is_err());
        // Thursday 20:00 — O day, off hours: allowed.
        assert!(p.schedule_allows(at(3, 20, 0)).is_ok());
        // Saturday any time — B: allowed.
        assert!(p.schedule_allows(at(5, 3, 0)).is_ok());
    }

    #[test]
    fn never_day() {
        let mut p = ProjectPolicy::default();
        p.schedule.week = "-BBBBBB".into();
        assert!(p.schedule_allows(at(0, 10, 0)).is_err());
    }

    #[test]
    fn rejects_bad_week() {
        let p = ProjectPolicy {
            schedule: SchedulePolicy {
                week: "WWX".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(ProjectPolicy::parse(r#"{"surprise": true}"#).is_err());
    }

    #[test]
    fn approval_gate() {
        let p = ProjectPolicy::default();
        assert!(!p.needs_approval(0));
        assert!(!p.needs_approval(1));
        assert!(p.needs_approval(2));
        assert!(p.needs_approval(3));
    }
}
