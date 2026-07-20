use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;

/// Baseline environment allowlist for child processes. Everything else is
/// stripped; adapters add named extras explicitly.
const ENV_ALLOWLIST: &[&str] = &["PATH", "HOME", "USER", "SHELL", "TERM", "LANG", "LC_ALL", "TMPDIR"];

pub struct Supervision {
    pub timeout: Duration,
    /// Per-stream capture limit; output beyond this is dropped and flagged.
    pub output_limit_bytes: usize,
    /// Cooperative cancellation, checked continuously.
    pub cancel: Arc<AtomicBool>,
    /// SIGTERM -> SIGKILL grace.
    pub kill_grace: Duration,
}

impl Default for Supervision {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(15 * 60),
            output_limit_bytes: 8 * 1024 * 1024,
            cancel: Arc::new(AtomicBool::new(false)),
            kill_grace: Duration::from_secs(5),
        }
    }
}

#[derive(Debug)]
pub struct SpawnOutcome {
    /// ok | failed | timeout | cancelled
    pub status: &'static str,
    pub exit_code: Option<i32>,
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub truncated: bool,
    pub wall_ms: u128,
}

impl SpawnOutcome {
    pub fn ok(&self) -> bool {
        self.status == "ok"
    }
}

/// Spawn `argv` (argv array — never a shell) in `cwd` with a stripped
/// environment, capturing bounded stdout/stderr to files in `evidence_dir`,
/// enforcing timeout and cancellation, and cleaning up the whole process
/// group on the way out.
pub async fn run_supervised(
    argv: &[String],
    cwd: &Path,
    extra_env: &[(String, String)],
    evidence_dir: &Path,
    label: &str,
    sup: &Supervision,
) -> Result<SpawnOutcome> {
    anyhow::ensure!(!argv.is_empty(), "empty argv");
    std::fs::create_dir_all(evidence_dir)?;
    let stdout_path = evidence_dir.join(format!("{label}.stdout.log"));
    let stderr_path = evidence_dir.join(format!("{label}.stderr.log"));

    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(cwd)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for key in ENV_ALLOWLIST {
        if let Ok(v) = std::env::var(key) {
            cmd.env(key, v);
        }
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    #[cfg(unix)]
    cmd.process_group(0); // own group so we can kill descendants

    let start = std::time::Instant::now();
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning {:?}", argv[0]))?;
    let pid = child.id().map(|p| p as i32);

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let limit = sup.output_limit_bytes;
    let out_task = tokio::spawn(capture(stdout, stdout_path.clone(), limit));
    let err_task = tokio::spawn(capture(stderr, stderr_path.clone(), limit));

    let deadline = tokio::time::Instant::now() + sup.timeout;
    let mut status: &'static str;
    let mut exit_code: Option<i32> = None;

    loop {
        tokio::select! {
            res = child.wait() => {
                let st = res?;
                exit_code = st.code();
                status = if st.success() { "ok" } else { "failed" };
                break;
            }
            _ = tokio::time::sleep_until(deadline) => {
                status = "timeout";
                kill_group(pid, &mut child, sup.kill_grace).await;
                break;
            }
            _ = tokio::time::sleep(Duration::from_millis(250)) => {
                if sup.cancel.load(Ordering::Relaxed) {
                    status = "cancelled";
                    kill_group(pid, &mut child, sup.kill_grace).await;
                    break;
                }
            }
        }
    }

    let (out_bytes, out_trunc) = out_task.await??;
    let (err_bytes, err_trunc) = err_task.await??;
    // A cancelled/timed-out child may still report an exit; keep our status.
    if status == "ok" && exit_code.is_none() {
        status = "failed"; // killed by signal
    }
    Ok(SpawnOutcome {
        status,
        exit_code,
        stdout_tail: tail_of(&out_bytes),
        stderr_tail: tail_of(&err_bytes),
        stdout_path,
        stderr_path,
        truncated: out_trunc || err_trunc,
        wall_ms: start.elapsed().as_millis(),
    })
}

/// Copy a stream to a file up to `limit` bytes; returns (captured, truncated).
async fn capture(
    mut stream: impl tokio::io::AsyncRead + Unpin,
    path: PathBuf,
    limit: usize,
) -> Result<(Vec<u8>, bool)> {
    let mut file = tokio::fs::File::create(&path).await?;
    let mut captured = Vec::new();
    let mut truncated = false;
    let mut buf = [0u8; 8192];
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        if captured.len() < limit {
            let take = n.min(limit - captured.len());
            tokio::io::AsyncWriteExt::write_all(&mut file, &buf[..take]).await?;
            captured.extend_from_slice(&buf[..take]);
            if take < n {
                truncated = true;
            }
        } else {
            truncated = true; // keep draining so the child is not blocked
        }
    }
    if truncated {
        tokio::io::AsyncWriteExt::write_all(&mut file, b"\n[garnish: output truncated]\n").await?;
    }
    Ok((captured, truncated))
}

fn tail_of(bytes: &[u8]) -> String {
    const TAIL: usize = 4096;
    let start = bytes.len().saturating_sub(TAIL);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

#[cfg(unix)]
async fn kill_group(pid: Option<i32>, child: &mut tokio::process::Child, grace: Duration) {
    if let Some(pid) = pid {
        unsafe { libc::kill(-pid, libc::SIGTERM) }; // negative pid = process group
        if tokio::time::timeout(grace, child.wait()).await.is_ok() {
            return;
        }
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
    let _ = child.kill().await;
}

#[cfg(not(unix))]
async fn kill_group(_pid: Option<i32>, child: &mut tokio::process::Child, _grace: Duration) {
    let _ = child.kill().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[tokio::test]
    async fn captures_exit_and_output() {
        let dir = tempfile::tempdir().unwrap();
        let out = run_supervised(
            &argv(&["/bin/echo", "hello"]),
            dir.path(),
            &[],
            dir.path(),
            "t",
            &Supervision::default(),
        )
        .await
        .unwrap();
        assert!(out.ok());
        assert_eq!(out.exit_code, Some(0));
        assert!(out.stdout_tail.contains("hello"));
    }

    #[tokio::test]
    async fn timeout_kills_process_tree() {
        let dir = tempfile::tempdir().unwrap();
        let sup = Supervision {
            timeout: Duration::from_millis(300),
            ..Default::default()
        };
        let start = std::time::Instant::now();
        let out = run_supervised(
            &argv(&["/bin/sleep", "30"]),
            dir.path(),
            &[],
            dir.path(),
            "t",
            &sup,
        )
        .await
        .unwrap();
        assert_eq!(out.status, "timeout");
        assert!(start.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let sup = Supervision::default();
        let cancel = sup.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            cancel.store(true, Ordering::Relaxed);
        });
        let out = run_supervised(
            &argv(&["/bin/sleep", "30"]),
            dir.path(),
            &[],
            dir.path(),
            "t",
            &sup,
        )
        .await
        .unwrap();
        assert_eq!(out.status, "cancelled");
    }

    #[tokio::test]
    async fn env_is_stripped() {
        std::env::set_var("GARNISH_SECRET_TEST", "leaky");
        let dir = tempfile::tempdir().unwrap();
        let out = run_supervised(
            &argv(&["/usr/bin/env"]),
            dir.path(),
            &[],
            dir.path(),
            "t",
            &Supervision::default(),
        )
        .await
        .unwrap();
        assert!(!out.stdout_tail.contains("GARNISH_SECRET_TEST"));
    }

    #[tokio::test]
    async fn output_limit_truncates() {
        let dir = tempfile::tempdir().unwrap();
        let sup = Supervision {
            output_limit_bytes: 1024,
            ..Default::default()
        };
        let out = run_supervised(
            &argv(&["/usr/bin/yes"]),
            dir.path(),
            &[],
            dir.path(),
            "t",
            &Supervision {
                timeout: Duration::from_millis(500),
                ..sup
            },
        )
        .await
        .unwrap();
        assert!(out.truncated);
    }
}
