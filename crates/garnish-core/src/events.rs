use anyhow::Result;
use rusqlite::Connection;
use sha2::{Digest, Sha256};

/// Append an event to the tamper-evident chain:
/// hash = sha256(prev_hash || at || kind || data_json).
pub fn append(
    conn: &Connection,
    task_id: Option<&str>,
    run_id: Option<&str>,
    kind: &str,
    data: &serde_json::Value,
) -> Result<String> {
    let id = crate::ids::new_id();
    let at = crate::ids::now();
    let data_json = serde_json::to_string(data)?;
    // Order by rowid: ULIDs are not monotonic within one millisecond.
    let prev: String = conn
        .query_row(
            "SELECT hash FROM events ORDER BY rowid DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap_or_default();
    let mut h = Sha256::new();
    h.update(prev.as_bytes());
    h.update(at.as_bytes());
    h.update(kind.as_bytes());
    h.update(data_json.as_bytes());
    let hash = format!("{:x}", h.finalize());
    conn.execute(
        "INSERT INTO events (id, at, task_id, run_id, kind, data_json, hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![id, at, task_id, run_id, kind, data_json, hash],
    )?;
    Ok(id)
}

/// Verify the whole chain; returns the number of events checked.
pub fn verify_chain(conn: &Connection) -> Result<usize> {
    let mut stmt =
        conn.prepare("SELECT at, kind, data_json, hash FROM events ORDER BY rowid ASC")?;
    let rows: Vec<(String, String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<std::result::Result<_, _>>()?;
    let mut prev = String::new();
    for (i, (at, kind, data, hash)) in rows.iter().enumerate() {
        let mut h = Sha256::new();
        h.update(prev.as_bytes());
        h.update(at.as_bytes());
        h.update(kind.as_bytes());
        h.update(data.as_bytes());
        let expect = format!("{:x}", h.finalize());
        if *hash != expect {
            anyhow::bail!("event chain broken at index {i}");
        }
        prev = hash.clone();
    }
    Ok(rows.len())
}

#[cfg(test)]
mod tests {
    #[test]
    fn chain_appends_and_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("s.db")).unwrap();
        for i in 0..5 {
            super::append(&conn, None, None, "test", &serde_json::json!({ "i": i })).unwrap();
        }
        assert_eq!(super::verify_chain(&conn).unwrap(), 5);
        // Tamper with one row.
        conn.execute("UPDATE events SET data_json = '{\"i\":99}' WHERE kind='test' AND data_json LIKE '%\"i\":2%'", []).unwrap();
        assert!(super::verify_chain(&conn).is_err());
    }
}
