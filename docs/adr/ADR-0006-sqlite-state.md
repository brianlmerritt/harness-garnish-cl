# ADR-0006: SQLite (WAL) canonical state with generated Markdown projections

Status: accepted (2026-07-20)

## Context

The brief rules out mutable Markdown as the transactional source of truth
(concurrency, atomicity, migrations, leasing, validation) while requiring
human-readable, editable project files.

## Decision

- Canonical state is one SQLite database in WAL mode at the global data
  directory (macOS: `~/Library/Application Support/harness-garnish/state.db`;
  Linux/WSL2: `$XDG_DATA_HOME/harness-garnish/state.db`, default
  `~/.local/share/...`). Schema in docs/data-model.md.
- Versioned SQL migrations embedded in the binary; a timestamped backup copy of
  the database is written before any migration runs.
- Each registered project gets generated projections under
  `<project>/.harness-garnish/`: `PROJECT.md`, `MEMORY.md`, `DECISIONS.md`,
  `TASKS.md`, `HANDOFF.md`, `agents/` stubs, and `runs/<run-id>/` evidence.
  Projections are regenerated from the database; a header marks generated
  files.
- Supported human edits (e.g. `MEMORY.md` facts, task priority fields) flow
  back only through `garnish sync` with validation and conflict reporting —
  never by the daemon silently re-reading arbitrary Markdown.
- Events are append-only rows; logs are summarised/archived rather than grown
  as one unbounded ledger. No private chain-of-thought is ever stored — goals,
  decisions, evidence, actions, results, blockers, next steps only.

## Consequences

- Crash-safe leases, idempotent transitions, and machine validation come from
  the database; readability comes from projections; the two never conflict
  because one direction is generation and the other is an explicit sync
  command.
- `rusqlite` with `bundled` SQLite keeps the static-binary property (ADR-0001).
