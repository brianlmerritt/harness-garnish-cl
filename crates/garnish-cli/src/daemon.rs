use crate::runner::{self, RunOptions};
use anyhow::{Context, Result};
use garnish_core::{paths, state, store, TaskStatus};
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct DaemonOptions {
    pub backend: String,
    pub image: String,
    /// Queue poll interval (env GARNISH_POLL_MS overrides, for tests).
    pub poll: Duration,
}

fn pidfile() -> PathBuf {
    paths::data_dir().join("daemon.pid")
}

pub fn read_pid() -> Option<i32> {
    std::fs::read_to_string(pidfile()).ok()?.trim().parse().ok()
}

fn pid_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Foreground daemon loop: recover -> housekeeping -> pick -> run.
/// Sequential (one task at a time) in Phase 2; SIGTERM/SIGINT pauses any
/// running task with a handoff and exits cleanly.
pub async fn run(opts: DaemonOptions) -> Result<()> {
    if let Some(pid) = read_pid() {
        if pid_alive(pid) {
            anyhow::bail!("daemon already running (pid {pid})");
        }
    }
    std::fs::create_dir_all(paths::data_dir())?;
    std::fs::write(pidfile(), std::process::id().to_string())?;

    let shutdown = Arc::new(AtomicBool::new(false));
    for sig in [tokio::signal::unix::SignalKind::terminate(), tokio::signal::unix::SignalKind::interrupt()] {
        let flag = shutdown.clone();
        let mut stream = tokio::signal::unix::signal(sig)?;
        tokio::spawn(async move {
            stream.recv().await;
            flag.store(true, Ordering::Relaxed);
        });
    }

    let poll = std::env::var("GARNISH_POLL_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(opts.poll);

    println!("garnish daemon running (pid {}, backend {}, poll {:?})", std::process::id(), opts.backend, poll);
    let result = daemon_loop(&opts, poll, &shutdown).await;
    let _ = std::fs::remove_file(pidfile());
    println!("garnish daemon stopped");
    result
}

async fn daemon_loop(opts: &DaemonOptions, poll: Duration, shutdown: &Arc<AtomicBool>) -> Result<()> {
    while !shutdown.load(Ordering::Relaxed) {
        let conn = garnish_core::db::open(&paths::db_path())?;
        let recovered = state::recover_expired_leases(&conn)?;
        for id in &recovered {
            println!("recovered orphaned task {id} -> ready");
        }

        if store::control_get(&conn, "pause_all")?.as_deref() == Some("1") {
            drop(conn);
            tokio::time::sleep(poll).await;
            continue;
        }

        match pick_next(&conn)? {
            Some(task_id) => {
                let run_opts = RunOptions {
                    backend: opts.backend.clone(),
                    image: opts.image.clone(),
                    external_cancel: Some(shutdown.clone()),
                    ..Default::default()
                };
                if let Err(e) = runner::run_task(&conn, &task_id, &run_opts).await {
                    eprintln!("task {task_id}: runner error: {e:#}");
                }
                schedule_retry_if_needed(&conn, &task_id)?;
            }
            None => {
                drop(conn);
                tokio::time::sleep(poll).await;
            }
        }
    }
    Ok(())
}

/// Idle-backlog policy (Phase 2): ready + eligible + deps met + schedule
/// window open + within unattended risk (needs no human approval). Approval-
/// gated tasks are left for interactive `task run`.
fn pick_next(conn: &Connection) -> Result<Option<String>> {
    for task in store::tasks_eligible(conn)? {
        let project = store::project_get(conn, &task.project_id)?;
        if !store::deps_unmet(conn, &task.id)?.is_empty() {
            continue;
        }
        if project.policy.schedule_allows(chrono::Local::now()).is_err() {
            continue;
        }
        if project.policy.needs_approval(task.spec.risk_tier)
            && store::approval_status_for_task(conn, &task.id)?.as_deref() != Some("approved")
        {
            continue;
        }
        return Ok(Some(task.id));
    }
    Ok(None)
}

/// After a failed agent run: bounded retry with exponential backoff.
fn schedule_retry_if_needed(conn: &Connection, task_id: &str) -> Result<()> {
    let task = store::task_get(conn, task_id)?;
    if task.status != TaskStatus::Failed {
        return Ok(());
    }
    let remaining = store::task_decrement_retry(conn, task_id)?;
    if remaining > 0 {
        let attempt = 3 - remaining; // budget starts at 2
        let base: i64 = std::env::var("GARNISH_BACKOFF_BASE_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30);
        let backoff = base.saturating_mul(1 << attempt.min(6));
        state::transition(conn, task_id, TaskStatus::Failed, TaskStatus::Ready, "daemon retry with backoff")?;
        store::task_set_not_before(conn, task_id, backoff)?;
        println!("task {task_id}: retrying in {backoff}s ({remaining} retries left)");
    } else {
        println!("task {task_id}: retry budget exhausted, stays failed");
    }
    Ok(())
}

/// Detached start: spawn `garnish daemon run` with logs in the data dir.
pub fn start(opts: &DaemonOptions) -> Result<()> {
    if let Some(pid) = read_pid() {
        if pid_alive(pid) {
            anyhow::bail!("daemon already running (pid {pid})");
        }
    }
    let log = std::fs::File::create(paths::data_dir().join("daemon.log"))?;
    let exe = std::env::current_exe()?;
    let child = std::process::Command::new(exe)
        .args(["daemon", "run", "--backend", &opts.backend, "--image", &opts.image])
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()
        .context("spawning daemon")?;
    println!("daemon started (pid {}), log: {}", child.id(), paths::data_dir().join("daemon.log").display());
    Ok(())
}

pub fn stop() -> Result<()> {
    match read_pid() {
        Some(pid) if pid_alive(pid) => {
            unsafe { libc::kill(pid, libc::SIGTERM) };
            println!("sent SIGTERM to daemon (pid {pid}); running task will pause with a handoff");
            Ok(())
        }
        _ => {
            let _ = std::fs::remove_file(pidfile());
            println!("daemon not running");
            Ok(())
        }
    }
}

pub fn status() -> Result<()> {
    match read_pid() {
        Some(pid) if pid_alive(pid) => println!("daemon running (pid {pid})"),
        _ => println!("daemon not running"),
    }
    Ok(())
}

/// Garbage collection: worktrees of terminal tasks, stale verify worktrees,
/// and `git worktree prune` per project. Branches are kept — they are the
/// integration evidence.
pub fn gc(conn: &Connection) -> Result<()> {
    let mut removed = 0usize;
    for (task_id, root, wt_path) in store::gc_candidates(conn)? {
        let wt = PathBuf::from(&wt_path);
        if wt.exists() {
            match garnish_exec::git::git(std::path::Path::new(&root), &["worktree", "remove", "--force", &wt_path]) {
                Ok(_) => {
                    removed += 1;
                    garnish_core::events::append(
                        conn, Some(&task_id), None, "cleanup",
                        &serde_json::json!({ "worktree": wt_path }),
                    )?;
                }
                Err(e) => eprintln!("gc: could not remove {wt_path}: {e}"),
            }
        }
    }
    // Stale verifier worktrees (crash leftovers) live under the data dir.
    for project in store::project_list(conn)? {
        let dir = paths::worktrees_dir(&project.id);
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with("verify-") {
                    let p = entry.path();
                    let _ = garnish_exec::git::git(
                        std::path::Path::new(&project.root_path),
                        &["worktree", "remove", "--force", p.to_str().unwrap()],
                    );
                    let _ = std::fs::remove_dir_all(&p);
                    removed += 1;
                }
            }
        }
        let _ = garnish_exec::git::git(std::path::Path::new(&project.root_path), &["worktree", "prune"]);
    }
    println!("gc: removed {removed} worktree(s)");
    Ok(())
}
