# Quota reserves and forecasting

How Garnish decides whether a subscription agent may start work, what gets
recorded, and the planned estimate-aware forecasting refinement.

## Windows and reserves (implemented)

Subscription providers expose (via CodexBar) two rolling windows:

- **session** — the short window (e.g. Claude's 5-hour window);
- **weekly** — the long window.

Each project sets a reserve per window (`policy.quota.reserve_pct_session`,
default 15; `reserve_pct_weekly`, default 20). A reserve means: *Garnish must
leave at least this much of the window untouched for the human.* Before
routing a task to an adapter, both windows are checked independently against
their reserves — **two AND-ed hard gates**; the tighter window is always the
binding one.

### Worked example: session 10% left, weekly 50% left (defaults)

1. Session 10% < 15% reserve → gate fails, regardless of weekly.
2. The task is **declined, not failed**: it stays `ready` with `not_before`
   set to the session window's reset time (≤ 5h away). The daemon skips it
   until then; nothing burns the remaining 10%.
3. At reset the session gate passes and weekly (50% ≥ 20%) becomes the
   binding constraint until *it* nears its reserve — then the same
   decline/reschedule happens against the weekly reset.
4. The gate is per-candidate: if another allowed adapter (e.g. Codex) clears
   its own windows, the task routes there instead of waiting. Decline only
   happens when every candidate fails.
5. Unknown/stale quota is never treated as headroom: per-project
   `quota.unknown_quota` (`fail_closed` default / `fail_open`) decides.

### Scoring near the boundary

Passing the gates is not the end: the routing score uses the **minimum** of
the two windows' remaining percentages
(`score = 0.6 * min_window/100 + 0.4 * historical_success`), so a provider
close to either boundary loses routes to healthier rivals before the hard
gate ever triggers.

## What is tracked (implemented)

Every routing decision leaves an audit trail:

- **`quota_snapshots` table** — one row per window checked: provider,
  window, `remaining_pct` (NULL when unknown), `resets_at`, source
  (codexbar/fake), confidence, and an explicit `unknown_reason`. Written on
  every gate check and every `garnish quota status`.
- **`tasks.route_json`** — the full candidate report for the chosen route:
  per-adapter score, quota state (`ok` with `min_remaining_pct` /
  `unknown`+policy / exclusion reason), success rate, and pin/override
  provenance.
- **`events`** — `quota_declined` events carry the candidate report and the
  reschedule time; `route` events record skipped candidates.
- **`costs` table** — API-billing usage (tokens + USD), kept deliberately
  separate: local token counts are **never** used to infer remaining
  subscription quota (ADR-0003).

Inspect with: `garnish quota status`, `garnish task show <id> --json`
(`.task.route`), `garnish config explain --project X` (effective reserves and
their provenance), and the web UX quota panel.

## Forecasting (design — Phase 5, not yet implemented)

The current gate answers "is there headroom *now*?" It does not answer "will
*this task* fit in the headroom before the reset?" The spec requires
"estimated headroom exceeds configured reserve plus uncertainty"; today the
uncertainty is absorbed by setting reserves conservatively. The planned
refinement:

### Inputs

- **Historical usage per (adapter, window)**: completed runs already record
  usage tokens (`runs.usage_json`) and wall time. From CodexBar snapshots
  taken before/after runs, derive *observed window-percent consumed per run*
  — the only honest bridge between token counts and window percentage, since
  providers do not publish the exchange rate.
- **Task estimate**: `spec.estimated_minutes` plus an uncertainty class
  (small/medium/large by scope), refined over time by comparing estimates to
  actuals per project.

### Admission rule (replaces the bare reserve check)

```
expected_burn  = P50 window-% per run for this adapter, scaled by task size
uncertainty    = P90 - P50 of the same distribution (min 1 sample floor: 2x)
admit iff  remaining - reserve >= expected_burn + uncertainty     (both windows)
      and  estimated_minutes fits before min(resets_at, schedule window close)
           unless the task is checkpointable
```

- Sparse history (< 5 runs) → fall back to today's plain reserve gate and
  say so in the route rationale.
- A forecast is advisory input to a *deterministic* gate — an LLM never
  overrides it (spec: control plane deterministic where policy can be).

### Consequences of a forecast miss

Forecasts will be wrong; the system already tolerates that: a run that
exhausts the window mid-flight hits provider errors → run fails → retry with
backoff lands after `not_before`/reset; checkpointable tasks pause with a
handoff instead. Forecasting reduces how often that happens; it does not
need to be perfect.

### Acceptance tests to add with the implementation

- Task admitted when `remaining - reserve` covers P50+P90 burn; declined
  when it does not, with the forecast recorded in `route_json.forecast`.
- Sparse-history fallback labelled in the rationale.
- Estimated duration exceeding time-to-reset declines a non-checkpointable
  task but admits a checkpointable one.
- Forecast numbers appear in `garnish task show` and the web quota panel.
