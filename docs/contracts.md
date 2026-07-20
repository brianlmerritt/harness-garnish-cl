# Integration contracts

Rust trait sketches (signatures indicative, not final). Every implementation
declares capabilities and a supported version range; `garnish doctor` probes
executables (`--version`, help output) and refuses to route work to an
unprobed or out-of-range integration. No name-based assumptions, no vendor
flags without fixtures, argv arrays only — never shell interpolation.

## AgentAdapter

One per coding-agent CLI: `claude-code`, `codex`, `antigravity`, `fake`.

```rust
pub struct AgentCapabilities {
    pub versions: VersionReq,           // tested range, probed at doctor time
    pub headless: bool,                 // structured non-interactive mode
    pub pty: bool,                      // usable under supervised PTY (AoE)
    pub resume: bool, pub fork: bool,
    pub event_schema: &'static str,     // parser id + version, fixture-backed
    pub instruction_files: &'static [&'static str], // e.g. AGENTS.md, CLAUDE.md
    pub sandbox_flags: SandboxSupport,  // CLI's own permission/sandbox modes
    pub emits_usage: bool,
    pub models: ModelSelection,
}

pub trait AgentAdapter {
    fn capabilities(&self) -> &AgentCapabilities;
    fn probe(&self) -> Result<ProbeReport>;         // version, auth-profile presence (never credential values)
    fn build_invocation(&self, task: &TaskCtx, mode: Mode) -> Result<Invocation>; // argv, env allowlist, cwd
    fn parse_events(&self, raw: &[u8]) -> Result<Vec<AgentEvent>>;  // versioned parser
    fn resume_invocation(&self, task: &TaskCtx, handle: &SessionRef) -> Result<Invocation>;
    fn health_signatures(&self) -> &[FailureSignature];  // known failure patterns
}
```

Verified starting points (probed 2026-07-20): Codex CLI 0.144.6 `codex exec`
(+`resume`, JSONL); Claude Code 2.1.215 `claude -p` (stream-JSON, allowed
tools, permission modes, `--resume`); Antigravity 1.1.4 `agy --print`
(`--print-timeout`, `--conversation` resume, `--sandbox`, `--mode`). Each
adapter ships recorded fixtures; drift tests fail loudly on format change.

## ExecutionPlane

```rust
pub trait ExecutionPlane {
    fn kind(&self) -> PlaneKind;                       // builtin_headless | aoe | fake
    fn start(&self, inv: Invocation, sup: Supervision) -> Result<RunHandle>;
    fn send(&self, h: &RunHandle, input: &str) -> Result<()>;      // pty only
    fn events(&self, h: &RunHandle) -> EventStream;    // structured where available (ACP), else capture
    fn cancel(&self, h: &RunHandle, grace: Duration) -> Result<()>; // TERM → KILL, process-group wide
    fn status(&self, h: &RunHandle) -> Result<RunStatus>;
}
```

`Supervision` carries timeout, output byte limits, heartbeat interval, and the
cancellation token. The AoE implementation drives the pinned `aoe` binary via
its REST API/JSON CLI and reads `aoe acp tail` JSON-lines when the agent has
an ACP adapter (ADR-0002).

## ContainerBackend

```rust
pub trait ContainerBackend {
    fn kind(&self) -> BackendKind;                     // docker | podman | fake
    fn probe(&self) -> Result<BackendReport>;          // version, rootless?, features
    fn create(&self, spec: &SandboxSpec) -> Result<Sandbox>;
    fn exec(&self, sb: &Sandbox, cmd: &Command, net: NetPhase) -> Result<ExecResult>;
    fn destroy(&self, sb: &Sandbox) -> Result<()>;
    fn gc(&self, older_than: Duration) -> Result<GcReport>; // orphan cleanup after crashes
}
```

`SandboxSpec` is the single place isolation is defined: image digest, worktree
mount (rw) and nothing else writable, non-root user, resource limits
(cpu/mem/pids/wall/disk), dropped capabilities + `no-new-privileges`, seccomp
where supported, `NetPhase` = `off | setup_allowlist(domains) | task_allowlist`.
No socket mounts, no host home. Setup (dependency fetch) and task phases get
separate network policies.

## QuotaProvider

```rust
pub trait QuotaProvider {
    fn snapshot(&self, provider: &str, profile: &ProfileRef) -> Result<QuotaSnapshot>;
    fn guard(&self, provider: &str, profile: &ProfileRef, min_remaining_pct: f32, window: Window)
        -> Result<GuardDecision>;      // Safe | Below | Unknown(reason) — Unknown is a normal state
}
```

Default impl shells to CodexBar CLI (`usage --format json`, `guard --json`,
stable exit codes) per ADR-0003. `FakeQuotaProvider` replays fixture
snapshots including stale/unknown/below/reset scenarios for tests.

## ModelProvider (API/local models — ADR-0007)

```rust
pub trait ModelProvider {
    fn capabilities(&self) -> &ModelProviderCaps;      // models, context limits, tools, structured output, prices?
    fn complete(&self, req: ChatRequest) -> Result<ChatResponse>;  // incl. usage fields
    fn cancel(&self, h: &CallHandle) -> Result<()>;
}
```

Implementations: `anthropic`, `openai`, `openai-compat` (base URL + key —
covers Ollama, llama.cpp server, OpenRouter). Per-provider rate limits and
retry policy in config. Usage from every response feeds the `costs` table.
API billing is never mixed with subscription quota.

## Routing

Hard filters first: capability match, project policy (allowed agents/models),
schedule window (W/O/B), quota guard, agent availability. Then a documented
score over quota headroom (both windows), time-to-reset vs estimate +
uncertainty, historical success/verification-failure rate, context continuity,
cost, latency, and user preference. Manual pinning always wins. Every route
records its snapshot and rationale in `tasks.route_json`. Planner,
implementer, and verifier are separate roles; the verifier defaults to a
different profile (and optionally provider) for high-risk tasks, and
completion is decided by verification evidence, never agent claims.
