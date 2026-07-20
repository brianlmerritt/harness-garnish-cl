//! Web UX integration tests: token auth, overview reads, and the write
//! actions (approval decide, task pause) going through the same policy path
//! as the CLI.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

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

    fn add_task(&self, extra: &[&str]) -> String {
        let mut args = vec![
            "task", "add", "--project", "demo", "--title", "web test task",
            "--goal", "write-file:hello.txt:hi",
            "--criterion", "exists",
            "--verify", "test -f hello.txt",
        ];
        args.extend_from_slice(extra);
        let (ok, out) = self.garnish(&args);
        assert!(ok, "{out}");
        out.split_whitespace().nth(1).unwrap().to_string()
    }

    /// Start `garnish web --port 0`, parse "http://127.0.0.1:PORT/?token=T".
    fn start_web(&self) -> (Child, String, String) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_garnish"))
            .args(["web", "--port", "0"])
            .env("GARNISH_DATA_DIR", self.data_dir.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        let url = line.split_whitespace().last().unwrap().to_string();
        let (base, token) = url.split_once("/?token=").unwrap();
        (child, base.to_string(), token.trim().to_string())
    }
}

fn get_json(url: &str, token: &str) -> serde_json::Value {
    ureq::get(url)
        .set("authorization", &format!("Bearer {token}"))
        .timeout(Duration::from_secs(10))
        .call()
        .unwrap()
        .into_json()
        .unwrap()
}

fn post_json(url: &str, token: &str) -> serde_json::Value {
    ureq::post(url)
        .set("authorization", &format!("Bearer {token}"))
        .timeout(Duration::from_secs(10))
        .call()
        .unwrap()
        .into_json()
        .unwrap()
}

#[test]
fn web_auth_reads_and_actions() {
    let env = Env::new();
    let task = env.add_task(&["--risk", "2"]);
    // Create the pending approval by attempting a run.
    let (ok, out) = env.garnish(&["task", "run", &task, "--backend", "fake"]);
    assert!(ok && out.contains("requires approval"), "{out}");

    let (mut server, base, token) = env.start_web();
    let result = std::panic::catch_unwind(|| {
        // 1. No token -> 401. Wrong token -> 401.
        for bad in [None, Some("wrong")] {
            let mut req = ureq::get(&format!("{base}/api/overview")).timeout(Duration::from_secs(10));
            if let Some(t) = bad {
                req = req.set("authorization", &format!("Bearer {t}"));
            }
            match req.call() {
                Err(ureq::Error::Status(401, _)) => {}
                other => panic!("expected 401, got {other:?}"),
            }
        }
        // The HTML shell itself is served without auth (it holds no data).
        assert!(ureq::get(&format!("{base}/")).call().unwrap().into_string().unwrap().contains("Harness Garnish"));

        // 2. Overview shows the project, task, and pending approval.
        let o = get_json(&format!("{base}/api/overview"), &token);
        assert_eq!(o["projects"][0]["name"], "demo");
        assert_eq!(o["tasks"][0]["status"], "awaiting_approval");
        let approval_id = o["approvals"][0]["id"].as_str().unwrap().to_string();

        // 3. Approve via the web path; recorded as decided_via=web.
        let r = post_json(&format!("{base}/api/approval/{approval_id}/approve"), &token);
        assert_eq!(r["status"], "approved");

        // 4. Task detail endpoint.
        let d = get_json(&format!("{base}/api/task/{}", o["tasks"][0]["id"].as_str().unwrap()), &token);
        assert_eq!(d["task"]["title"], "web test task");
        assert!(d["events"].as_array().unwrap().iter().any(|e| e["kind"] == "approval_decided"));
    });
    let _ = server.kill();
    let _ = server.wait();
    result.unwrap();

    // 5. The web approval is honoured by the normal runner path.
    let (ok, out) = env.garnish(&["task", "run", &task, "--backend", "fake"]);
    assert!(ok && out.contains("VERIFIED"), "web-approved task should run: {out}");
}

#[test]
fn web_pause_and_resume_ready_task() {
    let env = Env::new();
    let task = env.add_task(&[]);
    let (mut server, base, token) = env.start_web();
    let result = std::panic::catch_unwind(|| {
        let r = post_json(&format!("{base}/api/task/{task}/pause"), &token);
        assert_eq!(r["result"], "paused");
        let o = get_json(&format!("{base}/api/overview"), &token);
        assert_eq!(o["tasks"][0]["status"], "paused");
        let r = post_json(&format!("{base}/api/task/{task}/resume"), &token);
        assert_eq!(r["result"], "ready");
    });
    let _ = server.kill();
    let _ = server.wait();
    result.unwrap();
}
