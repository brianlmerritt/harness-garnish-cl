//! Real container-backend integration tests, env-gated so CI and quick local
//! runs skip them. Runs the full happy path against each named engine, plus a
//! network-isolation check. No agent quota is consumed (fake agent only).
//!
//! Enable with a comma-separated engine list:
//!
//!     GARNISH_TEST_BACKENDS=docker,podman cargo test -p garnish-cli --test backends -- --nocapture
//!
//! An engine named but not installed FAILS (you asked for it); unset var
//! skips everything.

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
            assert!(Command::new("git").args(&args).current_dir(e.repo.path()).output().unwrap().status.success());
        }
        let (ok, out) = e.garnish(&["project", "add", "--name", "demo", "--path", e.repo.path().to_str().unwrap()]);
        assert!(ok, "{out}");
        e
    }

    fn garnish(&self, args: &[&str]) -> (bool, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_garnish"))
            .args(args)
            .env("GARNISH_DATA_DIR", self.data_dir.path())
            .env("GARNISH_FAKE_AGENT_BIN", env!("CARGO_BIN_EXE_fake-agent"))
            .output()
            .unwrap();
        (
            out.status.success(),
            format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
        )
    }
}

fn enabled_backends() -> Vec<String> {
    std::env::var("GARNISH_TEST_BACKENDS")
        .map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default()
}

/// Full loop against the real engine: fake agent writes a file in the
/// worktree; verification runs `test -f` inside a constrained container.
#[test]
fn engine_happy_path() {
    let backends = enabled_backends();
    if backends.is_empty() {
        eprintln!("skipped: set GARNISH_TEST_BACKENDS=docker,podman to run");
        return;
    }
    for backend in &backends {
        let env = Env::new();
        let (ok, out) = env.garnish(&[
            "task", "add", "--project", "demo", "--title", &format!("{backend} happy"),
            "--goal", "write-file:hello.txt:via real engine",
            "--criterion", "hello.txt exists",
            "--verify", "test -f hello.txt",
        ]);
        assert!(ok, "[{backend}] {out}");
        let task = out.split_whitespace().nth(1).unwrap().to_string();
        let (ok, out) = env.garnish(&["task", "run", &task, "--backend", backend]);
        assert!(ok, "[{backend}] {out}");
        assert!(out.contains("VERIFIED"), "[{backend}] expected verification pass:\n{out}");
        println!("[{backend}] happy path OK");
    }
}

/// The verification container runs with --network=none: a task whose
/// verification needs the network must FAIL verification.
#[test]
fn engine_network_is_off() {
    let backends = enabled_backends();
    if backends.is_empty() {
        eprintln!("skipped: set GARNISH_TEST_BACKENDS=docker,podman to run");
        return;
    }
    for backend in &backends {
        let env = Env::new();
        let (ok, out) = env.garnish(&[
            "task", "add", "--project", "demo", "--title", &format!("{backend} netcheck"),
            "--goal", "write-file:hello.txt:hi",
            "--criterion", "network must be unreachable",
            "--verify", "wget -T 4 -q -O- http://example.com",
        ]);
        assert!(ok, "[{backend}] {out}");
        let task = out.split_whitespace().nth(1).unwrap().to_string();
        let (ok, out) = env.garnish(&["task", "run", &task, "--backend", backend]);
        assert!(ok, "[{backend}] {out}");
        assert!(
            out.contains("verification FAILED"),
            "[{backend}] network reachable inside sandbox — isolation broken:\n{out}"
        );
        println!("[{backend}] network-off confirmed");
    }
}
