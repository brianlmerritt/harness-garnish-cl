//! Phase 3 quota-routing tests: below-reserve decline + reschedule,
//! unknown-quota fail-closed/fail-open, and route scoring evidence.
//! The fake adapter is forced through the quota gate via
//! GARNISH_FAKE_ADAPTER_QUOTA_PROVIDER; quota states come from the fake
//! provider (GARNISH_QUOTA=fake + GARNISH_FAKE_QUOTA). No real quota is read.

use std::path::PathBuf;
use std::process::Command;

struct Env {
    data_dir: tempfile::TempDir,
    repo: tempfile::TempDir,
}

impl Env {
    fn new(policy_json: Option<&str>) -> Self {
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
        let mut args = vec![
            "project".to_string(), "add".into(), "--name".into(), "demo".into(),
            "--path".into(), e.repo.path().to_str().unwrap().to_string(),
        ];
        if let Some(p) = policy_json {
            args.push("--policy-json".into());
            args.push(p.into());
        }
        let argrefs: Vec<&str> = args.iter().map(String::as_str).collect();
        let (ok, out) = e.garnish(&argrefs, &[]);
        assert!(ok, "{out}");
        e
    }

    fn garnish(&self, args: &[&str], extra_env: &[(&str, &str)]) -> (bool, String) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_garnish"));
        cmd.args(args)
            .env("GARNISH_DATA_DIR", self.data_dir.path())
            .env("GARNISH_FAKE_AGENT_BIN", fake_agent_bin())
            .env("GARNISH_QUOTA", "fake")
            .env("GARNISH_FAKE_ADAPTER_QUOTA_PROVIDER", "fakeprov");
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
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

    fn task_json(&self, id: &str) -> serde_json::Value {
        let (ok, out) = self.garnish(&["task", "show", id, "--json"], &[]);
        assert!(ok, "{out}");
        serde_json::from_str(&out).unwrap()
    }
}

fn fake_agent_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake-agent"))
}

/// Plenty of headroom in both windows -> routed, run, scored.
#[test]
fn quota_ok_routes_and_records_score() {
    let env = Env::new(None);
    let task = env.add_task();
    let quota = r#"{"fakeprov": {"session": {"remaining": 80}, "weekly": {"remaining": 70}}}"#;
    let (ok, out) = env.garnish(&["task", "run", &task, "--backend", "fake"], &[("GARNISH_FAKE_QUOTA", quota)]);
    assert!(ok && out.contains("VERIFIED"), "{out}");
    let v = env.task_json(&task);
    assert_eq!(v["task"]["status"], "review");
    let route = &v["task"]["route"];
    assert_eq!(route["quota"]["state"], "ok");
    assert_eq!(route["quota"]["min_remaining_pct"], 70.0);
    assert!(route["score"].as_f64().unwrap() > 0.5, "route: {route}");
}

/// Below the weekly reserve -> declined and rescheduled for the reset time.
#[test]
fn quota_below_reserve_declines_and_reschedules() {
    let env = Env::new(None);
    let task = env.add_task();
    let quota = r#"{"fakeprov": {"session": {"remaining": 80}, "weekly": {"remaining": 5, "resets_in_secs": 1800}}}"#;
    let (ok, out) = env.garnish(&["task", "run", &task, "--backend", "fake"], &[("GARNISH_FAKE_QUOTA", quota)]);
    assert!(ok, "{out}");
    assert!(out.contains("declined") && out.contains("rescheduled"), "{out}");
    let v = env.task_json(&task);
    assert_eq!(v["task"]["status"], "ready", "declined task must stay ready");
    // A daemon would not pick it up before the reset (not_before is set):
    // eligible-now listing must exclude it. Re-running with good quota still
    // works because manual `task run` targets the task directly.
    let quota_ok = r#"{"fakeprov": {"session": {"remaining": 80}, "weekly": {"remaining": 60}}}"#;
    let (ok, out) = env.garnish(&["task", "run", &task, "--backend", "fake"], &[("GARNISH_FAKE_QUOTA", quota_ok)]);
    assert!(ok && out.contains("VERIFIED"), "{out}");
}

/// Unknown quota + default fail_closed -> declined, no reschedule time.
#[test]
fn quota_unknown_fail_closed_declines() {
    let env = Env::new(None);
    let task = env.add_task();
    let (ok, out) = env.garnish(
        &["task", "run", &task, "--backend", "fake"],
        &[("GARNISH_FAKE_QUOTA", r#"{"fakeprov": "unknown"}"#)],
    );
    assert!(ok, "{out}");
    assert!(out.contains("declined"), "{out}");
    assert_eq!(env.task_json(&task)["task"]["status"], "ready");
}

/// Stale snapshot is unknown, not a number -> fail_closed declines.
#[test]
fn quota_stale_treated_as_unknown() {
    let env = Env::new(None);
    let task = env.add_task();
    let quota = r#"{"fakeprov": {"session": {"remaining": 90, "stale": true}, "weekly": {"remaining": 90}}}"#;
    let (ok, out) = env.garnish(&["task", "run", &task, "--backend", "fake"], &[("GARNISH_FAKE_QUOTA", quota)]);
    assert!(ok, "{out}");
    assert!(out.contains("declined"), "stale must not count as remaining quota: {out}");
}

/// Unknown quota + fail_open project policy -> runs, with the gap recorded.
#[test]
fn quota_unknown_fail_open_proceeds_with_warning() {
    let policy = r#"{"quota": {"unknown_quota": "fail_open"}}"#;
    let env = Env::new(Some(policy));
    let task = env.add_task();
    let (ok, out) = env.garnish(
        &["task", "run", &task, "--backend", "fake"],
        &[("GARNISH_FAKE_QUOTA", r#"{"fakeprov": "unknown"}"#)],
    );
    assert!(ok && out.contains("VERIFIED"), "{out}");
    let v = env.task_json(&task);
    assert_eq!(v["task"]["route"]["quota"]["state"], "unknown");
    assert_eq!(v["task"]["route"]["quota"]["policy"], "fail_open");
}
