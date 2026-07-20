# ADR-0007: Revised MVP — three-platform binary, subscriptions, API cost tracking, first web UX

Status: accepted (2026-07-20)

## Context

The build brief's original phasing put a web view in Phase 5 and API-model
providers in Phase 4. The user redefined the MVP (2026-07-20): a working Rust
binary on Linux, macOS, and WSL2, driving the subscription CLIs, with simple
OpenAI/Anthropic API cost management and a first web UX.

## Decision

MVP = spec Phases 1–3 plus two pulled-forward slices:

1. **API providers (thin slice):** a `ModelProvider` trait with Anthropic and
   OpenAI implementations plus one OpenAI-compatible implementation covering
   Ollama, llama.cpp server, and OpenRouter (same wire format, different base
   URL/key). User-selectable models. Cost tracking is *simple*: record the
   usage fields each API returns per call, price them from a bundled editable
   price table, aggregate per project/task/day. No harness rebuild around raw
   APIs — API agents get the same task contract and a minimal tool loop, and
   richer SDK integration stays post-MVP.
2. **First web UX (thin slice):** `garnish web` serves an axum app on
   loopback only with a bearer token. Read: projects, backlog, task detail,
   run evidence, quota snapshots, cost summaries. Write: exactly the approval
   actions (approve/deny) and pause/resume/cancel — the same operations the
   CLI exposes, through the same policy engine. No remote exposure design in
   MVP; users who want remote access tunnel via SSH/Tailscale at their own
   configuration.

Deferred out of MVP: skills registry, MCP/ACP trust controls beyond what AoE
composition needs, notifications/remote approvals, Tokscale history, credential
projection into containers, Apple Container backend, TUI.

## Consequences

- The demo criteria in docs/mvp-acceptance.md supersede the spec's list.
- Web UX being a thin client over the same internal API keeps the control
  plane CLI-first and avoids a second policy path.
