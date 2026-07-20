use crate::model::TaskStatus;
use anyhow::Result;
use rusqlite::Connection;

use TaskStatus::*;

/// Documented transitions (docs/data-model.md). Side states are reachable
/// from any active state; `Superseded` from anywhere non-terminal.
pub fn allowed(from: TaskStatus, to: TaskStatus) -> bool {
    if from == to {
        return true; // idempotent repeat is a no-op
    }
    let active = matches!(
        from,
        Ready | Leased | Planning | AwaitingApproval | Running | Verifying | Review | Paused | Blocked
    );
    match (from, to) {
        (Draft, Ready) => true,
        (Ready, Leased) => true,
        (Leased, Planning) => true,
        (Planning, AwaitingApproval) => true,
        (Planning, Running) => true,
        (AwaitingApproval, Running) => true,
        (Running, Verifying) => true,
        (Verifying, Review) => true,
        (Verifying, Failed) => true,
        (Verifying, Ready) => true, // retry within budget
        (Review, Completed) => true,
        (Failed, Ready) => true, // manual retry
        (Paused, Ready) => true, // resume
        (Blocked, Ready) => true,
        (Leased | Running, Failed) => true, // crash/agent failure
        (_, Paused) | (_, Blocked) | (_, Cancelled) if active => true,
        (_, Superseded) if from != Completed && from != Cancelled => true,
        _ => false,
    }
}

/// Validated, recorded, idempotent transition. Returns false when the task
/// was already in `to` (no-op), true when the transition happened.
pub fn transition(
    conn: &Connection,
    task_id: &str,
    from: TaskStatus,
    to: TaskStatus,
    reason: &str,
) -> Result<bool> {
    if !allowed(from, to) {
        anyhow::bail!(
            "invalid transition {} -> {} for task {task_id}",
            from.as_str(),
            to.as_str()
        );
    }
    if from == to {
        return Ok(false);
    }
    // Guard against concurrent movers: only update if still in `from`.
    let n = conn.execute(
        "UPDATE tasks SET status = ?1, updated_at = ?2 WHERE id = ?3 AND status = ?4",
        rusqlite::params![to.as_str(), crate::ids::now(), task_id, from.as_str()],
    )?;
    if n == 0 {
        let current: String =
            conn.query_row("SELECT status FROM tasks WHERE id = ?1", [task_id], |r| r.get(0))?;
        if current == to.as_str() {
            return Ok(false); // someone else already did it — idempotent
        }
        anyhow::bail!(
            "transition conflict for task {task_id}: expected {}, found {current}",
            from.as_str()
        );
    }
    crate::events::append(
        conn,
        Some(task_id),
        None,
        "transition",
        &serde_json::json!({ "from": from.as_str(), "to": to.as_str(), "reason": reason }),
    )?;
    Ok(true)
}

/// Return expired leases to `ready` (orphan recovery). Returns recovered ids.
pub fn recover_expired_leases(conn: &Connection) -> Result<Vec<String>> {
    let now = crate::ids::now();
    let mut stmt = conn.prepare(
        "SELECT id, status FROM tasks
         WHERE status IN ('leased','planning','running','verifying')
           AND lease_expires IS NOT NULL AND lease_expires < ?1",
    )?;
    let rows: Vec<(String, String)> = stmt
        .query_map([&now], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    let mut recovered = vec![];
    for (id, status) in rows {
        let from = TaskStatus::parse(&status)?;
        // Expired active work returns to ready via failed->ready semantics,
        // recorded explicitly as lease expiry.
        conn.execute(
            "UPDATE tasks SET status='ready', lease_owner=NULL, lease_expires=NULL, updated_at=?1 WHERE id=?2",
            rusqlite::params![crate::ids::now(), id],
        )?;
        crate::events::append(
            conn,
            Some(&id),
            None,
            "transition",
            &serde_json::json!({ "from": from.as_str(), "to": "ready", "reason": "lease expired (orphan recovery)" }),
        )?;
        recovered.push(id);
    }
    Ok(recovered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_allowed() {
        let path = [Draft, Ready, Leased, Planning, Running, Verifying, Review, Completed];
        for w in path.windows(2) {
            assert!(allowed(w[0], w[1]), "{:?} -> {:?}", w[0], w[1]);
        }
    }

    #[test]
    fn invalid_jumps_rejected() {
        assert!(!allowed(Draft, Running));
        assert!(!allowed(Completed, Ready));
        assert!(!allowed(Cancelled, Running));
        assert!(!allowed(Completed, Superseded));
    }

    #[test]
    fn approval_detour() {
        assert!(allowed(Planning, AwaitingApproval));
        assert!(allowed(AwaitingApproval, Running));
        assert!(allowed(AwaitingApproval, Blocked));
    }
}
