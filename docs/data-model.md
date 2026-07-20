# Data model

Canonical state: single SQLite database, WAL mode (ADR-0006). Times are UTC
ISO-8601 strings; IDs are ULIDs (sortable, no coordination). JSON columns hold
validated documents whose schemas are versioned in code.

## Tables (schema v1 sketch)

```sql
CREATE TABLE schema_version (version INTEGER NOT NULL);

CREATE TABLE projects (
  id            TEXT PRIMARY KEY,
  name          TEXT NOT NULL UNIQUE,
  root_path     TEXT NOT NULL,           -- absolute path on this machine
  kind          TEXT NOT NULL,           -- normal | overarching
  manifest_json TEXT NOT NULL,           -- repos, environments, declared submodules (pinned), build/test commands
  policy_json   TEXT NOT NULL,           -- per-project policy: quota, git, autonomy, schedule (docs/policy-model.md)
  created_at    TEXT NOT NULL,
  archived_at   TEXT
);

CREATE TABLE profiles (                  -- agent/account profiles; multiple per provider
  id            TEXT PRIMARY KEY,
  provider      TEXT NOT NULL,           -- claude-code | codex | antigravity | anthropic-api | openai-api | openai-compat
  name          TEXT NOT NULL,           -- e.g. "codex-personal", "claude-work"
  config_json   TEXT NOT NULL,           -- auth *reference* (env name, keychain item, config dir), base_url, default model
  UNIQUE(provider, name)
);

CREATE TABLE tasks (
  id              TEXT PRIMARY KEY,
  project_id      TEXT NOT NULL REFERENCES projects(id),
  title           TEXT NOT NULL,
  spec_json       TEXT NOT NULL,         -- goal, rationale, scope, non_scope, acceptance_criteria[],
                                         -- verification_commands[], estimates (wall time, uncertainty, context),
                                         -- checkpoint_strategy, allowed/disallowed agents-models-skills-networks-
                                         -- secrets-files-commands-backends, integration_policy, risk_tier
  priority        INTEGER NOT NULL DEFAULT 0,
  deadline        TEXT,
  status          TEXT NOT NULL,         -- state machine below
  lease_owner     TEXT,                  -- scheduler instance id
  lease_expires   TEXT,
  heartbeat_at    TEXT,
  retry_budget    INTEGER NOT NULL DEFAULT 2,
  cancel_token    TEXT,
  git_json        TEXT,                  -- worktree path, branch, base/current commit, submodule revisions
  route_json      TEXT,                  -- chosen profile/model, quota snapshot id, score breakdown, rationale
  created_at      TEXT NOT NULL,
  updated_at      TEXT NOT NULL
);

CREATE TABLE task_deps (
  task_id    TEXT NOT NULL REFERENCES tasks(id),
  depends_on TEXT NOT NULL REFERENCES tasks(id),
  PRIMARY KEY (task_id, depends_on)      -- cycle check enforced in code at insert
);

CREATE TABLE runs (
  id           TEXT PRIMARY KEY,
  task_id      TEXT NOT NULL REFERENCES tasks(id),
  attempt      INTEGER NOT NULL,
  profile_id   TEXT REFERENCES profiles(id),
  model        TEXT,
  mode         TEXT NOT NULL,            -- headless | pty | verify
  backend      TEXT NOT NULL,            -- docker | podman | fake | host-cli
  started_at   TEXT, ended_at TEXT,
  exit_status  TEXT,                     -- ok | failed | timeout | cancelled | crashed
  usage_json   TEXT,                     -- tokens in/out/cache, api cost, wall time
  evidence_dir TEXT NOT NULL             -- .harness-garnish/runs/<run-id>/
);

CREATE TABLE events (                    -- append-only; never updated
  id         TEXT PRIMARY KEY,
  at         TEXT NOT NULL,
  task_id    TEXT, run_id TEXT,
  kind       TEXT NOT NULL,              -- transition | route | quota_snapshot | approval | process | file_change |
                                         -- test | retry | handoff | cleanup | policy_denial
  data_json  TEXT NOT NULL,
  hash       TEXT NOT NULL               -- sha256(prev.hash || canonical(this)) — tamper-evident chain
);

CREATE TABLE approvals (
  id           TEXT PRIMARY KEY,
  task_id      TEXT REFERENCES tasks(id),
  requested_at TEXT NOT NULL,
  action_json  TEXT NOT NULL,            -- action, target, effect, command, risk class, reversibility, scope
  expires_at   TEXT NOT NULL,
  status       TEXT NOT NULL,            -- pending | approved | denied | expired | revoked
  decided_at   TEXT, decided_via TEXT,   -- cli | web
  reusable_rule_json TEXT                -- narrow rule if the user granted one
);

CREATE TABLE quota_snapshots (
  id         TEXT PRIMARY KEY,
  at         TEXT NOT NULL,
  provider   TEXT NOT NULL, profile_id TEXT,
  window     TEXT NOT NULL,              -- session | weekly | ...
  used_pct   REAL, remaining_pct REAL, resets_at TEXT,
  source     TEXT NOT NULL,              -- codexbar | api | manual | fake
  confidence TEXT NOT NULL,              -- high | medium | low | unknown
  unknown_reason TEXT
);

CREATE TABLE costs (                     -- simple API cost ledger (ADR-0007)
  id         TEXT PRIMARY KEY,
  at         TEXT NOT NULL,
  run_id     TEXT REFERENCES runs(id),
  project_id TEXT NOT NULL,
  provider   TEXT NOT NULL, model TEXT NOT NULL,
  input_tokens INTEGER, output_tokens INTEGER, cache_tokens INTEGER,
  usd        REAL                        -- priced from bundled editable price table; NULL if unknown
);

CREATE TABLE artifacts (
  id      TEXT PRIMARY KEY,
  run_id  TEXT NOT NULL REFERENCES runs(id),
  kind    TEXT NOT NULL,                 -- patch | commit_ref | log | verification | handoff | summary
  path    TEXT NOT NULL,
  sha256  TEXT
);
```

## Task state machine

```
draft → ready → leased → planning → awaiting_approval → running → verifying → review → completed
```

Side states reachable from any active state: `paused`, `blocked`, `failed`,
`cancelled`, `superseded`.

| From | To | Trigger |
|---|---|---|
| draft | ready | spec validated, acceptance criteria + verification commands present |
| ready | leased | scheduler acquires lease (project schedule window open, deps done, quota gate passed) |
| leased | planning | agent/route chosen, worktree created |
| planning | awaiting_approval | plan or Class ≥2 action needs approval |
| planning / awaiting_approval | running | policy auto-permits, or approval granted |
| running | verifying | agent run ended with claimed success |
| verifying | review | verification commands passed in clean sandbox |
| verifying | failed | verification failed and retry budget exhausted (else → ready with retry decrement) |
| review | completed | user integrates or accepts the presented patch/branch |
| any active | paused | user pause / schedule window closed (checkpoint + handoff packet written) |
| any active | blocked | dependency, quota, or approval expired/denied |
| any active | cancelled | durable cancellation |
| any | superseded | replaced by a newer task |

Rules: every transition is validated against this table, idempotent (repeat of
the same transition is a no-op, not an error), and recorded as an `events` row.
Leases carry `lease_expires`; an expired lease returns the task to `ready` via
orphan recovery, so a crashed process never leaves a permanently leased task.
`awaiting_approval` and `paused` always have a handoff packet (spec §handoff)
in the run evidence directory so any compatible agent can resume.

## Projections

Generated from these tables into `<project>/.harness-garnish/` (ADR-0006):
`PROJECT.md` (from `projects.manifest_json`), `TASKS.md` (tasks by status),
`HANDOFF.md` (latest handoff packet of the active/paused task),
`DECISIONS.md`, `MEMORY.md` (curated facts table, size-limited, dated
provenance — the one projection with a human-edit → `garnish sync` path),
`agents/` vendor stubs (AGENTS.md, CLAUDE.md pointing at PROJECT.md), and
`runs/<run-id>/` evidence (manifest.json, events.jsonl, stdout/stderr logs,
verification.json, summary.md, patch refs).
