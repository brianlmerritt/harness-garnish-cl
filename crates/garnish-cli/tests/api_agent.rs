//! API-agent slice e2e: the `api` adapter drives garnish-api-agent with the
//! deterministic fake model provider (no network, no keys, no cost), through
//! the normal loop: route -> worktree -> tool loop writes the file -> commit
//! -> verification -> patch, with usage recorded in the cost ledger.

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
            .env("GARNISH_API_AGENT_BIN", env!("CARGO_BIN_EXE_garnish-api-agent"))
            .env("GARNISH_API_PROVIDER", "fake")
            .env("GARNISH_API_MODEL", "fake-model")
            .output()
            .unwrap();
        (
            out.status.success(),
            format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
        )
    }
}

#[test]
fn api_agent_end_to_end_with_cost_ledger() {
    let env = Env::new();
    let (ok, out) = env.garnish(&[
        "task", "add", "--project", "demo", "--title", "api hello",
        "--goal", "write-file:hello.txt:from the api agent",
        "--criterion", "hello.txt exists",
        "--verify", "grep -q api hello.txt",
    ]);
    assert!(ok, "{out}");
    let task = out.split_whitespace().nth(1).unwrap().to_string();

    let (ok, out) = env.garnish(&["task", "run", &task, "--adapter", "api", "--backend", "fake"]);
    assert!(ok, "{out}");
    assert!(out.contains("VERIFIED"), "api agent should complete the task: {out}");

    // Usage flowed into the cost ledger, priced from the bundled table
    // (fake-model is priced at $1/M in, $2/M out).
    let (ok, out) = env.garnish(&["cost", "--json"]);
    assert!(ok, "{out}");
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    let row = &rows[0];
    assert_eq!(row["provider"], "fake");
    assert_eq!(row["model"], "fake-model");
    assert!(row["input_tokens"].as_i64().unwrap() > 0);
    assert!(row["usd"].as_f64().unwrap() > 0.0, "fake-model is priced; usd must not be NULL");

    // Human view mentions a total.
    let (ok, out) = env.garnish(&["cost"]);
    assert!(ok && out.contains("total priced"), "{out}");
}

/// The API agent's tools must refuse to escape the worktree.
#[test]
fn api_agent_rejects_path_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_garnish-api-agent"))
        .arg("write-file:../escape.txt:nope")
        .current_dir(dir.path())
        .env("GARNISH_API_PROVIDER", "fake")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("escapes the working directory"), "{stdout}");
    assert!(!dir.path().parent().unwrap().join("escape.txt").exists());
}
