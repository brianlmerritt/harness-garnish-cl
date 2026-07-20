# ADR-0002: Hybrid execution plane; compose over Agent of Empires for PTY

Status: accepted (2026-07-20)

## Context

Two execution styles are needed: structured non-interactive runs (preferred for
bounded tasks — exit codes, machine-readable events) and supervised PTY
sessions (interactive-only flows, approval prompts). Investigation (2026-07-20)
of session-manager candidates:

- **Agent of Empires (AoE)** — MIT, Rust + TS, very active (v1.13.0
  2026-07-16), ~2 core maintainers, Mozilla.ai-adjacent. Explicitly designed to
  be driven by external orchestrators: documented REST API
  (`POST /api/sessions` with worktree/sandbox options, `send`, `output`,
  bearer-token auth, `--read-only` mode), JSON CLI, and a structured ACP layer
  (`aoe acp tail/history` emitting JSON-lines transcripts of plans, tool calls,
  approvals). Sandboxes: Docker, Podman, Apple Containers. Hard tmux
  dependency for terminal mode; ACP mode needs Node 20+.
- **Agent Deck** — MIT, Go, very active but ~95% single-author; JSON CLI;
  output capture is a tmux pane-scrape heuristic; REST surface undocumented.

## Decision

1. Garnish implements **structured headless execution in-house**: it spawns
   `codex exec`, `claude -p`, and `agy --print` directly (argv arrays, no
   shell), parsing each CLI's documented JSON/JSONL output. All three installed
   CLIs have verified headless modes (probed 2026-07-20: Claude Code 2.1.215,
   Codex CLI 0.144.6, Antigravity 1.1.4).
2. Supervised PTY sessions are provided by **composing over AoE** behind the
   `ExecutionPlane` trait: version-pinned binary, driven via its REST API and
   JSON CLI, structured state read from ACP transcripts where the agent
   supports ACP, pane capture otherwise.
3. The `ExecutionPlane` trait is the boundary: AoE can be replaced (by Agent
   Deck, a native PTY layer, or anything else) without touching the scheduler,
   policy, or state code. A `FakeExecutionPlane` implements the trait for
   tests.

## Consequences

- No in-house PTY/tmux supervision code in the MVP; the hardest commodity
  problem is delegated to an active MIT project.
- Version drift risk: AoE is ~6 months old and fast-moving. Mitigation: pin the
  tested version range, probe at `garnish doctor`, keep fixtures of its JSON
  output, and treat the trait as the compatibility firewall.
- tmux (and Node 20+ for ACP mode) become optional runtime dependencies, needed
  only when supervised sessions are used.
- MVP can ship on headless mode alone if AoE integration slips; PTY composition
  is not on the MVP critical path (docs/mvp-acceptance.md).
