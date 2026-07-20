use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

const MIGRATIONS: &[(&str, &str)] = &[
    ("0001_init", include_str!("../migrations/0001_init.sql")),
    ("0002_daemon", include_str!("../migrations/0002_daemon.sql")),
    ("0003_runs_adapter", include_str!("../migrations/0003_runs_adapter.sql")),
];

/// Open (creating if needed) the canonical database, applying pending
/// migrations. A timestamped backup is written before any migration runs on
/// an existing database (ADR-0006).
pub fn open(path: &Path) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let existed = path.exists();
    let conn = Connection::open(path)
        .with_context(|| format!("opening database {}", path.display()))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL)",
        [],
    )?;
    let current: i64 = conn
        .query_row("SELECT COALESCE(MAX(version), 0) FROM schema_version", [], |r| r.get(0))?;

    let pending: Vec<_> = MIGRATIONS
        .iter()
        .enumerate()
        .filter(|(i, _)| (*i as i64) >= current)
        .collect();

    if !pending.is_empty() {
        if existed && current > 0 {
            backup(path)?;
        }
        for (i, (name, sql)) in pending {
            let tx_result: Result<()> = (|| {
                conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))?;
                conn.execute("INSERT INTO schema_version (version) VALUES (?1)", [(i as i64) + 1])?;
                Ok(())
            })();
            tx_result.with_context(|| format!("applying migration {name}"))?;
        }
    }
    Ok(conn)
}

fn backup(path: &Path) -> Result<()> {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let backup = path.with_extension(format!("db.bak-{stamp}"));
    std::fs::copy(path, &backup)
        .with_context(|| format!("backing up database to {}", backup.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn open_and_migrate_twice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        {
            let conn = super::open(&path).unwrap();
            let v: i64 = conn
                .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, 3);
        }
        // Reopen: idempotent, no re-apply.
        let conn = super::open(&path).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 3);
    }
}
