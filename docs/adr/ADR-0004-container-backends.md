# ADR-0004: Docker and rootless Podman backends behind one trait

Status: accepted (2026-07-20)

## Context

The user requires both Docker and Podman support, tested on an Ubuntu 24 VPS.
Local macOS machine has Docker Desktop 29.6.1 only. Dagger **Container Use**
was evaluated for this role and rejected: Apache-2.0 and conceptually a good
fit (container-per-task, git-branch change capture), but effectively
unmaintained since ~Oct 2025 — last release 2025-08-19, an open issue asking
Dagger to archive it, and unanswered security reports (path traversal, git
option injection) from mid-2026. Its design informs ours; its code does not.

## Decision

- A `ContainerBackend` trait (docs/contracts.md) with three implementations in
  MVP: `docker`, `podman` (rootless preferred), and `fake` (for tests).
  Both real backends are exercised in CI on Linux; macOS CI covers Docker.
- Backends are driven via their CLIs with argv arrays (no shell, no socket
  mounted into task containers). Apple Container may be added later behind the
  same trait; it is not installed here and not in scope now.
- Task containers follow the spec's constraints: non-root user where feasible,
  only the task worktree mounted read-write, no Docker/Podman socket, no host
  home/SSH/keychain mounts, network off by default with per-phase allowlists,
  pinned image digests, resource limits, `no-new-privileges` and dropped
  capabilities where the backend supports them.
- Promotion of results is git-only: commits/patches from the isolated
  worktree, never file copies out of a container (Container Use's one idea we
  keep).
- Honest isolation claims per platform: on macOS/Windows the container runs in
  Docker Desktop's VM; subscription CLIs authenticated on the host run **on the
  host**, confined to the task worktree plus their own sandbox/permission
  flags, unless the user opts into task-scoped credential projection into a
  container (post-MVP). Documentation and `garnish doctor` must state which
  mode is in effect — worktree isolation is never described as full container
  isolation.

## Consequences

- One scheduler codepath for both engines; engine quirks (rootless UID
  mapping, seccomp defaults, network flags) live inside each backend impl with
  per-engine tests on the VPS.
- No dependency on the dormant Container Use project or the Dagger engine.
