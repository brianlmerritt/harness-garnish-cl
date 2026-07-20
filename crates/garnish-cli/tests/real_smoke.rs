//! OPT-IN smoke tests against REAL agent CLIs. These CONSUME SUBSCRIPTION
//! QUOTA and are ignored by default; CI must never run them.
//!
//!     cargo test -p garnish-cli --test real_smoke -- --ignored
//!
//! Each drives the full loop: route (with real codexbar quota gate if
//! installed) -> worktree -> real agent -> docker-verified -> patch.

use std::path::Path;
use std::process::Command;

fn run_real(adapter: &str) {
    let data_dir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["-c", "user.name=t", "-c", "user.email=t@t", "commit", "--allow-empty", "-m", "init"],
    ] {
        assert!(Command::new("git").args(&args).current_dir(repo.path()).output().unwrap().status.success());
    }
    let garnish = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_garnish"))
            .args(args)
            .env("GARNISH_DATA_DIR", data_dir.path())
            .output()
            .unwrap();
        (
            out.status.success(),
            format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
        )
    };
    let policy = r#"{"quota": {"unknown_quota": "fail_open"}}"#;
    let (ok, out) = garnish(&[
        "project", "add", "--name", "smoke", "--path", repo.path().to_str().unwrap(),
        "--policy-json", policy,
    ]);
    assert!(ok, "{out}");
    let (ok, out) = garnish(&[
        "task", "add", "--project", "smoke", "--title", "real smoke",
        "--goal", "Create a file named hello.txt containing exactly the single line: hello from garnish",
        "--criterion", "hello.txt exists with expected content",
        "--verify", "grep -q garnish hello.txt",
    ]);
    assert!(ok, "{out}");
    let task = out.split_whitespace().nth(1).unwrap().to_string();
    let (ok, out) = garnish(&["task", "run", &task, "--adapter", adapter, "--backend", "docker", "--timeout-min", "10"]);
    assert!(ok, "{out}");
    assert!(out.contains("VERIFIED"), "real {adapter} run did not verify: {out}");
    assert!(!repo.path().join("hello.txt").exists(), "leaked into user checkout");
    let _ = Path::new("");
}

#[test]
#[ignore = "consumes Claude subscription quota"]
fn real_claude_end_to_end() {
    run_real("claude-code");
}

#[test]
#[ignore = "consumes Codex subscription quota"]
fn real_codex_end_to_end() {
    run_real("codex");
}

#[test]
#[ignore = "consumes Antigravity subscription quota"]
fn real_antigravity_end_to_end() {
    run_real("antigravity");
}
