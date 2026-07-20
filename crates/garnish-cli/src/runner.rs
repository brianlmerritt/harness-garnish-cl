use anyhow::{Context, Result};
use garnish_core::{ids, paths, projections, state, store, Task, TaskGit, TaskStatus};
use garnish_exec::{backend_by_name, git as gitx, worktree, SandboxSpec, Supervision};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

pub struct RunOptions {
    pub adapter_override: Option<String>,
    pub backend: String,
    pub timeout_min: Option<u32>,
    pub image: String,
    /// Set by the daemon on shutdown: stop the agent and pause (not cancel)
    /// the task, leaving a handoff packet.
    pub external_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            adapter_override: None,
            backend: "fake".into(),
            timeout_min: None,
            image: "alpine:3.20".into(),
            external_cancel: None,
        }
    }
}

/// The Phase 1 vertical slice: route -> worktree -> execute -> verify ->
/// present patch. Approval-gated tasks stop at awaiting_approval and continue
/// on the next `task run` after approval.
pub async fn run_task(conn: &Connection, task_id: &str, opts: &RunOptions) -> Result<()> {
    state::recover_expired_leases(conn)?;
    let mut task = store::task_get(conn, task_id)?;
    let project = store::project_get(conn, &task.project_id)?;
    let policy = &project.policy;
    let repo = PathBuf::from(&project.root_path);

    match task.status {
        TaskStatus::Ready => {}
        TaskStatus::AwaitingApproval => {
            match store::approval_status_for_task(conn, task_id)?.as_deref() {
                Some("approved") => {}
                Some("denied") => {
                    state::transition(conn, task_id, TaskStatus::AwaitingApproval, TaskStatus::Blocked, "approval denied")?;
                    println!("task {task_id}: approval denied -> blocked");
                    return Ok(());
                }
                other => {
                    println!(
                        "task {task_id}: awaiting approval ({}). Use `garnish approval list` / `approve`.",
                        other.unwrap_or("pending")
                    );
                    return Ok(());
                }
            }
        }
        s => anyhow::bail!("task {task_id} is {}, expected ready (or awaiting_approval)", s.as_str()),
    }

    // Gates: dependencies, schedule.
    let unmet = store::deps_unmet(conn, task_id)?;
    if !unmet.is_empty() {
        println!("task {task_id}: blocked on unmet dependencies: {unmet:?}");
        return Ok(());
    }
    if let Err(why) = policy.schedule_allows(chrono::Local::now()) {
        garnish_core::events::append(
            conn, Some(task_id), None, "schedule_denied",
            &serde_json::json!({ "reason": why }),
        )?;
        println!("task {task_id}: not started — {why}");
        return Ok(());
    }

    // Route.
    let adapter_name = route(conn, &task, policy, opts)?;
    let adapter = garnish_agents::adapter_by_name(&adapter_name)?;

    // Lease + state walk.
    let resuming = task.status == TaskStatus::AwaitingApproval;
    // Lease covers the run plus slack; overridable for crash-recovery tests.
    let lease_secs = std::env::var("GARNISH_LEASE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or((opts.timeout_min.unwrap_or(policy.quota.max_task_minutes) as i64) * 60 + 300);
    store::task_lease(conn, task_id, "garnish-cli", lease_secs)?;
    store::task_clear_flags(conn, task_id)?; // stale cancel/pause from earlier runs
    if !resuming {
        state::transition(conn, task_id, TaskStatus::Ready, TaskStatus::Leased, "scheduled by task run")?;
        state::transition(conn, task_id, TaskStatus::Leased, TaskStatus::Planning, "route chosen")?;

        // Approval gate before any execution (Class >= 2).
        if policy.needs_approval(task.spec.risk_tier)
            && store::approval_status_for_task(conn, task_id)?.as_deref() != Some("approved")
        {
            let action = serde_json::json!({
                "action": "run agent task",
                "task": task_id,
                "title": task.title,
                "risk_tier": task.spec.risk_tier,
                "agent": adapter_name,
                "effect": format!("agent runs in isolated worktree of {}", project.name),
                "reversibility": "worktree branch can be discarded",
            });
            store::approval_create(conn, Some(task_id), &action, 24 * 60)?;
            state::transition(conn, task_id, TaskStatus::Planning, TaskStatus::AwaitingApproval, "risk tier requires approval")?;
            write_handoff(conn, &project, task_id, "awaiting approval; next safe action: approve or deny")?;
            println!("task {task_id}: risk tier {} requires approval — created request; run `garnish approval list`", task.spec.risk_tier);
            return Ok(());
        }
    }

    // Worktree (create once; reuse when resuming after approval).
    let git = match &task.git {
        Some(g) => g.clone(),
        None => {
            let wt = worktree::create(&repo, &project.id, task_id, &policy.git.branch_prefix)?;
            let g = TaskGit {
                worktree_path: wt.path.to_string_lossy().into_owned(),
                branch: wt.branch,
                base_commit: wt.base_commit,
                head_commit: None,
            };
            store::task_set_git(conn, task_id, &g)?;
            garnish_core::events::append(
                conn, Some(task_id), None, "worktree_created",
                &serde_json::json!({ "path": g.worktree_path, "branch": g.branch, "base": g.base_commit }),
            )?;
            g
        }
    };
    let wt_path = PathBuf::from(&git.worktree_path);

    let from = if resuming { TaskStatus::AwaitingApproval } else { TaskStatus::Planning };
    state::transition(conn, task_id, from, TaskStatus::Running, "agent starting")?;

    // Run the agent.
    let run_id = ids::new_id();
    let evidence = paths::projection_dir(&repo).join("runs").join(&run_id);
    std::fs::create_dir_all(&evidence)?;
    task = store::task_get(conn, task_id)?;
    let attempt = 3 - task.retry_budget.max(0);
    store::run_create(conn, &run_id, task_id, attempt, "headless", &opts.backend, &evidence.to_string_lossy())?;

    let timeout_min = opts.timeout_min.unwrap_or(policy.quota.max_task_minutes);
    let sup = Supervision {
        timeout: Duration::from_secs(timeout_min as u64 * 60),
        ..Default::default()
    };
    let cancel_flag = sup.cancel.clone();
    let poller_flag = sup.cancel.clone();
    let external = opts.external_cancel.clone();
    let db_path = paths::db_path();
    let cancel_task_id = task_id.to_string();
    let poller = std::thread::spawn(move || {
        let mut ticks: u64 = 0;
        loop {
            std::thread::sleep(Duration::from_millis(500));
            ticks += 1;
            if poller_flag.load(Ordering::Relaxed) {
                break; // runner finished
            }
            if let Some(ext) = &external {
                if ext.load(Ordering::Relaxed) {
                    poller_flag.store(true, Ordering::Relaxed);
                    break;
                }
            }
            if let Ok(c) = garnish_core::db::open(&db_path) {
                if store::task_cancel_requested(&c, &cancel_task_id).unwrap_or(false)
                    || store::task_pause_requested(&c, &cancel_task_id).unwrap_or(false)
                {
                    poller_flag.store(true, Ordering::Relaxed);
                    break;
                }
                if ticks.is_multiple_of(10) {
                    // Heartbeat every ~5s: extend the lease while alive.
                    let _ = store::task_heartbeat(&c, &cancel_task_id, 60);
                }
            }
        }
    });

    let inv = adapter.build_invocation(&task.spec.goal)?;
    garnish_core::events::append(
        conn, Some(task_id), Some(&run_id), "process",
        &serde_json::json!({ "argv": inv.argv, "cwd": wt_path, "phase": "agent" }),
    )?;
    let outcome = garnish_exec::spawn::run_supervised(
        &inv.argv, &wt_path, &inv.extra_env, &evidence, "agent", &sup,
    )
    .await?;
    cancel_flag.store(true, Ordering::Relaxed); // stop poller
    let _ = poller.join();

    // Persist structured agent events.
    let stdout = std::fs::read_to_string(&outcome.stdout_path).unwrap_or_default();
    let events = adapter.parse_events(&stdout);
    let mut jsonl = String::new();
    for e in &events {
        jsonl.push_str(&serde_json::to_string(e)?);
        jsonl.push('\n');
    }
    std::fs::write(evidence.join("events.jsonl"), jsonl)?;

    match outcome.status {
        "ok" => {}
        "cancelled" => {
            // User cancel -> cancelled; pause request or daemon shutdown ->
            // paused with a handoff packet (preemption-safe).
            let user_cancel = store::task_cancel_requested(conn, task_id)?;
            if user_cancel {
                store::run_finish(conn, &run_id, "cancelled", None)?;
                state::transition(conn, task_id, TaskStatus::Running, TaskStatus::Cancelled, "cancel requested")?;
                finish_projections(conn, &project)?;
                println!("task {task_id}: cancelled cleanly (agent process tree stopped)");
            } else {
                store::run_finish(conn, &run_id, "paused", None)?;
                state::transition(conn, task_id, TaskStatus::Running, TaskStatus::Paused, "pause/shutdown requested")?;
                store::task_clear_flags(conn, task_id)?;
                write_handoff(conn, &project, task_id,
                    "paused mid-run; worktree preserved. Next safe action: `garnish task resume` then `task run` (any compatible adapter)")?;
                finish_projections(conn, &project)?;
                println!("task {task_id}: paused (handoff written; worktree preserved)");
            }
            return Ok(());
        }
        other => {
            store::run_finish(conn, &run_id, other, None)?;
            state::transition(conn, task_id, TaskStatus::Running, TaskStatus::Failed, &format!("agent {other}"))?;
            write_handoff(conn, &project, task_id, "agent failed; next safe action: inspect evidence, `garnish task retry`")?;
            finish_projections(conn, &project)?;
            println!(
                "task {task_id}: agent {other} (exit {:?}). Evidence: {}",
                outcome.exit_code,
                evidence.display()
            );
            return Ok(());
        }
    }

    // Commit the worktree result.
    let head = worktree::commit_all(&wt_path, &format!("garnish task {task_id}: {}", task.title))?;
    let mut git = git.clone();
    git.head_commit = Some(head.clone());
    store::task_set_git(conn, task_id, &git)?;
    store::run_finish(conn, &run_id, "ok", None)?;

    // Independent verification in a clean checkout of the produced commit.
    state::transition(conn, task_id, TaskStatus::Running, TaskStatus::Verifying, "agent claims done")?;
    let verdict = verify(conn, task_id, &task, &repo, &project.id, &head, opts, &evidence).await?;

    if verdict {
        state::transition(conn, task_id, TaskStatus::Verifying, TaskStatus::Review, "verification passed")?;
        let patch = worktree::diff(&wt_path, &git.base_commit, &head)?;
        let patch_path = evidence.join("patch.diff");
        std::fs::write(&patch_path, &patch)?;
        let summary = format!(
            "# Task {task_id} — {title}\n\nStatus: review\n\n- branch: `{branch}`\n- base: `{base}`\n- head: `{head}`\n- patch: `{patch_path}`\n- verification: `{ver}`\n\nIntegration is yours: nothing has been pushed or merged.\n",
            title = task.title,
            branch = git.branch,
            base = git.base_commit,
            patch_path = patch_path.display(),
            ver = evidence.join("verification.json").display(),
        );
        std::fs::write(evidence.join("summary.md"), &summary)?;
        write_handoff(conn, &project, task_id, "verified; next safe action: review branch and integrate manually")?;
        finish_projections(conn, &project)?;
        println!("task {task_id}: VERIFIED -> review\n  branch {branch}\n  patch  {p}\n  evidence {e}",
            branch = git.branch, p = patch_path.display(), e = evidence.display());
    } else {
        let remaining = store::task_decrement_retry(conn, task_id)?;
        if remaining > 0 {
            state::transition(conn, task_id, TaskStatus::Verifying, TaskStatus::Ready, "verification failed; retry budget remains")?;
            println!("task {task_id}: verification FAILED — returned to ready ({remaining} retries left)");
        } else {
            state::transition(conn, task_id, TaskStatus::Verifying, TaskStatus::Failed, "verification failed; retries exhausted")?;
            println!("task {task_id}: verification FAILED — no retries left -> failed");
        }
        write_handoff(conn, &project, task_id, "verification failed; next safe action: inspect verification.json")?;
        finish_projections(conn, &project)?;
    }
    Ok(())
}

/// Route selection: explicit override > task pin > policy pin > policy
/// allowlist order > fake. Hard-filtered by the project allowlist; records
/// snapshot + rationale (quota scoring arrives in Phase 3).
fn route(
    conn: &Connection,
    task: &Task,
    policy: &garnish_core::policy::ProjectPolicy,
    opts: &RunOptions,
) -> Result<String> {
    let (candidates, why): (Vec<String>, &str) = if let Some(a) = &opts.adapter_override {
        (vec![a.clone()], "cli override")
    } else if let Some(a) = &task.spec.pinned_agent {
        (vec![a.clone()], "task pin")
    } else if let Some(a) = &policy.agents.pinned {
        (vec![a.clone()], "project pin")
    } else if !policy.agents.allowed.is_empty() {
        (policy.agents.allowed.clone(), "project allowlist order")
    } else {
        (vec!["fake".into()], "default (no allowlist configured)")
    };

    for name in &candidates {
        if !policy.agent_allowed(name) {
            anyhow::bail!("adapter {name} is not allowed by project policy");
        }
        let adapter = garnish_agents::adapter_by_name(name)?;
        match adapter.probe() {
            Ok(version) => {
                store::task_set_route(
                    conn,
                    &task.id,
                    &serde_json::json!({
                        "adapter": name,
                        "version": version,
                        "reason": why,
                        "quota": { "state": "not-evaluated", "note": "quota routing lands in Phase 3" },
                    }),
                )?;
                return Ok(name.clone());
            }
            Err(e) => {
                garnish_core::events::append(
                    conn, Some(&task.id), None, "route",
                    &serde_json::json!({ "adapter": name, "skipped": e.to_string() }),
                )?;
            }
        }
    }
    anyhow::bail!("no usable adapter among {candidates:?} ({why})")
}

/// Run the task's verification commands in a detached clean worktree at
/// `head`, via the selected backend. All commands must exit 0.
#[allow(clippy::too_many_arguments)]
async fn verify(
    conn: &Connection,
    task_id: &str,
    task: &Task,
    repo: &Path,
    project_id: &str,
    head: &str,
    opts: &RunOptions,
    evidence: &Path,
) -> Result<bool> {
    let backend = backend_by_name(&opts.backend)?;
    let verify_path = paths::worktrees_dir(project_id).join(format!("verify-{}", ids::new_id()));
    gitx::git(repo, &["worktree", "add", "--detach", verify_path.to_str().unwrap(), head])
        .context("creating clean verification worktree")?;

    let spec = SandboxSpec::new(&opts.image, &verify_path);
    let mut results = vec![];
    let mut all_ok = true;
    for (i, cmd) in task.spec.verification_commands.iter().enumerate() {
        let sup = Supervision {
            timeout: Duration::from_secs(600),
            ..Default::default()
        };
        let out = backend
            .exec(&spec, cmd, evidence, &format!("verify-{i}"), &sup)
            .await?;
        let ok = out.ok();
        all_ok &= ok;
        results.push(serde_json::json!({
            "argv": cmd,
            "exit_code": out.exit_code,
            "status": out.status,
            "stdout_tail": out.stdout_tail,
        }));
        garnish_core::events::append(
            conn, Some(task_id), None, "test",
            &serde_json::json!({ "argv": cmd, "status": out.status }),
        )?;
    }
    let verification = serde_json::json!({
        "commit": head,
        "backend": backend.kind().as_str(),
        "image": opts.image,
        "clean_worktree": verify_path,
        "passed": all_ok,
        "results": results,
    });
    std::fs::write(
        evidence.join("verification.json"),
        serde_json::to_string_pretty(&verification)?,
    )?;
    let _ = gitx::git(repo, &["worktree", "remove", "--force", verify_path.to_str().unwrap()]);
    Ok(all_ok)
}

fn write_handoff(
    conn: &Connection,
    project: &garnish_core::Project,
    task_id: &str,
    next_action: &str,
) -> Result<()> {
    let task = store::task_get(conn, task_id)?;
    let runs = store::run_list(conn, task_id)?;
    let packet = serde_json::json!({
        "task": task_id,
        "title": task.title,
        "goal": task.spec.goal,
        "acceptance_criteria": task.spec.acceptance_criteria,
        "status": task.status.as_str(),
        "git": task.git,
        "runs": runs.iter().map(|r| serde_json::json!({
            "id": r.id, "mode": r.mode, "backend": r.backend,
            "exit_status": r.exit_status, "evidence": r.evidence_dir,
        })).collect::<Vec<_>>(),
        "next_safe_action": next_action,
        "note": "resume from repository state and this evidence; no hidden conversation state exists",
    });
    let evidence_dir = runs
        .last()
        .map(|r| PathBuf::from(&r.evidence_dir))
        .unwrap_or_else(|| paths::projection_dir(Path::new(&project.root_path)).join("runs"));
    projections::write_handoff(project, &task, &packet, &evidence_dir)?;
    Ok(())
}

fn finish_projections(conn: &Connection, project: &garnish_core::Project) -> Result<()> {
    projections::write_all(conn, project)
}
