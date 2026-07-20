-- Phase 2: daemon scheduling support.
ALTER TABLE tasks ADD COLUMN not_before TEXT;                          -- retry backoff / wake time
ALTER TABLE tasks ADD COLUMN pause_requested INTEGER NOT NULL DEFAULT 0;
CREATE TABLE control (key TEXT PRIMARY KEY, value TEXT NOT NULL);      -- e.g. pause_all=1
