# ADR-0003: CodexBar CLI as primary quota provider; Tokscale optional history

Status: accepted (2026-07-20)

## Context

Routing needs provider-reported remaining subscription quota and reset times.
These come from undocumented per-provider endpoints that change often —
maintaining probes in-house is permanent chase-work. Investigated 2026-07-20:

- **CodexBar** (steipete/CodexBar) — MIT, Swift, extremely active (v0.45.2
  2026-07-19, 18.7k stars). Bundles **CodexBarCLI**, separable from the macOS
  menu-bar app, with prebuilt tarballs for macOS and Linux (glibc + musl).
  Confirmed interfaces: `codexbar usage --format json` (per-provider
  `usedPercent`, `windowMinutes`, `resetsAt`, multi-account flags) and
  `codexbar guard --provider X --min-remaining N --window session|weekly
  --json` with documented stable exit codes (0 safe / 1 below threshold /
  64 bad args / 69 unavailable). Covers Codex, Claude, and ~60 providers using
  existing logins.
- **Tokscale** (junhoyeo/tokscale) — MIT, Rust core + TS/Bun wrapper, very
  active (v4.5.3 2026-07-14). Parses local Claude/Codex/other session logs for
  historical tokens/cost; `tokscale --json`; needs Node/Bun; JSON schema
  informal.

## Decision

- The default `QuotaProvider` implementation shells out to CodexBar CLI
  (`usage` and `guard`), version-probed at `garnish doctor`, JSON parsed behind
  a versioned parser. Multi-account maps onto Garnish's per-provider profiles.
- Every quota snapshot records source, timestamp, confidence, staleness, and an
  explicit `unknown` reason. Remaining subscription quota is **never** inferred
  from local token totals.
- Tokscale is an optional historical-telemetry adapter (post-MVP); for MVP,
  simple API cost tracking for Anthropic/OpenAI is computed in-house from
  usage fields returned by the APIs themselves (ADR-0007).
- All parsers are replaceable; unknown/stale quota is a normal state handled by
  per-project fail-open/fail-closed policy, never silently treated as 100%.

## Consequences

- High-leverage reuse of a large community maintaining provider probes; Garnish
  contains no scraping of provider endpoints or credential stores.
- External binary dependency (per-platform tarball); if absent, quota state is
  `unknown` and policy decides — Garnish still functions.
- CodexBar's JSON schema may drift with its fast release cadence; guarded by
  fixtures and the stable `guard` exit codes.
