//! Project memory: curated facts reach agents inside the worktree, the
//! context never leaks into task commits, and agent proposals surface as
//! evidence without auto-promotion.

use std::path::PathBuf;
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

    fn task_json(&self, id: &str) -> serde_json::Value {
        let (ok, out) = self.garnish(&["task", "show", id, "--json"]);
        assert!(ok, "{out}");
        serde_json::from_str(&out).unwrap()
    }
}

#[test]
fn memory_reaches_agent_and_never_leaks_into_patch() {
    let env = Env::new();

    // Curate two facts; MEMORY.md projection regenerates.
    let (ok, out) = env.garnish(&["memory", "add", "--project", "demo", "Tests run with `make check`, not cargo test"]);
    assert!(ok, "{out}");
    let (ok, _) = env.garnish(&["memory", "add", "--project", "demo", "Never touch the vendored/ directory"]);
    assert!(ok);
    let memory_md = std::fs::read_to_string(env.repo.path().join(".harness-garnish/MEMORY.md")).unwrap();
    assert!(memory_md.contains("make check") && memory_md.contains("vendored/"), "{memory_md}");
    assert!(memory_md.contains("(user, "), "provenance missing: {memory_md}");

    // Run a task; the worktree must contain the materialised context and the
    // agent must receive the preamble pointing at it.
    let (ok, out) = env.garnish(&[
        "task", "add", "--project", "demo", "--title", "t",
        "--goal", "write-file:hello.txt:hi",
        "--criterion", "exists", "--verify", "test -f hello.txt",
    ]);
    assert!(ok, "{out}");
    let task = out.split_whitespace().nth(1).unwrap().to_string();
    let (ok, out) = env.garnish(&["task", "run", &task, "--backend", "fake"]);
    assert!(ok && out.contains("VERIFIED"), "{out}");

    let v = env.task_json(&task);
    let wt = PathBuf::from(v["task"]["git"]["worktree_path"].as_str().unwrap());
    let wt_memory = std::fs::read_to_string(wt.join(".harness-garnish/MEMORY.md")).unwrap();
    assert!(wt_memory.contains("make check"), "memory not materialised in worktree");
    assert!(wt.join(".harness-garnish/PROJECT.md").exists());

    let evidence = PathBuf::from(v["runs"][0]["evidence_dir"].as_str().unwrap());
    let events = std::fs::read_to_string(evidence.join("events.jsonl")).unwrap();
    assert!(events.contains("MEMORY.md"), "agent prompt lacked the context preamble: {events}");

    // The context must never appear in the produced patch.
    let patch = std::fs::read_to_string(evidence.join("patch.diff")).unwrap();
    assert!(!patch.contains(".harness-garnish"), "context leaked into patch:\n{patch}");
    assert!(patch.contains("hello.txt"));

    // Removal regenerates the projection.
    let (_, list) = env.garnish(&["memory", "list", "--project", "demo"]);
    let id = list.lines().next().unwrap().split_whitespace().next().unwrap().to_string();
    let (ok, _) = env.garnish(&["memory", "remove", &id]);
    assert!(ok);
    let memory_md = std::fs::read_to_string(env.repo.path().join(".harness-garnish/MEMORY.md")).unwrap();
    assert!(!memory_md.contains("make check") && memory_md.contains("vendored/"));
}

#[test]
fn agent_memory_proposals_surface_as_evidence_not_canon() {
    let env = Env::new();
    // The fake agent "discovers" something and writes a proposal file.
    let (ok, out) = env.garnish(&[
        "task", "add", "--project", "demo", "--title", "proposer",
        "--goal", "write-file:.harness-garnish/memory-proposals.md:CI needs docker 27+",
        "--criterion", "proposal written", "--verify", "true",
    ]);
    assert!(ok, "{out}");
    let task = out.split_whitespace().nth(1).unwrap().to_string();
    let (ok, out) = env.garnish(&["task", "run", &task, "--backend", "fake"]);
    assert!(ok, "{out}");
    assert!(out.contains("agent proposed memory"), "{out}");

    let v = env.task_json(&task);
    let evidence = PathBuf::from(v["runs"][0]["evidence_dir"].as_str().unwrap());
    let proposal = std::fs::read_to_string(evidence.join("memory-proposals.md")).unwrap();
    assert!(proposal.contains("docker 27+"));

    // Not canon: the project MEMORY.md is untouched, and the patch is clean.
    let memory_md = std::fs::read_to_string(env.repo.path().join(".harness-garnish/MEMORY.md")).unwrap();
    assert!(!memory_md.contains("docker 27+"), "proposal must not auto-promote");
    let patch = std::fs::read_to_string(evidence.join("patch.diff")).unwrap();
    assert!(!patch.contains("memory-proposals"), "proposal leaked into patch:\n{patch}");

    let (ok, _) = env.garnish(&["memory", "add", "--project", "demo", "CI needs docker 27+"]);
    assert!(ok);
    let memory_md = std::fs::read_to_string(env.repo.path().join(".harness-garnish/MEMORY.md")).unwrap();
    assert!(memory_md.contains("docker 27+"), "promotion via garnish memory add");
}
