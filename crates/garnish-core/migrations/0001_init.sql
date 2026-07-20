CREATE TABLE projects (
  id            TEXT PRIMARY KEY,
  name          TEXT NOT NULL UNIQUE,
  root_path     TEXT NOT NULL,
  kind          TEXT NOT NULL DEFAULT 'normal',
  manifest_json TEXT NOT NULL DEFAULT '{}',
  policy_json   TEXT NOT NULL DEFAULT '{}',
  created_at    TEXT NOT NULL,
  archived_at   TEXT
);

CREATE TABLE profiles (
  id          TEXT PRIMARY KEY,
  provider    TEXT NOT NULL,
  name        TEXT NOT NULL,
  config_json TEXT NOT NULL DEFAULT '{}',
  UNIQUE(provider, name)
);

CREATE TABLE tasks (
  id            TEXT PRIMARY KEY,
  project_id    TEXT NOT NULL REFERENCES projects(id),
  title         TEXT NOT NULL,
  spec_json     TEXT NOT NULL,
  priority      INTEGER NOT NULL DEFAULT 0,
  deadline      TEXT,
  status        TEXT NOT NULL DEFAULT 'draft',
  lease_owner   TEXT,
  lease_expires TEXT,
  heartbeat_at  TEXT,
  retry_budget  INTEGER NOT NULL DEFAULT 2,
  cancel_requested INTEGER NOT NULL DEFAULT 0,
  git_json      TEXT,
  route_json    TEXT,
  created_at    TEXT NOT NULL,
  updated_at    TEXT NOT NULL
);

CREATE TABLE task_deps (
  task_id    TEXT NOT NULL REFERENCES tasks(id),
  depends_on TEXT NOT NULL REFERENCES tasks(id),
  PRIMARY KEY (task_id, depends_on)
);

CREATE TABLE runs (
  id           TEXT PRIMARY KEY,
  task_id      TEXT NOT NULL REFERENCES tasks(id),
  attempt      INTEGER NOT NULL,
  profile_id   TEXT,
  model        TEXT,
  mode         TEXT NOT NULL,
  backend      TEXT NOT NULL,
  started_at   TEXT,
  ended_at     TEXT,
  exit_status  TEXT,
  usage_json   TEXT,
  evidence_dir TEXT NOT NULL
);

CREATE TABLE events (
  id        TEXT PRIMARY KEY,
  at        TEXT NOT NULL,
  task_id   TEXT,
  run_id    TEXT,
  kind      TEXT NOT NULL,
  data_json TEXT NOT NULL,
  hash      TEXT NOT NULL
);

CREATE TABLE approvals (
  id                 TEXT PRIMARY KEY,
  task_id            TEXT REFERENCES tasks(id),
  requested_at       TEXT NOT NULL,
  action_json        TEXT NOT NULL,
  expires_at         TEXT NOT NULL,
  status             TEXT NOT NULL DEFAULT 'pending',
  decided_at         TEXT,
  decided_via        TEXT,
  reusable_rule_json TEXT
);

CREATE TABLE quota_snapshots (
  id             TEXT PRIMARY KEY,
  at             TEXT NOT NULL,
  provider       TEXT NOT NULL,
  profile_id     TEXT,
  window         TEXT NOT NULL,
  used_pct       REAL,
  remaining_pct  REAL,
  resets_at      TEXT,
  source         TEXT NOT NULL,
  confidence     TEXT NOT NULL,
  unknown_reason TEXT
);

CREATE TABLE costs (
  id            TEXT PRIMARY KEY,
  at            TEXT NOT NULL,
  run_id        TEXT REFERENCES runs(id),
  project_id    TEXT NOT NULL,
  provider      TEXT NOT NULL,
  model         TEXT NOT NULL,
  input_tokens  INTEGER,
  output_tokens INTEGER,
  cache_tokens  INTEGER,
  usd           REAL
);

CREATE TABLE artifacts (
  id     TEXT PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES runs(id),
  kind   TEXT NOT NULL,
  path   TEXT NOT NULL,
  sha256 TEXT
);

CREATE INDEX idx_tasks_project_status ON tasks(project_id, status);
CREATE INDEX idx_events_task ON events(task_id, at);
CREATE INDEX idx_runs_task ON runs(task_id);
