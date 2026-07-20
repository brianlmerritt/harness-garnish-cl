# Harness Garnish

A local control plane for AI-assisted software development. Garnish coordinates
coding-agent CLIs (Claude Code, Codex CLI, Antigravity) and API/local-model
agents, selects work that can safely finish within available quota and time,
runs it in isolated git worktrees and containers, verifies results
independently, and leaves a transparent audit trail.

Status: **Phase 1 — tested vertical slice**. `garnish` builds and runs the
full loop: create task → route → execute in an isolated worktree (fake or
real agent) → independently verify in a clean sandbox (docker/podman/fake)
→ present a patch. See [docs/mvp-acceptance.md](docs/mvp-acceptance.md) for
what is and is not yet covered.

```
cargo build --workspace          # binaries: garnish, fake-agent
cargo test --workspace           # unit + e2e tests, no provider quota used
target/debug/garnish doctor      # probe engines, agent CLIs, quota tooling
```

- Build brief: [Harness-Garnish-build-specification.md](Harness-Garnish-build-specification.md)
- CLI name: `garnish` (`hg` collides with Mercurial)
- Language: Rust (static binaries for macOS arm64, Linux x86_64/arm64; WSL2 uses the Linux binary)

## Design documents

| Document | Contents |
|---|---|
| [docs/architecture.md](docs/architecture.md) | Planes, components, platform matrix |
| [docs/data-model.md](docs/data-model.md) | SQLite schema, task state machine |
| [docs/contracts.md](docs/contracts.md) | Adapter/backend/provider trait contracts |
| [docs/policy-model.md](docs/policy-model.md) | Risk classes, per-project policy, W/O/B scheduling |
| [docs/threat-model.md](docs/threat-model.md) | Threats and mitigations |
| [docs/mvp-acceptance.md](docs/mvp-acceptance.md) | Revised MVP scope and demo criteria |
| [docs/adr/](docs/adr/) | Architecture decision records |

## Confirmed decisions (2026-07-20)

- Platforms: macOS (arm64), Linux (Ubuntu 24 VPS), WSL2. Windows native is out of scope; WSL2 runs the Linux binary.
- Container backends: Docker **and** rootless Podman, both supported and tested, behind one trait.
- Execution plane: hybrid — built-in structured headless spawning; Agent of Empires composed behind a versioned trait for supervised PTY sessions, replaceable later.
- Agents: Claude Code, Codex CLI, Antigravity CLI; multiple account profiles per provider.
- API/local models: Anthropic and OpenAI APIs plus OpenAI-compatible endpoints (Ollama, llama.cpp, OpenRouter), user-selectable models.
- Quota policy, git permissions, autonomy, and W/O/B day scheduling are configured **per project**.
- Canonical state: versioned SQLite (WAL); human-readable projections in each project's `.harness-garnish/`.
- MVP includes a first loopback web UX and simple OpenAI/Anthropic API cost tracking.
