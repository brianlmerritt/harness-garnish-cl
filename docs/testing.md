# Testing guide

Three tiers. Tier 1 is safe everywhere; tier 2 needs container engines;
tier 3 consumes subscription quota and is always opt-in.

## Tier 1 — full suite, fakes only (any machine, CI-safe)

```
cargo test --workspace
```

No engines or agent logins needed; nothing touches provider quota.
Expected: all tests pass (≈50). Any failure on Linux/WSL2 is a portability
bug — report the test name and output.

## Tier 2 — real container engines (env-gated)

```
GARNISH_TEST_BACKENDS=docker,podman cargo test -p garnish-cli --test backends -- --nocapture
```

- Name only the engines installed on that machine (an engine you name but
  don't have fails deliberately).
- First run pulls `alpine:3.20` (needs network for the pull itself).
- Expected per engine: `[engine] happy path OK` and
  `[engine] network-off confirmed`.

## Tier 3 — real agents (OPT-IN, consumes quota)

```
cargo test -p garnish-cli --test real_smoke -- --ignored
```

Runs one tiny task through each of Claude Code, Codex, and Antigravity with
docker verification. Requires the CLIs authenticated on that machine. Run a
single agent with e.g. `-- --ignored real_codex_end_to_end`.

## Per-platform checklist

| Platform | Steps |
|---|---|
| macOS (dev) | Tier 1; Tier 2 with `GARNISH_TEST_BACKENDS=docker`; Tier 3 as desired |
| Ubuntu 24 VPS | Install rust (`rustup`), `git clone`, Tier 1, then Tier 2 with `GARNISH_TEST_BACKENDS=docker,podman`; also run `target/debug/garnish doctor` and check both engines show `ok` |
| WSL2 | Keep the clone on the Linux filesystem (not `/mnt/c`). Tier 1, then Tier 2 with `GARNISH_TEST_BACKENDS=podman` (add `docker` if Docker Desktop WSL integration is on) |

Rootless podman notes (VPS/WSL2): garnish passes `--userns=keep-id` so the
worktree mount stays writable; if `engine_happy_path` fails only on podman,
capture `podman info --format '{{.Host.Security.Rootless}}'` and the test
output — that's the most likely divergence point.
