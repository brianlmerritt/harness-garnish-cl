//! MVP criterion 8: pause a running task (handoff packet written), then
//! resume and complete it with a DIFFERENT adapter — fake CLI agent first,
//! then the api adapter with the fake model provider. Resume works from
//! repository state + the evidence bundle; no conversation state pretends
//! to be portable.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn paused_task_resumes_under_a_different_adapter() {
    let data_dir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["-c", "user.name=t", "-c", "user.email=t@t", "commit", "--allow-empty", "-m", "init"],
    ] {
        assert!(Command::new("git").args(&args).current_dir(repo.path()).output().unwrap().status.success());
    }
    let garnish = |args: &[&str], extra: &[(&str, &str)]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_garnish"));
        cmd.args(args)
            .env("GARNISH_DATA_DIR", data_dir.path())
            .env("GARNISH_FAKE_AGENT_BIN", env!("CARGO_BIN_EXE_fake-agent"))
            .env("GARNISH_API_AGENT_BIN", env!("CARGO_BIN_EXE_garnish-api-agent"))
            .env("GARNISH_API_PROVIDER", "fake")
            .env("GARNISH_API_MODEL", "fake-model");
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        (
            out.status.success(),
            format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
        )
    };
    let status = |id: &str| -> String {
        let (ok, out) = garnish(&["task", "show", id, "--json"], &[]);
        assert!(ok, "{out}");
        serde_json::from_str::<serde_json::Value>(&out).unwrap()["task"]["status"]
            .as_str().unwrap().to_string()
    };

    let (ok, out) = garnish(&["project", "add", "--name", "demo", "--path", repo.path().to_str().unwrap()], &[]);
    assert!(ok, "{out}");
    let (ok, out) = garnish(&[
        "task", "add", "--project", "demo", "--title", "handoff demo",
        "--goal", "write-file:hello.txt:completed after handoff",
        "--criterion", "hello.txt exists",
        "--verify", "grep -q handoff hello.txt",
    ], &[]);
    assert!(ok, "{out}");
    let task = out.split_whitespace().nth(1).unwrap().to_string();

    // Adapter #1 (fake CLI agent, sleeping) starts the task.
    let mut child = Command::new(env!("CARGO_BIN_EXE_garnish"))
        .args(["task", "run", &task, "--backend", "fake", "--adapter", "fake"])
        .env("GARNISH_DATA_DIR", data_dir.path())
        .env("GARNISH_FAKE_AGENT_BIN", env!("CARGO_BIN_EXE_fake-agent"))
        .env("GARNISH_FAKE_MODE", "sleep")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while status(&task) != "running" {
        assert!(Instant::now() < deadline, "task never started");
        std::thread::sleep(Duration::from_millis(250));
    }

    // Pause at the next safe point -> handoff packet.
    let (ok, out) = garnish(&["task", "pause", &task], &[]);
    assert!(ok, "{out}");
    child.wait().unwrap();
    assert_eq!(status(&task), "paused");
    let handoff = std::fs::read_to_string(repo.path().join(".harness-garnish/HANDOFF.md")).unwrap();
    assert!(handoff.contains("next_safe_action"), "handoff packet missing: {handoff}");
    assert!(handoff.contains("no hidden conversation state"), "{handoff}");

    // Adapter #2 (api + fake model provider) resumes in the SAME worktree
    // and completes; verification decides, not the agent.
    let (ok, out) = garnish(&["task", "resume", &task], &[]);
    assert!(ok, "{out}");
    let (ok, out) = garnish(&["task", "run", &task, "--backend", "fake", "--adapter", "api"], &[]);
    assert!(ok, "{out}");
    assert!(out.contains("VERIFIED"), "resumed task should verify: {out}");
    assert_eq!(status(&task), "review");

    // The route history shows the second adapter; both runs are evidenced.
    let (_, out) = garnish(&["task", "show", &task, "--json"], &[]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["task"]["route"]["adapter"], "api");
    let adapters: Vec<&str> = v["runs"].as_array().unwrap().iter()
        .filter_map(|r| r["adapter"].as_str()).collect();
    assert!(adapters.contains(&"fake") && adapters.contains(&"api"), "runs: {adapters:?}");
}
