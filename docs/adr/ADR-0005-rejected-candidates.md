# ADR-0005: Candidates not adopted — DeerFlow, Ivy Tendril, Container Use, Agent Deck

Status: accepted (2026-07-20)

## Context

The build brief mandated an adopt/fork/compose investigation of seven
projects before greenfield work. Adopted/composed: Agent of Empires
(ADR-0002), CodexBar CLI (ADR-0003), optionally Tokscale (ADR-0003).
This ADR records the rejections and their reasons, verified 2026-07-20.

## DeerFlow (bytedance/deer-flow) — not adopted

MIT, very active, 77k stars. Version 2.0 (June 2026) rewrote it from a
LangGraph planning/research framework into a **full agent harness** (sub-agent
orchestration, memory, sandboxes, IM integrations) whose scope overlaps
Garnish's own. Extracting "just the planner" means adopting LangChain +
LangGraph + a server + Redis/Postgres. The planning loop Garnish needs is
small and is built in-house. DeerFlow could later be attached as an optional
sidecar planning service via its HTTP gateway if ever justified.

## Ivy Tendril (Ivy-Interactive/Ivy-Tendril) — prior art only, code reuse prohibited

Turned out to be a near-identical competitor product: agent-agnostic coding
orchestration with worktree isolation, verification gates, multi-CLI support,
cost tracking, PR lifecycle. C#/.NET 10, very active. Licensed
**FSL-1.1-ALv2** (source-available, not OSI despite "open source" marketing):
prohibits any use that provides "the same or substantially similar
functionality" — which describes Garnish exactly. Each version converts to
Apache-2.0 only after two years. **No code, assets, or text from Tendril may
be copied into Garnish.** Its lifecycle design (Draft→Execute→Review→PR,
verification-gate feedback loops) is legitimate prior art to study.

## Dagger Container Use — design reference only

See ADR-0004: dormant since ~Oct 2025, open archive request, unanswered
security issues. Its git-branch-per-environment promotion model is retained as
a design idea; the code and the Dagger engine dependency are not.

## Agent Deck (asheshgoplani/agent-deck) — fallback, not primary

MIT, Go, very active but ~95% single-author. JSON CLI and worktree/Docker
support are real, but output capture is a tmux pane-scrape heuristic and the
REST surface is undocumented. Agent of Empires offers a documented API and
structured ACP transcripts (ADR-0002). Agent Deck remains a viable alternative
`ExecutionPlane` implementation if AoE falters; no adapter is built for it in
the MVP.

## Consequences

- Required notices: none — no code is copied from any rejected project.
- Re-evaluate this ADR if AoE stalls (promote Agent Deck or in-house PTY) or
  if a Tendril version of interest reaches its Apache-2.0 conversion date.
