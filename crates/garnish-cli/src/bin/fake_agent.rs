//! Deterministic fake agent for tests and CI. Consumes no provider quota.
//!
//! argv[1] = goal. If the goal has the form `write-file:<name>:<content>`,
//! success mode writes that file into the cwd (the task worktree).
//!
//! GARNISH_FAKE_MODE:
//!   ok    - do the work, emit events, exit 0
//!   fail  - emit an error event, exit 1
//!   sleep - hang for 300s (timeout/cancel tests)
//!   lie   - do nothing, claim success, exit 0 (verifier must catch this)

fn emit(v: serde_json::Value) {
    println!("{v}");
}

fn main() {
    let goal = std::env::args().nth(1).unwrap_or_default();
    let mode = std::env::var("GARNISH_FAKE_MODE").unwrap_or_else(|_| "ok".into());
    emit(serde_json::json!({ "type": "start", "goal": goal, "mode": mode }));

    match mode.as_str() {
        "sleep" => {
            std::thread::sleep(std::time::Duration::from_secs(300));
        }
        "fail" => {
            emit(serde_json::json!({ "type": "error", "message": "deterministic failure" }));
            std::process::exit(1);
        }
        "lie" => {
            emit(serde_json::json!({ "type": "result", "status": "success", "note": "(lying)" }));
        }
        _ => {
            // The directive may sit anywhere in the goal (the runner prefixes
            // a context preamble); take everything after the marker.
            if let Some(idx) = goal.find("write-file:") {
                let rest = &goal[idx + "write-file:".len()..];
                if let Some((name, content)) = rest.split_once(':') {
                    // Refuse path traversal exactly like a real tool boundary should.
                    if name.contains("..") || name.starts_with('/') {
                        emit(serde_json::json!({ "type": "error", "message": "path escapes worktree" }));
                        std::process::exit(1);
                    }
                    std::fs::write(name, format!("{content}\n")).expect("write");
                    emit(serde_json::json!({ "type": "action", "tool": "write_file", "path": name }));
                }
            }
            emit(serde_json::json!({ "type": "result", "status": "success" }));
        }
    }
}
