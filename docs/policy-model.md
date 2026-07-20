# Policy model

Policy is enforced by the control plane, outside any model. An agent can
propose; it cannot approve, escalate, edit the policy authorising its current
run, or mark its own work verified.

## Risk classes

| Class | Examples | Default handling |
|---|---|---|
| 0 | state reads, planning, status, diffs | automatic |
| 1 | writes + tests inside the task worktree/container, within declared scope | automatic when project policy permits ("reasonable autonomy within container") |
| 2 | dependency downloads, selected secrets, new external services, host config, long/high-cost runs | pre-approved per-project rule or human approval |
| 3 | push/PR/merge, deploy, migrations, external messages, broad network, credential changes, destructive git, deleting persistent data, writes outside scope | explicit human approval, always |

Approval requests show action, target, concrete effect, exact command/API
call, risk, reversibility, requested scope, expiry, and safer alternatives.
One-shot unless the user grants a narrow reusable rule; all recorded.

## Per-project policy (`projects.policy_json`)

Set at `garnish project add`, edited via `garnish project policy`; `garnish
config explain` shows provenance (global default → project → task override).

```jsonc
{
  "schedule": {
    "week": "WWWOOBB",            // Mon→Sun; W=working hours only, O=off-hours only, B=any time, -=never
    "working_hours": null          // null → inherit global, e.g. {"start":"09:00","end":"17:30","tz":"Europe/London"}
  },
  "quota": {
    "reserve_pct": {"session": 15, "weekly": 20},
    "max_task_minutes": 45,
    "unknown_quota": "fail_closed" // fail_closed | fail_open | ask (interactive only)
  },
  "git": {
    "task_branches": true,         // create worktree branches + commit within them
    "push": "never",               // never | approval | auto
    "pr": "never", "merge": "never",
    "submodule_update": "never",
    "branch_prefix": "garnish/"
  },
  "autonomy": {
    "auto_class_max": 1,           // classes ≤ this run unattended (in-sandbox)
    "class2_rules": [],            // narrow pre-approvals, e.g. {"action":"net_allow","domains":["crates.io"]}
    "network_default": "off"
  },
  "agents": {
    "allowed": ["claude-code/*", "codex/*", "antigravity/*", "anthropic-api/*", "openai-api/*"],
    "pinned": null,
    "verifier": {"independent": true, "prefer_different_provider_for_risk": ">=2"}
  },
  "budget": {"max_usd_per_day": null, "max_usd_per_task": null}
}
```

## W/O/B scheduling

Global config defines the working-hours window per weekday and timezone. Each
project's `schedule.week` is seven characters, Monday→Sunday:

- `W` — tasks for this project may **start** only inside working hours;
- `O` — only outside working hours;
- `B` — any time that day;
- `-` — never that day.

Example `WWWOOBB`: Mon–Wed during work hours, Thu–Fri off-hours only,
weekend unrestricted. The gate applies to *starting* a phase; a running task
crossing the boundary is checkpointed and paused at the next safe point
(handoff packet written), matching the quota-boundary behaviour. The daemon's
wake-time calculation includes the next window opening alongside quota resets.

## Precedence

`organisation/managed (none yet) → global defaults → project policy → task
override (only fields the project marks overridable)`. Unknown keys in any
policy document are validation errors, not silently ignored. Schema versioned
and migrated with the database.

## Quota gating

See [quota-reserves-and-forecasting.md](quota-reserves-and-forecasting.md)
for the full reserve semantics, a worked example, what is recorded where,
and the planned estimate-aware forecasting.

Before leasing: `guard(provider, profile, reserve_pct, window)` for both
session and weekly windows. `Below` → reschedule for after `resets_at`.
`Unknown` → per-project `unknown_quota` policy (default fail-closed when the
daemon runs unattended; `ask` only in interactive runs). Estimated duration +
uncertainty must fit inside `min(time_to_reset, schedule window remaining,
max_task_minutes)` unless the task declares itself checkpointable.
