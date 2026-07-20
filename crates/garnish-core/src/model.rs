use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub kind: String, // normal | overarching
    pub manifest: serde_json::Value,
    pub policy: crate::policy::ProjectPolicy,
    pub created_at: String,
}

/// Validated task specification (tasks.spec_json).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSpec {
    pub goal: String,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub scope: Vec<String>,
    #[serde(default)]
    pub non_scope: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    /// Commands run by the independent verifier in a clean sandbox. argv arrays.
    pub verification_commands: Vec<Vec<String>>,
    /// 0..=3, see docs/policy-model.md. >=2 requires approval before running.
    #[serde(default)]
    pub risk_tier: u8,
    #[serde(default = "default_est_minutes")]
    pub estimated_minutes: u32,
    #[serde(default)]
    pub checkpointable: bool,
    /// Adapter names the task may run on; empty = project policy decides.
    #[serde(default)]
    pub allowed_agents: Vec<String>,
    /// Manual pin: adapter name that must be used.
    #[serde(default)]
    pub pinned_agent: Option<String>,
}

fn default_est_minutes() -> u32 {
    15
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub spec: TaskSpec,
    pub priority: i64,
    pub status: TaskStatus,
    pub lease_owner: Option<String>,
    pub lease_expires: Option<String>,
    pub retry_budget: i64,
    pub cancel_requested: bool,
    pub git: Option<TaskGit>,
    pub route: Option<serde_json::Value>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskGit {
    pub worktree_path: String,
    pub branch: String,
    pub base_commit: String,
    pub head_commit: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Draft,
    Ready,
    Leased,
    Planning,
    AwaitingApproval,
    Running,
    Verifying,
    Review,
    Completed,
    Paused,
    Blocked,
    Failed,
    Cancelled,
    Superseded,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Draft => "draft",
            TaskStatus::Ready => "ready",
            TaskStatus::Leased => "leased",
            TaskStatus::Planning => "planning",
            TaskStatus::AwaitingApproval => "awaiting_approval",
            TaskStatus::Running => "running",
            TaskStatus::Verifying => "verifying",
            TaskStatus::Review => "review",
            TaskStatus::Completed => "completed",
            TaskStatus::Paused => "paused",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Failed => "failed",
            TaskStatus::Cancelled => "cancelled",
            TaskStatus::Superseded => "superseded",
        }
    }

    pub fn parse(s: &str) -> anyhow::Result<Self> {
        serde_json::from_value(serde_json::Value::String(s.to_string()))
            .map_err(|_| anyhow::anyhow!("unknown task status: {s}"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub task_id: String,
    pub attempt: i64,
    pub mode: String,    // headless | verify
    pub backend: String, // docker | podman | fake | host-cli
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub exit_status: Option<String>, // ok | failed | timeout | cancelled | crashed
    pub usage: Option<serde_json::Value>,
    pub evidence_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Approval {
    pub id: String,
    pub task_id: Option<String>,
    pub requested_at: String,
    pub action: serde_json::Value,
    pub expires_at: String,
    pub status: String, // pending | approved | denied | expired | revoked
    pub decided_at: Option<String>,
    pub decided_via: Option<String>,
}
