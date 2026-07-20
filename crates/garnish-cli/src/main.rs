mod daemon;
mod runner;
mod web;

use anyhow::Result;
use clap::{Parser, Subcommand};
use garnish_core::{db, events, paths, policy::ProjectPolicy, projections, state, store, TaskSpec, TaskStatus};

#[derive(Parser)]
#[command(name = "garnish", version, about = "Harness Garnish — local control plane for AI-assisted development")]
struct Cli {
    /// Emit JSON instead of human output where supported.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create the data directory and database.
    Init,
    /// Probe platform, engines, agent CLIs, and quota tooling.
    Doctor,
    #[command(subcommand)]
    Project(ProjectCmd),
    #[command(subcommand)]
    Task(TaskCmd),
    #[command(subcommand)]
    Approval(ApprovalCmd),
    #[command(subcommand)]
    Events(EventsCmd),
    #[command(subcommand)]
    Daemon(DaemonCmd),
    /// Remove worktrees of finished tasks and stale verifier checkouts.
    Gc,
    #[command(subcommand)]
    Quota(QuotaCmd),
    #[command(subcommand)]
    Profile(ProfileCmd),
    /// API cost ledger, aggregated per project/day/provider/model.
    Cost {
        #[arg(long)]
        project: Option<String>,
    },
    /// Serve the web UX on loopback (bearer-token auth; port 0 = random).
    Web {
        #[arg(long, default_value_t = 4180)]
        port: u16,
    },
    #[command(subcommand)]
    Config(ConfigCmd),
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Show a project's effective policy field by field, with provenance
    /// (built-in default vs project override).
    Explain {
        #[arg(long)]
        project: String,
    },
}

#[derive(Subcommand)]
enum QuotaCmd {
    /// Show remaining-quota snapshots (source: codexbar or fake).
    Status {
        /// Provider id, e.g. claude, codex, antigravity.
        #[arg(long, default_value = "claude")]
        provider: String,
    },
}

#[derive(Subcommand)]
enum ProfileCmd {
    /// Register an account profile for a provider (auth stays with the CLI's
    /// own login; garnish stores only a reference).
    Add {
        #[arg(long)]
        provider: String,
        #[arg(long)]
        name: String,
        /// Optional JSON config (e.g. {"codexbar_account": "work"}).
        #[arg(long, default_value = "{}")]
        config: String,
    },
    List,
}

#[derive(Subcommand)]
enum DaemonCmd {
    /// Run the daemon in the foreground.
    Run {
        #[arg(long, default_value = "fake")]
        backend: String,
        #[arg(long, default_value = "alpine:3.20")]
        image: String,
    },
    /// Start the daemon detached (logs to the data dir).
    Start {
        #[arg(long, default_value = "fake")]
        backend: String,
        #[arg(long, default_value = "alpine:3.20")]
        image: String,
    },
    Stop,
    Status,
    /// Stop leasing new tasks (running task finishes/pauses normally).
    PauseAll,
    ResumeAll,
}

#[derive(Subcommand)]
enum ProjectCmd {
    Add {
        #[arg(long)]
        name: String,
        #[arg(long)]
        path: String,
        #[arg(long, default_value = "normal")]
        kind: String,
        /// W/O/B/- for Mon..Sun, e.g. WWWOOBB.
        #[arg(long)]
        schedule: Option<String>,
        /// Allowed adapters in preference order, comma-separated.
        #[arg(long)]
        agents: Option<String>,
        /// Full policy JSON (overrides --schedule/--agents).
        #[arg(long)]
        policy_json: Option<String>,
    },
    List,
    Show { name: String },
}

#[derive(Subcommand)]
enum TaskCmd {
    Add {
        #[arg(long)]
        project: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        goal: String,
        /// Repeatable acceptance criterion.
        #[arg(long = "criterion", required = true)]
        criteria: Vec<String>,
        /// Repeatable verification command (whitespace-split argv; no shell).
        #[arg(long = "verify", required = true)]
        verify: Vec<String>,
        #[arg(long, default_value_t = 1)]
        risk: u8,
        #[arg(long)]
        pin: Option<String>,
        /// Repeatable dependency task id.
        #[arg(long = "depends-on")]
        depends_on: Vec<String>,
    },
    List {
        #[arg(long)]
        project: Option<String>,
    },
    Show { id: String },
    Run {
        id: String,
        #[arg(long)]
        adapter: Option<String>,
        #[arg(long, default_value = "fake")]
        backend: String,
        #[arg(long)]
        timeout_min: Option<u32>,
        #[arg(long, default_value = "alpine:3.20")]
        image: String,
    },
    Cancel { id: String },
    /// Return a failed task to ready (consumes no retry budget).
    Retry { id: String },
    /// Pause: a ready task immediately; a running task at the next safe
    /// point, with a handoff packet.
    Pause { id: String },
    /// Return a paused task to ready.
    Resume { id: String },
}

#[derive(Subcommand)]
enum ApprovalCmd {
    List,
    Approve { id: String },
    Deny { id: String },
}

#[derive(Subcommand)]
enum EventsCmd {
    /// Verify the tamper-evident event hash chain.
    Verify,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let conn = db::open(&paths::db_path())?;
    match cli.cmd {
        Cmd::Init => {
            println!("initialised {}", paths::db_path().display());
        }
        Cmd::Doctor => doctor(cli.json)?,
        Cmd::Project(c) => project_cmd(&conn, c, cli.json)?,
        Cmd::Task(c) => task_cmd(&conn, c, cli.json).await?,
        Cmd::Approval(c) => approval_cmd(&conn, c, cli.json)?,
        Cmd::Events(EventsCmd::Verify) => {
            let n = events::verify_chain(&conn)?;
            println!("event chain OK ({n} events)");
        }
        Cmd::Daemon(c) => match c {
            DaemonCmd::Run { backend, image } => {
                daemon::run(daemon::DaemonOptions { backend, image, poll: std::time::Duration::from_secs(5) }).await?;
            }
            DaemonCmd::Start { backend, image } => {
                daemon::start(&daemon::DaemonOptions { backend, image, poll: std::time::Duration::from_secs(5) })?;
            }
            DaemonCmd::Stop => daemon::stop()?,
            DaemonCmd::Status => daemon::status()?,
            DaemonCmd::PauseAll => {
                store::control_set(&conn, "pause_all", "1")?;
                println!("pause-all set: daemon will not lease new tasks");
            }
            DaemonCmd::ResumeAll => {
                store::control_set(&conn, "pause_all", "0")?;
                println!("pause-all cleared");
            }
        },
        Cmd::Gc => daemon::gc(&conn)?,
        Cmd::Quota(QuotaCmd::Status { provider }) => {
            match garnish_providers::provider_from_env() {
                None => println!("no quota source configured (install codexbar or set GARNISH_QUOTA)"),
                Some(source) => {
                    let snaps = source.snapshot(&provider)?;
                    for s in &snaps {
                        store::quota_snapshot_insert(
                            &conn, &s.provider, &s.window, s.remaining_pct,
                            s.resets_at.as_deref(), &s.source, &s.confidence,
                            s.unknown_reason.as_deref(),
                        )?;
                    }
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&snaps)?);
                    } else {
                        for s in snaps {
                            match s.remaining_pct {
                                Some(pct) => println!(
                                    "{:12} {:8} {:5.1}% remaining  resets {}",
                                    s.provider, s.window, pct, s.resets_at.as_deref().unwrap_or("-")
                                ),
                                None => println!(
                                    "{:12} {:8} unknown ({})",
                                    s.provider, s.window,
                                    s.unknown_reason.as_deref().unwrap_or("no reason")
                                ),
                            }
                        }
                    }
                }
            }
        }
        Cmd::Profile(c) => match c {
            ProfileCmd::Add { provider, name, config } => {
                let cfg: serde_json::Value = serde_json::from_str(&config)?;
                let id = store::profile_add(&conn, &provider, &name, &cfg)?;
                println!("profile {provider}/{name} added ({id})");
            }
            ProfileCmd::List => {
                for (id, provider, name) in store::profile_list(&conn)? {
                    println!("{id}  {provider}/{name}");
                }
            }
        },
        Cmd::Cost { project } => {
            let pid = match project {
                Some(name) => Some(store::project_get(&conn, &name)?.id),
                None => None,
            };
            let rows = store::cost_summary(&conn, pid.as_deref())?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else if rows.is_empty() {
                println!("no API costs recorded");
            } else {
                let mut total = 0.0;
                for r in &rows {
                    let usd = r["usd"].as_f64();
                    total += usd.unwrap_or(0.0);
                    println!(
                        "{}  {:12} {:14} {:28} in {:>9} out {:>8} cache {:>9}  {}",
                        r["date"].as_str().unwrap_or("-"),
                        r["project"].as_str().unwrap_or("-"),
                        r["provider"].as_str().unwrap_or("-"),
                        r["model"].as_str().unwrap_or("-"),
                        r["input_tokens"], r["output_tokens"], r["cache_tokens"],
                        match usd {
                            Some(v) => format!("${v:.4}"),
                            None => "unpriced".to_string(),
                        },
                    );
                }
                println!("total priced: ${total:.4}");
            }
        }
        Cmd::Web { port } => web::serve(port).await?,
        Cmd::Config(ConfigCmd::Explain { project }) => {
            let p = store::project_get(&conn, &project)?;
            let defaults = flatten(&serde_json::to_value(ProjectPolicy::default())?);
            let effective = flatten(&serde_json::to_value(&p.policy)?);
            if cli.json {
                let rows: Vec<serde_json::Value> = effective
                    .iter()
                    .map(|(k, v)| {
                        serde_json::json!({
                            "field": k, "value": v,
                            "source": if defaults.get(k) == Some(v) { "default" } else { "project" },
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                println!("effective policy for project {} ({}):", p.name, p.id);
                for (k, v) in &effective {
                    let source = if defaults.get(k) == Some(v) { "default" } else { "PROJECT " };
                    println!("  {source:8} {k} = {v}");
                }
                println!("precedence: built-in defaults -> project policy (task overrides not yet implemented)");
            }
        }
    }
    Ok(())
}

/// Flatten a policy document to sorted dot-path -> value pairs.
fn flatten(v: &serde_json::Value) -> std::collections::BTreeMap<String, serde_json::Value> {
    fn walk(prefix: &str, v: &serde_json::Value, out: &mut std::collections::BTreeMap<String, serde_json::Value>) {
        match v {
            serde_json::Value::Object(map) => {
                for (k, child) in map {
                    let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                    walk(&path, child, out);
                }
            }
            other => {
                out.insert(prefix.to_string(), other.clone());
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk("", v, &mut out);
    out
}

fn probe_version(cmd: &str, args: &[&str]) -> Option<String> {
    std::process::Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        })
}

fn doctor(json: bool) -> Result<()> {
    let checks: Vec<(&str, Option<String>)> = vec![
        ("git", probe_version("git", &["--version"])),
        ("docker", probe_version("docker", &["--version"])),
        ("podman", probe_version("podman", &["--version"])),
        ("claude", probe_version("claude", &["--version"])),
        ("codex", probe_version("codex", &["--version"])),
        ("agy", probe_version("agy", &["--version"])),
        ("codexbar", probe_version("codexbar", &["--version"])),
        ("tmux", probe_version("tmux", &["-V"])),
        ("aoe", probe_version("aoe", &["--version"])),
    ];
    if json {
        let map: serde_json::Map<String, serde_json::Value> = checks
            .iter()
            .map(|(k, v)| {
                (k.to_string(), v.clone().map(serde_json::Value::String).unwrap_or(serde_json::Value::Null))
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&map)?);
    } else {
        println!("garnish doctor — platform {} / data dir {}", std::env::consts::OS, paths::data_dir().display());
        for (name, v) in checks {
            match v {
                Some(v) => println!("  ok       {name:9} {v}"),
                None => println!("  missing  {name:9} (not found or not runnable)"),
            }
        }
        println!("note: subscription CLIs run on the host confined to task worktrees;\n      container isolation applies to build/test/verification commands (ADR-0004)");
    }
    Ok(())
}

fn project_cmd(conn: &rusqlite::Connection, c: ProjectCmd, json: bool) -> Result<()> {
    match c {
        ProjectCmd::Add { name, path, kind, schedule, agents, policy_json } => {
            let root = std::fs::canonicalize(&path)?;
            anyhow::ensure!(root.join(".git").exists(), "{} is not a git repository", root.display());
            let mut policy = match policy_json {
                Some(j) => ProjectPolicy::parse(&j)?,
                None => ProjectPolicy::default(),
            };
            if let Some(s) = schedule {
                policy.schedule.week = s;
            }
            if let Some(a) = agents {
                policy.agents.allowed = a.split(',').map(|s| s.trim().to_string()).collect();
            }
            policy.validate()?;
            let p = store::project_add(conn, &name, &root.to_string_lossy(), &kind, &policy)?;
            // Keep generated projections out of the user's `git status` via
            // .git/info/exclude — never by editing user-owned files.
            let exclude = root.join(".git/info/exclude");
            let existing = std::fs::read_to_string(&exclude).unwrap_or_default();
            if !existing.lines().any(|l| l.trim() == ".harness-garnish/") {
                std::fs::create_dir_all(exclude.parent().unwrap())?;
                std::fs::write(&exclude, format!("{existing}\n.harness-garnish/\n"))?;
            }
            projections::write_all(conn, &p)?;
            println!("project {} added ({})", p.name, p.id);
        }
        ProjectCmd::List => {
            let projects = store::project_list(conn)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&projects)?);
            } else {
                for p in projects {
                    println!("{}  {}  {} [{}]", p.id, p.name, p.root_path, p.kind);
                }
            }
        }
        ProjectCmd::Show { name } => {
            let p = store::project_get(conn, &name)?;
            println!("{}", serde_json::to_string_pretty(&p)?);
        }
    }
    Ok(())
}

async fn task_cmd(conn: &rusqlite::Connection, c: TaskCmd, json: bool) -> Result<()> {
    match c {
        TaskCmd::Add { project, title, goal, criteria, verify, risk, pin, depends_on } => {
            let p = store::project_get(conn, &project)?;
            let spec = TaskSpec {
                goal,
                rationale: String::new(),
                scope: vec![],
                non_scope: vec![],
                acceptance_criteria: criteria,
                verification_commands: verify
                    .iter()
                    .map(|v| v.split_whitespace().map(String::from).collect())
                    .collect(),
                risk_tier: risk,
                estimated_minutes: 15,
                checkpointable: false,
                allowed_agents: vec![],
                pinned_agent: pin,
            };
            let t = store::task_add(conn, &p.id, &title, &spec)?;
            for dep in depends_on {
                store::dep_add(conn, &t.id, &dep)?;
            }
            projections::write_all(conn, &p)?;
            println!("task {} added (status ready)", t.id);
        }
        TaskCmd::List { project } => {
            let pid = match project {
                Some(name) => Some(store::project_get(conn, &name)?.id),
                None => None,
            };
            let tasks = store::task_list(conn, pid.as_deref())?;
            if json {
                println!("{}", serde_json::to_string_pretty(&tasks)?);
            } else {
                for t in tasks {
                    println!("{}  [{:17}]  {}", t.id, t.status.as_str(), t.title);
                }
            }
        }
        TaskCmd::Show { id } => {
            let t = store::task_get(conn, &id)?;
            let runs = store::run_list(conn, &id)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "task": t, "runs": runs }))?);
            } else {
                println!("{}", serde_json::to_string_pretty(&t)?);
                for r in runs {
                    println!("run {} [{}] {} -> {:?}  evidence: {}", r.id, r.mode, r.backend, r.exit_status, r.evidence_dir);
                }
            }
        }
        TaskCmd::Run { id, adapter, backend, timeout_min, image } => {
            let opts = runner::RunOptions {
                adapter_override: adapter,
                backend,
                timeout_min,
                image,
                external_cancel: None,
            };
            runner::run_task(conn, &id, &opts).await?;
        }
        TaskCmd::Cancel { id } => {
            store::task_request_cancel(conn, &id)?;
            println!("cancellation requested for {id} (a running agent stops within ~1s)");
        }
        TaskCmd::Retry { id } => {
            let t = store::task_get(conn, &id)?;
            state::transition(conn, &id, t.status, TaskStatus::Ready, "manual retry")?;
            println!("task {id} -> ready");
        }
        TaskCmd::Pause { id } => {
            let t = store::task_get(conn, &id)?;
            if t.status == TaskStatus::Running {
                store::task_request_pause(conn, &id)?;
                println!("pause requested for running task {id}; it stops at the next safe point with a handoff");
            } else {
                state::transition(conn, &id, t.status, TaskStatus::Paused, "manual pause")?;
                println!("task {id} -> paused");
            }
        }
        TaskCmd::Resume { id } => {
            let t = store::task_get(conn, &id)?;
            state::transition(conn, &id, t.status, TaskStatus::Ready, "manual resume")?;
            println!("task {id} -> ready (worktree and evidence preserved)");
        }
    }
    Ok(())
}

fn approval_cmd(conn: &rusqlite::Connection, c: ApprovalCmd, json: bool) -> Result<()> {
    match c {
        ApprovalCmd::List => {
            let pending = store::approval_list_pending(conn)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&pending)?);
            } else if pending.is_empty() {
                println!("no pending approvals");
            } else {
                for a in pending {
                    println!("{}  task={:?}  expires={}\n  {}", a.id, a.task_id, a.expires_at, a.action);
                }
            }
        }
        ApprovalCmd::Approve { id } => {
            let status = store::approval_decide(conn, &id, true, "cli")?;
            println!("approval {id}: {status}");
        }
        ApprovalCmd::Deny { id } => {
            let status = store::approval_decide(conn, &id, false, "cli")?;
            println!("approval {id}: {status}");
        }
    }
    Ok(())
}
