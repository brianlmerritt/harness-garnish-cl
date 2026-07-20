use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

/// Run git with argv array (never a shell) in `cwd`; error on non-zero exit.
pub fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("running git {args:?}"))?;
    if !out.status.success() {
        anyhow::bail!(
            "git {:?} failed ({}): {}",
            args,
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn head_commit(repo: &Path) -> Result<String> {
    git(repo, &["rev-parse", "HEAD"])
}

/// Record the state of the user's checkout before any task work
/// (protect-dirty-tree requirement). Returned as evidence JSON.
pub fn snapshot_status(repo: &Path) -> Result<serde_json::Value> {
    Ok(serde_json::json!({
        "head": head_commit(repo)?,
        "branch": git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?,
        "status": git(repo, &["status", "--porcelain"])?,
        "remotes": git(repo, &["remote", "-v"]).unwrap_or_default(),
    }))
}
