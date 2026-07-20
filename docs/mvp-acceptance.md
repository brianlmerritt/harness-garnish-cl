# MVP acceptance criteria

MVP definition (user, 2026-07-20): working Rust binary on Linux, macOS, and
WSL2, driving the subscription CLIs, with multiple profiles per provider,
simple OpenAI/Anthropic API cost management, and a first web UX (ADR-0007).

The MVP is acceptable only when this demonstration passes — on macOS arm64 and
Ubuntu 24 (WSL2 = the Linux binary; spot-checked), with CI green using fakes
only (no subscription quota):

1. `garnish init` and `garnish doctor` report platform, backends (docker
   **and** podman on the VPS), agent CLIs with probed versions, and CodexBar
   availability, with honest "not available" states.
2. Register two projects (one overarching) plus a global backlog; add
   dependent tasks with explicit acceptance criteria and verification
   commands; a dependency cycle is rejected.
3. Per-project policy works: a project with schedule `WWWOOBB` refuses to
   start a task outside its window and schedules the wake time; per-project
   quota reserve and git permissions are enforced and visible via
   `garnish config explain`.
4. Quota gate: with a real or fixture CodexBar snapshot, a task below reserve
   headroom is declined and rescheduled for after reset; `unknown` quota
   follows the project's fail-open/closed policy.
5. Routing: a ready task routes to an adapter+profile (multiple profiles per
   provider configured) with the score breakdown and rationale recorded; manual
   pinning overrides.
6. Isolation: task gets a fresh worktree/branch and a constrained container
   (no credential mounts, no engine socket, network off by default) — verified
   by an automated negative test on both docker and podman.
7. Execution: a fake agent and at least one real adapter (Codex `exec` or
   Claude `-p`) run headless, stream structured events into run evidence, and
   cancel cleanly (no orphan processes/containers).
8. Recovery: kill the daemon mid-run; on restart the lease expires, the task
   returns to `ready`, and evidence is intact.
9. Handoff: pause a task; the handoff packet (goal, commits, commands+results,
   decisions, blocker, next safe action) lets a *different* adapter resume.
10. Verification: an independent verifier in a clean sandbox runs the declared
    commands against the produced commit; a false "done" claim from the
    implementer agent is caught and the task does not reach `review`.
11. API models + cost: one task (or verifier role) runs via the Anthropic or
    OpenAI provider (and the openai-compat path against a local/OpenRouter
    endpoint); per-call usage is recorded and `garnish cost` (and the web UX)
    shows per-project/day cost from the price table.
12. Web UX: `garnish web` on loopback with token auth shows projects, backlog,
    task detail with live run events, quota snapshots, and cost summary;
    approving a Class-2 request and pausing/cancelling a task from the browser
    works and is recorded identically to the CLI path.
13. Integration: the result is presented as branch + patch + logs + tests +
    approvals + quota/cost usage + remaining risks; nothing is pushed or
    merged; a `git status` on the user's checkout shows their own files
    untouched.

Out of MVP scope (tracked, not demoed): skills registry, MCP/ACP trust
controls beyond AoE composition, notifications/remote approvals, Tokscale
history, credential projection into containers, Apple Container, TUI.
Real-agent smoke tests exist but are opt-in and labelled quota-consuming.
