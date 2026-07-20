# ADR-0001: Rust control plane with static binaries

Status: accepted (2026-07-20)

## Context

The control plane must run identically on macOS arm64, Ubuntu 24 (VPS), and
WSL2, be installable as a single artifact, supervise child processes reliably,
and embed a transactional store. The user explicitly chose Rust for the static
binary property.

## Decision

- Rust (stable toolchain, edition 2024) for the entire control plane and CLI.
- Release targets: `aarch64-apple-darwin`, `x86_64-unknown-linux-musl`,
  `aarch64-unknown-linux-musl`. WSL2 is served by the Linux binaries.
- Core crates: `tokio` (async runtime, process supervision), `clap` (CLI),
  `axum` (loopback web UX/API), `rusqlite` with the `bundled` feature (SQLite
  compiled in — no system dependency, works with musl), `serde`/`serde_json`
  (events, JSON output), `tracing` (structured logs).
- Migrations: plain SQL files embedded in the binary, applied in order with a
  `schema_version` table and automatic pre-migration backup (ADR-0006).

## Consequences

- Single-file install and uniform behaviour across platforms; no runtime
  dependency on Python/Node for the control plane itself.
- Integrations that ship their own runtimes (CodexBar CLI tarballs, Agent of
  Empires binary + Node for ACP, tmux) remain external, probed by
  `garnish doctor`, and are optional per feature.
- Slower iteration than a scripting language; mitigated by keeping the control
  plane small and pushing variability behind traits (docs/contracts.md).
