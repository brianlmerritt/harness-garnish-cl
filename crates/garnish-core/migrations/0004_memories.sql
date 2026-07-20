-- Project memory: curated durable facts with dated provenance (ADR-0006).
CREATE TABLE memories (
  id         TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  content    TEXT NOT NULL,
  source     TEXT NOT NULL DEFAULT 'user',   -- user | agent-proposal-accepted
  created_at TEXT NOT NULL
);
