use crate::git::git;
use anyhow::Result;
use std::path::{Path, PathBuf};

pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
    pub base_commit: String,
}

/// One worktree/branch per write task, created under the global data dir —
/// never inside the user's checkout.
pub fn create(repo: &Path, project_id: &str, task_id: &str, branch_prefix: &str) -> Result<Worktree> {
    let base_commit = crate::git::head_commit(repo)?;
    let branch = format!("{branch_prefix}{task_id}");
    let dir = garnish_core::paths::worktrees_dir(project_id);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(task_id);
    if path.exists() {
        anyhow::bail!("worktree path already exists: {}", path.display());
    }
    git(
        repo,
        &["worktree", "add", "-b", &branch, path.to_str().unwrap(), &base_commit],
    )?;
    Ok(Worktree { path, branch, base_commit })
}

/// Remove a task worktree; keeps the branch (the branch is the evidence).
pub fn remove(repo: &Path, worktree_path: &Path) -> Result<()> {
    git(
        repo,
        &["worktree", "remove", "--force", worktree_path.to_str().unwrap()],
    )?;
    Ok(())
}

/// Commit everything in the worktree as the task result. Returns the head
/// commit (base commit if nothing changed).
pub fn commit_all(worktree: &Path, message: &str) -> Result<String> {
    git(worktree, &["add", "-A"])?;
    let staged = git(worktree, &["status", "--porcelain"])?;
    if !staged.is_empty() {
        git(
            worktree,
            &[
                "-c", "user.name=garnish",
                "-c", "user.email=garnish@localhost",
                "commit", "-m", message,
            ],
        )?;
    }
    crate::git::head_commit(worktree)
}

/// Produce the patch (unified diff) between base and head.
pub fn diff(worktree: &Path, base: &str, head: &str) -> Result<String> {
    git(worktree, &["diff", &format!("{base}..{head}")])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn init_repo(dir: &Path) {
        for args in [
            vec!["init", "-b", "main"],
            vec!["-c", "user.name=t", "-c", "user.email=t@t", "commit", "--allow-empty", "-m", "init"],
        ] {
            let st = Command::new("git").args(&args).current_dir(dir).output().unwrap();
            assert!(st.status.success(), "{args:?}: {}", String::from_utf8_lossy(&st.stderr));
        }
    }

    #[test]
    fn worktree_lifecycle() {
        let data = tempfile::tempdir().unwrap();
        std::env::set_var("GARNISH_DATA_DIR", data.path());
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());

        let wt = create(repo.path(), "proj1", "task1", "garnish/").unwrap();
        assert!(wt.path.exists());
        assert!(!wt.path.starts_with(repo.path()), "worktree must not live inside the checkout");

        std::fs::write(wt.path.join("hello.txt"), "hi\n").unwrap();
        let head = commit_all(&wt.path, "task result").unwrap();
        assert_ne!(head, wt.base_commit);
        let d = diff(&wt.path, &wt.base_commit, &head).unwrap();
        assert!(d.contains("hello.txt"));

        remove(repo.path(), &wt.path).unwrap();
        assert!(!wt.path.exists());
    }
}
