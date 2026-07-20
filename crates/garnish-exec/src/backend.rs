use crate::spawn::{run_supervised, SpawnOutcome, Supervision};
use anyhow::Result;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Docker,
    Podman,
    Fake,
}

impl BackendKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            BackendKind::Docker => "docker",
            BackendKind::Podman => "podman",
            BackendKind::Fake => "fake",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetPhase {
    Off,
    // Phase 2+: SetupAllowlist(domains)
}

/// The single place isolation is defined (docs/contracts.md).
#[derive(Debug, Clone)]
pub struct SandboxSpec {
    pub image: String,
    /// Only writable mount: the task worktree, at /work.
    pub worktree: PathBuf,
    pub net: NetPhase,
    pub cpus: f32,
    pub memory_mb: u32,
    pub pids: u32,
}

impl SandboxSpec {
    pub fn new(image: &str, worktree: &Path) -> Self {
        Self {
            image: image.into(),
            worktree: worktree.to_path_buf(),
            net: NetPhase::Off,
            cpus: 2.0,
            memory_mb: 1024,
            pids: 256,
        }
    }
}

pub trait ContainerBackend: Send + Sync {
    fn kind(&self) -> BackendKind;
    /// Probe availability + version. Err = not usable.
    fn probe(&self) -> Result<String>;
    /// Run one command (argv array) inside an ephemeral sandbox with the
    /// worktree mounted rw at /work. Evidence goes to `evidence_dir`.
    fn exec<'a>(
        &'a self,
        spec: &'a SandboxSpec,
        argv: &'a [String],
        evidence_dir: &'a Path,
        label: &'a str,
        sup: &'a Supervision,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<SpawnOutcome>> + Send + 'a>>;
}

/// Docker/Podman via their CLIs — same argument shape, argv arrays only,
/// no engine socket ever mounted into the sandbox.
pub struct EngineBackend {
    kind: BackendKind,
    program: String,
}

impl EngineBackend {
    pub fn docker() -> Self {
        Self { kind: BackendKind::Docker, program: "docker".into() }
    }
    pub fn podman() -> Self {
        Self { kind: BackendKind::Podman, program: "podman".into() }
    }

    fn run_args(&self, spec: &SandboxSpec, argv: &[String]) -> Vec<String> {
        let mut a: Vec<String> = vec![
            "run".into(),
            "--rm".into(),
            "--cap-drop=ALL".into(),
            "--security-opt".into(),
            "no-new-privileges".into(),
            format!("--pids-limit={}", spec.pids),
            format!("--memory={}m", spec.memory_mb),
            format!("--cpus={}", spec.cpus),
            "-v".into(),
            format!("{}:/work", spec.worktree.display()),
            "-w".into(),
            "/work".into(),
        ];
        if matches!(spec.net, NetPhase::Off) {
            a.push("--network=none".into());
        }
        if self.kind == BackendKind::Podman {
            // rootless: keep host uid mapping so the worktree stays writable
            a.push("--userns=keep-id".into());
        }
        a.push(spec.image.clone());
        a.extend(argv.iter().cloned());
        a
    }
}

impl ContainerBackend for EngineBackend {
    fn kind(&self) -> BackendKind {
        self.kind
    }

    fn probe(&self) -> Result<String> {
        let out = std::process::Command::new(&self.program)
            .arg("--version")
            .output()?;
        anyhow::ensure!(out.status.success(), "{} --version failed", self.program);
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn exec<'a>(
        &'a self,
        spec: &'a SandboxSpec,
        argv: &'a [String],
        evidence_dir: &'a Path,
        label: &'a str,
        sup: &'a Supervision,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<SpawnOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let mut full = vec![self.program.clone()];
            full.extend(self.run_args(spec, argv));
            run_supervised(&full, &spec.worktree, &[], evidence_dir, label, sup).await
        })
    }
}

/// Fake backend: executes on the host in the worktree directory. Used by CI
/// and tests; carries no isolation and says so.
pub struct FakeBackend;

impl ContainerBackend for FakeBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Fake
    }

    fn probe(&self) -> Result<String> {
        Ok("fake backend (host execution, NO isolation)".into())
    }

    fn exec<'a>(
        &'a self,
        spec: &'a SandboxSpec,
        argv: &'a [String],
        evidence_dir: &'a Path,
        label: &'a str,
        sup: &'a Supervision,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<SpawnOutcome>> + Send + 'a>> {
        Box::pin(async move {
            run_supervised(argv, &spec.worktree, &[], evidence_dir, label, sup).await
        })
    }
}

pub fn backend_by_name(name: &str) -> Result<Box<dyn ContainerBackend>> {
    match name {
        "docker" => Ok(Box::new(EngineBackend::docker())),
        "podman" => Ok(Box::new(EngineBackend::podman())),
        "fake" => Ok(Box::new(FakeBackend)),
        other => anyhow::bail!("unknown backend: {other} (docker|podman|fake)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_args_are_constrained() {
        let spec = SandboxSpec::new("alpine:3.20", Path::new("/tmp/wt"));
        let args = EngineBackend::docker().run_args(&spec, &["true".into()]);
        let joined = args.join(" ");
        assert!(joined.contains("--network=none"));
        assert!(joined.contains("--cap-drop=ALL"));
        assert!(joined.contains("no-new-privileges"));
        assert!(joined.contains("/tmp/wt:/work"));
        assert!(!joined.contains("docker.sock"));
    }

    #[tokio::test]
    async fn fake_backend_runs_in_worktree() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("marker"), "x").unwrap();
        let spec = SandboxSpec::new("unused", dir.path());
        let out = FakeBackend
            .exec(
                &spec,
                &["/bin/ls".into()],
                dir.path(),
                "ls",
                &Supervision::default(),
            )
            .await
            .unwrap();
        assert!(out.ok());
        assert!(out.stdout_tail.contains("marker"));
    }
}
