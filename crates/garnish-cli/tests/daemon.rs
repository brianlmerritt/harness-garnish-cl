//! Phase 2 failure-injection and restart tests: daemon scheduling, crash
//! recovery via lease expiry, pause-all, graceful shutdown with handoff,
//! retry backoff, and garbage collection. Fake agent + fake backend only.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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
            assert!(out.status.success());
        }
        let (ok, out) = e.garnish(&["project", "add", "--name", "demo", "--path", e.repo.path().to_str().unwrap()], &[]);
        assert!(ok, "{out}");
        e
    }

    fn cmd(&self, args: &[&str], extra_env: &[(&str, &str)]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_garnish"));
        cmd.args(args)
            .env("GARNISH_DATA_DIR", self.data_dir.path())
            .env("GARNISH_FAKE_AGENT_BIN", fake_agent_bin());
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        cmd
    }

    fn garnish(&self, args: &[&str], extra_env: &[(&str, &str)]) -> (bool, String) {
        let out = self.cmd(args, extra_env).output().unwrap();
        (
            out.status.success(),
            format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
        )
    }

    fn add_task(&self) -> String {
        let (ok, out) = self.garnish(
            &[
                "task", "add", "--project", "demo", "--title", "t",
                "--goal", "write-file:hello.txt:hi",
                "--criterion", "exists",
                "--verify", "test -f hello.txt",
            ],
            &[],
        );
        assert!(ok, "{out}");
        out.split_whitespace().nth(1).unwrap().to_string()
    }

    fn status(&self, id: &str) -> String {
        let (ok, out) = self.garnish(&["task", "show", id, "--json"], &[]);
        assert!(ok, "{out}");
        serde_json::from_str::<serde_json::Value>(&out).unwrap()["task"]["status"]
            .as_str().unwrap().to_string()
    }

    fn wait_status(&self, id: &str, want: &str, secs: u64) {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let s = self.status(id);
            if s == want {
                return;
            }
            assert!(Instant::now() < deadline, "task {id} stuck in {s}, wanted {want}");
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    fn spawn_daemon(&self, extra_env: &[(&str, &str)]) -> Child {
        let mut env = vec![("GARNISH_POLL_MS", "200")];
        env.extend_from_slice(extra_env);
        self.cmd(&["daemon", "run"], &env)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }
}

fn fake_agent_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake-agent"))
}

fn sigterm(child: &Child) {
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
}

#[test]
fn daemon_picks_up_and_completes_ready_task() {
    let env = Env::new();
    let task = env.add_task();
    let mut d = env.spawn_daemon(&[]);
    env.wait_status(&task, "review", 30);
    sigterm(&d);
    assert!(d.wait().unwrap().success());
}

#[test]
fn crash_recovery_lease_expires_and_task_resumes() {
    let env = Env::new();
    let task = env.add_task();

    // Run with a sleeping agent and a 2s lease, then SIGKILL garnish mid-run
    // (simulated crash: no cleanup, no state transition).
    let mut child = env
        .cmd(&["task", "run", &task, "--backend", "fake"],
             &[("GARNISH_FAKE_MODE", "sleep"), ("GARNISH_LEASE_SECS", "2")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    env.wait_status(&task, "running", 20);
    child.kill().unwrap(); // SIGKILL — a real crash
    child.wait().unwrap();
    assert_eq!(env.status(&task), "running", "crash leaves stale running state");

    // Heartbeats stopped; after the lease expires any invocation recovers the
    // orphan and the same command re-runs it to completion.
    std::thread::sleep(Duration::from_secs(3));
    let (ok, out) = env.garnish(&["task", "run", &task, "--backend", "fake"], &[]);
    assert!(ok, "{out}");
    assert!(out.contains("VERIFIED"), "recovered task should run to review: {out}");
    assert_eq!(env.status(&task), "review");
}

#[test]
fn pause_all_blocks_leasing_until_resumed() {
    let env = Env::new();
    let task = env.add_task();
    let (ok, _) = env.garnish(&["daemon", "pause-all"], &[]);
    assert!(ok);
    let mut d = env.spawn_daemon(&[]);
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(env.status(&task), "ready", "pause-all must prevent leasing");
    let (ok, _) = env.garnish(&["daemon", "resume-all"], &[]);
    assert!(ok);
    env.wait_status(&task, "review", 30);
    sigterm(&d);
    d.wait().unwrap();
}

#[test]
fn daemon_shutdown_pauses_running_task_with_handoff_and_resume_works() {
    let env = Env::new();
    let task = env.add_task();
    let mut d = env.spawn_daemon(&[("GARNISH_FAKE_MODE", "sleep")]);
    env.wait_status(&task, "running", 30);

    sigterm(&d); // graceful shutdown: pause, don't cancel
    assert!(d.wait().unwrap().success());
    env.wait_status(&task, "paused", 15);

    let handoff = std::fs::read_to_string(env.repo.path().join(".harness-garnish/HANDOFF.md")).unwrap();
    assert!(handoff.contains("next_safe_action"), "handoff packet missing: {handoff}");

    // Resume from repository state + evidence; a fresh (well-behaved) agent
    // finishes the task in the preserved worktree.
    let (ok, out) = env.garnish(&["task", "resume", &task], &[]);
    assert!(ok, "{out}");
    let (ok, out) = env.garnish(&["task", "run", &task, "--backend", "fake"], &[]);
    assert!(ok, "{out}");
    assert_eq!(env.status(&task), "review", "{out}");
}

#[test]
fn failed_task_retries_with_backoff_then_exhausts() {
    let env = Env::new();
    let task = env.add_task();
    let mut d = env.spawn_daemon(&[("GARNISH_FAKE_MODE", "fail"), ("GARNISH_BACKOFF_BASE_SECS", "1")]);
    // Budget = 2 retries; always-failing agent must end failed.
    env.wait_status(&task, "failed", 60);
    sigterm(&d);
    d.wait().unwrap();

    let (ok, out) = env.garnish(&["task", "show", &task, "--json"], &[]);
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["task"]["retry_budget"], 0, "retry budget should be exhausted");
    let runs = v["runs"].as_array().unwrap();
    assert!(runs.len() >= 2, "expected retried runs, got {}", runs.len());
}

#[test]
fn gc_removes_terminal_worktrees_but_keeps_branches() {
    let env = Env::new();
    let task = env.add_task();
    let (ok, out) = env.garnish(&["task", "run", &task, "--backend", "fake"], &[]);
    assert!(ok && out.contains("VERIFIED"), "{out}");

    // review -> completed is a user act; emulate via cancel-side terminal
    // state instead: mark completed is not exposed yet, so GC a cancelled one.
    let task2 = env.add_task();
    let mut child = env
        .cmd(&["task", "run", &task2, "--backend", "fake"], &[("GARNISH_FAKE_MODE", "sleep")])
        .stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    env.wait_status(&task2, "running", 20);
    env.garnish(&["task", "cancel", &task2], &[]);
    child.wait().unwrap();
    env.wait_status(&task2, "cancelled", 10);

    let (_, show) = env.garnish(&["task", "show", &task2, "--json"], &[]);
    let v: serde_json::Value = serde_json::from_str(&show).unwrap();
    let wt = PathBuf::from(v["task"]["git"]["worktree_path"].as_str().unwrap());
    let branch = v["task"]["git"]["branch"].as_str().unwrap().to_string();
    assert!(wt.exists());

    let (ok, out) = env.garnish(&["gc"], &[]);
    assert!(ok, "{out}");
    assert!(!wt.exists(), "cancelled task's worktree should be removed");
    // Branch survives as evidence.
    let out = Command::new("git").args(["branch", "--list", &branch]).current_dir(env.repo.path()).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains(&branch));
    // The reviewed (non-terminal) task's worktree is untouched.
    let (_, show1) = env.garnish(&["task", "show", &task, "--json"], &[]);
    let v1: serde_json::Value = serde_json::from_str(&show1).unwrap();
    assert!(PathBuf::from(v1["task"]["git"]["worktree_path"].as_str().unwrap()).exists());
}
