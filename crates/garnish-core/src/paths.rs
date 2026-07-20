use std::path::PathBuf;

/// Global data directory (ADR-0006). Overridable via GARNISH_DATA_DIR
/// (used by tests to isolate state).
pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("GARNISH_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").expect("HOME not set");
    if cfg!(target_os = "macos") {
        PathBuf::from(home).join("Library/Application Support/harness-garnish")
    } else {
        match std::env::var("XDG_DATA_HOME") {
            Ok(x) if !x.is_empty() => PathBuf::from(x).join("harness-garnish"),
            _ => PathBuf::from(home).join(".local/share/harness-garnish"),
        }
    }
}

pub fn db_path() -> PathBuf {
    data_dir().join("state.db")
}

/// Worktrees live under the data dir, never inside the user's checkout.
pub fn worktrees_dir(project_id: &str) -> PathBuf {
    data_dir().join("worktrees").join(project_id)
}

/// Per-project projection dir inside the project checkout.
pub fn projection_dir(project_root: &std::path::Path) -> PathBuf {
    project_root.join(".harness-garnish")
}
