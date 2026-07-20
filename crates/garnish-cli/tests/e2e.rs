//! Phase 1 end-to-end tests: create task -> route -> execute in isolated
//! worktree -> verify -> present patch. Uses the fake agent and fake backend
//! only — consumes no provider quota (CI-safe).

use std::path::{Path, PathBuf};
use std::process::Command;

struct Env {
    data_dir: tempfile::TempDir,
    repo: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let e = Self {
            data_dir: tempfile::tempdir().unwrap(),
            repo: tempfile::tempdir().unwrap(),
        };
        for args in [
            vec!["init", "-b", "main"],
            vec!["-c", "user.name=t", "-c", "user.email=t@t", "commit", "--allow-empty", "-m", "init"],
        ] {
            let out = Command::new("git").args(&args).current_dir(e.repo.path()).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        }
        e
    }

    fn garnish(&self, args: &[&str]) -> (bool, String) {
        self.garnish_with(args, &[])
    }

    fn garnish_with(&self, args: &[&str], extra_env: &[(&str, &str)]) -> (bool, String) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_garnish"));
        cmd.args(args)
            .env("GARNISH_DATA_DIR", self.data_dir.path())
            .env("GARNISH_FAKE_AGENT_BIN", fake_agent_bin());
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), text)
    }

    fn add_project(&self, name: &str) {
        let (ok, out) = self.garnish(&[
            "project", "add", "--name", name,
            "--path", self.repo.path().to_str().unwrap(),
        ]);
        assert!(ok, "project add failed: {out}");
    }

    fn add_task(&self, extra: &[&str]) -> String {
        let mut args = vec![
            "task", "add", "--project", "demo", "--title", "make hello",
            "--goal", "write-file:hello.txt:hi from fake agent",
            "--criterion", "hello.txt exists",
            "--verify", "test -f hello.txt",
        ];
        args.extend_from_slice(extra);
        let (ok, out) = self.garnish(&args);
        assert!(ok, "task add failed: {out}");
        out.split_whitespace().nth(1).unwrap().to_string()
    }

    fn task_status(&self, id: &str) -> String {
        let (ok, out) = self.garnish(&["task", "show", id, "--json"]);
        assert!(ok, "task show failed: {out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        v["task"]["status"].as_str().unwrap().to_string()
    }
}

fn fake_agent_bin() -> PathBuf {
    // Built by cargo before this package's integration tests run.
    PathBuf::from(env!("CARGO_BIN_EXE_fake-agent"))
}

fn repo_is_untouched(repo: &Path) {
    let out = Command::new("git").args(["status", "--porcelain"]).current_dir(repo).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "", "user checkout was modified");
    assert!(!repo.join("hello.txt").exists(), "task output leaked into the user checkout");
}

#[test]
fn happy_path_to_review_with_patch() {
    let env = Env::new();
    env.add_project("demo");
    let task = env.add_task(&[]);

    let (ok, out) = env.garnish(&["task", "run", &task, "--backend", "fake"]);
    assert!(ok, "task run failed: {out}");
    assert!(out.contains("VERIFIED"), "expected verification pass: {out}");
    assert_eq!(env.task_status(&task), "review");

    // Evidence: patch, verification, events, handoff, projections.
    let runs_dir = env.repo.path().join(".harness-garnish/runs");
    let run_dir = std::fs::read_dir(&runs_dir).unwrap().next().unwrap().unwrap().path();
    let patch = std::fs::read_to_string(run_dir.join("patch.diff")).unwrap();
    assert!(patch.contains("hello.txt"));
    let verification: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("verification.json")).unwrap()).unwrap();
    assert_eq!(verification["passed"], true);
    assert!(run_dir.join("events.jsonl").exists());
    assert!(env.repo.path().join(".harness-garnish/TASKS.md").exists());
    assert!(env.repo.path().join(".harness-garnish/HANDOFF.md").exists());

    // The task branch exists but the user's checkout is untouched
    // (.harness-garnish projections are generated files, ignored here).
    let out = Command::new("git").args(["branch", "--list", &format!("garnish/{task}")])
        .current_dir(env.repo.path()).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("garnish/"));
    assert!(!env.repo.path().join("hello.txt").exists());

    // Event chain intact.
    let (ok, out) = env.garnish(&["events", "verify"]);
    assert!(ok && out.contains("event chain OK"), "{out}");
}

#[test]
fn lying_agent_is_caught_by_verifier() {
    let env = Env::new();
    env.add_project("demo");
    let task = env.add_task(&[]);
    let (ok, out) = env.garnish_with(
        &["task", "run", &task, "--backend", "fake"],
        &[("GARNISH_FAKE_MODE", "lie")],
    );
    assert!(ok, "{out}");
    assert!(out.contains("verification FAILED"), "verifier believed a lying agent: {out}");
    assert_eq!(env.task_status(&task), "ready", "should return to ready with retry budget left");
}

#[test]
fn risk_tier_2_requires_approval_then_runs() {
    let env = Env::new();
    env.add_project("demo");
    let task = env.add_task(&["--risk", "2"]);

    let (ok, out) = env.garnish(&["task", "run", &task, "--backend", "fake"]);
    assert!(ok, "{out}");
    assert!(out.contains("requires approval"), "{out}");
    assert_eq!(env.task_status(&task), "awaiting_approval");

    let (ok, out) = env.garnish(&["approval", "list", "--json"]);
    assert!(ok, "{out}");
    let approvals: serde_json::Value = serde_json::from_str(&out).unwrap();
    let approval_id = approvals[0]["id"].as_str().unwrap().to_string();

    let (ok, out) = env.garnish(&["approval", "approve", &approval_id]);
    assert!(ok && out.contains("approved"), "{out}");

    let (ok, out) = env.garnish(&["task", "run", &task, "--backend", "fake"]);
    assert!(ok, "{out}");
    assert_eq!(env.task_status(&task), "review", "approved task should complete: {out}");
}

#[test]
fn denied_approval_blocks_task() {
    let env = Env::new();
    env.add_project("demo");
    let task = env.add_task(&["--risk", "3"]);
    env.garnish(&["task", "run", &task, "--backend", "fake"]);
    let (_, out) = env.garnish(&["approval", "list", "--json"]);
    let approvals: serde_json::Value = serde_json::from_str(&out).unwrap();
    let id = approvals[0]["id"].as_str().unwrap().to_string();
    env.garnish(&["approval", "deny", &id]);
    let (ok, _) = env.garnish(&["task", "run", &task, "--backend", "fake"]);
    assert!(ok);
    assert_eq!(env.task_status(&task), "blocked");
}

#[test]
fn schedule_never_day_refuses_start() {
    let env = Env::new();
    let (ok, out) = env.garnish(&[
        "project", "add", "--name", "demo",
        "--path", env.repo.path().to_str().unwrap(),
        "--schedule=-------",
    ]);
    assert!(ok, "{out}");
    let task = env.add_task(&[]);
    let (ok, out) = env.garnish(&["task", "run", &task, "--backend", "fake"]);
    assert!(ok, "{out}");
    assert!(out.contains("not started"), "{out}");
    assert_eq!(env.task_status(&task), "ready");
    repo_is_untouched(env.repo.path());
}

#[test]
fn cancellation_stops_running_agent() {
    let env = Env::new();
    env.add_project("demo");
    let task = env.add_task(&[]);

    // Start a run whose fake agent sleeps for 300s, then cancel it.
    let mut child = Command::new(env!("CARGO_BIN_EXE_garnish"))
        .args(["task", "run", &task, "--backend", "fake"])
        .env("GARNISH_DATA_DIR", env.data_dir.path())
        .env("GARNISH_FAKE_AGENT_BIN", fake_agent_bin())
        .env("GARNISH_FAKE_MODE", "sleep")
        .spawn()
        .unwrap();

    // Give it time to reach `running`, then request cancellation.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(300));
        if env.task_status(&task) == "running" || std::time::Instant::now() > deadline {
            break;
        }
    }
    let (ok, out) = env.garnish(&["task", "cancel", &task]);
    assert!(ok, "{out}");

    let start = std::time::Instant::now();
    let status = child.wait().unwrap();
    assert!(status.success());
    assert!(
        start.elapsed() < std::time::Duration::from_secs(30),
        "cancellation took too long"
    );
    assert_eq!(env.task_status(&task), "cancelled");
}

/// Regression (found on the Ubuntu VPS): an agent that probes OK but fails
/// to spawn must fail the task immediately, not strand it in `running`
/// until the lease expires.
#[test]
fn unspawnable_agent_fails_task_immediately() {
    let env = Env::new();
    env.add_project("demo");
    let task = env.add_task(&[]);
    // Exists (probe passes) but is not executable (spawn fails).
    let dud = env.data_dir.path().join("dud-agent");
    std::fs::write(&dud, "not a binary").unwrap();
    let (ok, out) = env.garnish_with(
        &["task", "run", &task, "--backend", "fake"],
        &[("GARNISH_FAKE_AGENT_BIN", dud.to_str().unwrap())],
    );
    assert!(!ok, "run should report the spawn error: {out}");
    assert_eq!(
        env.task_status(&task),
        "failed",
        "task must fail cleanly, never hang in running: {out}"
    );
}

#[test]
fn dependent_task_waits_for_dependency() {
    let env = Env::new();
    env.add_project("demo");
    let first = env.add_task(&[]);
    let second = env.add_task(&["--depends-on", &first]);
    let (ok, out) = env.garnish(&["task", "run", &second, "--backend", "fake"]);
    assert!(ok, "{out}");
    assert!(out.contains("unmet dependencies"), "{out}");
    assert_eq!(env.task_status(&second), "ready");
}
