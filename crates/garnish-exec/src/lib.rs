pub mod backend;
pub mod git;
pub mod spawn;
pub mod worktree;

pub use backend::{backend_by_name, BackendKind, ContainerBackend, NetPhase, SandboxSpec};
pub use spawn::{SpawnOutcome, Supervision};
