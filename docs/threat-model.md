# Threat model

Scope: single-user local tool; attacker-controlled inputs are repository
content, agent/tool output, and network responses — not other local users.
Web UX is loopback-only with bearer auth (ADR-0007).

| # | Threat | Mitigations |
|---|---|---|
| 1 | Prompt-injected repo content steers an agent into harmful actions | Risk-class policy outside the model (Class ≥2 gated); network off by default; worktree-only writes; promotion via reviewed patches; independent verifier |
| 2 | Hostile tool/MCP/website output | Same policy gates; MVP attaches no third-party MCP servers; AoE/CodexBar outputs parsed with strict schemas, treated as data |
| 3 | Shell injection / unsafe argv | argv arrays everywhere, no `shell=true`, no prompt interpolation into shell strings; injection attempts covered in tests |
| 4 | Path traversal, symlink escape, glob expansion, mount confusion | Canonicalise + prefix-check every mount/write path against the task worktree; symlinks resolved before validation; container mounts built from validated `SandboxSpec` only (lesson from Container Use issue #337) |
| 5 | Secret exposure via env/argv/logs/patches/context | Env allowlist per invocation; secrets by reference (keychain/env provider), injected short-lived, never in argv; redaction at ingestion and presentation; verification diff scanned for secret patterns |
| 6 | Dependency compromise / cache poisoning | Per-project isolated caches; setup-phase domain allowlists; pinned image digests; SBOM/provenance recorded per run |
| 7 | Container breakout / dangerous mounts | No docker/podman socket in task containers; non-root, dropped caps, `no-new-privileges`, seccomp; resource limits; honest per-platform isolation claims (ADR-0004) |
| 8 | Cross-project / cross-account leakage | One worktree+sandbox per task; per-profile auth references; caches keyed by project; events/artifacts scoped by project id |
| 9 | Agent weakens policy, tampers audit, self-verifies | Policy files and DB are outside every mount; hash-chained append-only events; verifier runs in a clean sandbox against the produced commit, chosen by policy not by the implementer agent |
| 10 | Runaway loops, fork bombs, disk/token spend | Retry budgets + backoff + circuit breakers; PID/cpu/mem/disk/wall limits; per-project daily/task USD budgets; pause-all and emergency stop |
| 11 | Unauthenticated control surface / remote approval spoofing | Web/API binds 127.0.0.1 only; bearer token required for state-changing calls; no remote design in MVP — remote access is user-managed SSH/Tailscale tunneling |
| 12 | Malicious "instructions" inside task/handoff content | Handoff packets are structured evidence (commits, commands, results), rendered as data; approvals always display the concrete command to a human |
| 13 | Credential store scraping | Garnish never reads credential values; adapters record only the *location kind* of an auth profile; CodexBar reuses the user's own logins in its own process |
| 14 | Backup/support-bundle leakage | Bundles opt-in and redacted; DB backups stay in the protected data dir (0700); retention limits on logs/events |

Residual risks (documented, accepted for MVP): host-run subscription CLIs are
confined by worktree + the CLI's own sandbox flags, not a container (stated
honestly per ADR-0004); Docker Desktop on macOS places containers in a shared
VM; CodexBar/AoE are trusted third-party binaries — version-pinned and
checksummed at install, but not audited line-by-line.
