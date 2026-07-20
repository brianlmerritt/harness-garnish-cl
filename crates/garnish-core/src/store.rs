use crate::model::*;
use crate::policy::ProjectPolicy;
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashSet;

// ---------- projects ----------

pub fn project_add(
    conn: &Connection,
    name: &str,
    root_path: &str,
    kind: &str,
    policy: &ProjectPolicy,
) -> Result<Project> {
    policy.validate()?;
    let p = Project {
        id: crate::ids::new_id(),
        name: name.into(),
        root_path: root_path.into(),
        kind: kind.into(),
        manifest: serde_json::json!({}),
        policy: policy.clone(),
        created_at: crate::ids::now(),
    };
    conn.execute(
        "INSERT INTO projects (id, name, root_path, kind, manifest_json, policy_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            p.id,
            p.name,
            p.root_path,
            p.kind,
            serde_json::to_string(&p.manifest)?,
            serde_json::to_string(&p.policy)?,
            p.created_at
        ],
    )
    .with_context(|| format!("adding project {name}"))?;
    crate::events::append(
        conn,
        None,
        None,
        "project_added",
        &serde_json::json!({ "project_id": p.id, "name": name, "root": root_path }),
    )?;
    Ok(p)
}

fn row_to_project(r: &rusqlite::Row) -> rusqlite::Result<(Project, String)> {
    Ok((
        Project {
            id: r.get(0)?,
            name: r.get(1)?,
            root_path: r.get(2)?,
            kind: r.get(3)?,
            manifest: serde_json::Value::Null,
            policy: ProjectPolicy::default(),
            created_at: r.get(6)?,
        },
        r.get::<_, String>(5)?,
    ))
}

const PROJECT_COLS: &str = "id, name, root_path, kind, manifest_json, policy_json, created_at";

pub fn project_get(conn: &Connection, id_or_name: &str) -> Result<Project> {
    let row = conn
        .query_row(
            &format!("SELECT {PROJECT_COLS} FROM projects WHERE id = ?1 OR name = ?1"),
            [id_or_name],
            row_to_project,
        )
        .optional()?
        .with_context(|| format!("project not found: {id_or_name}"))?;
    let (mut p, policy_json) = row;
    p.policy = ProjectPolicy::parse(&policy_json)?;
    Ok(p)
}

pub fn project_list(conn: &Connection) -> Result<Vec<Project>> {
    let mut stmt =
        conn.prepare(&format!("SELECT {PROJECT_COLS} FROM projects ORDER BY created_at"))?;
    let rows: Vec<(Project, String)> = stmt
        .query_map([], row_to_project)?
        .collect::<std::result::Result<_, _>>()?;
    rows.into_iter()
        .map(|(mut p, pj)| {
            p.policy = ProjectPolicy::parse(&pj)?;
            Ok(p)
        })
        .collect()
}

pub fn project_set_policy(conn: &Connection, id: &str, policy: &ProjectPolicy) -> Result<()> {
    policy.validate()?;
    conn.execute(
        "UPDATE projects SET policy_json = ?1 WHERE id = ?2",
        params![serde_json::to_string(policy)?, id],
    )?;
    Ok(())
}

// ---------- tasks ----------

pub fn task_add(conn: &Connection, project_id: &str, title: &str, spec: &TaskSpec) -> Result<Task> {
    if spec.acceptance_criteria.is_empty() {
        anyhow::bail!("task needs at least one acceptance criterion");
    }
    if spec.verification_commands.is_empty() {
        anyhow::bail!("task needs at least one verification command");
    }
    if spec.risk_tier > 3 {
        anyhow::bail!("risk_tier must be 0..=3");
    }
    let now = crate::ids::now();
    let t = Task {
        id: crate::ids::new_id(),
        project_id: project_id.into(),
        title: title.into(),
        spec: spec.clone(),
        priority: 0,
        status: TaskStatus::Ready,
        lease_owner: None,
        lease_expires: None,
        retry_budget: 2,
        cancel_requested: false,
        git: None,
        route: None,
        created_at: now.clone(),
        updated_at: now.clone(),
    };
    conn.execute(
        "INSERT INTO tasks (id, project_id, title, spec_json, status, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'ready', ?5, ?5)",
        params![t.id, t.project_id, t.title, serde_json::to_string(spec)?, now],
    )?;
    crate::events::append(
        conn,
        Some(&t.id),
        None,
        "task_added",
        &serde_json::json!({ "title": title, "project_id": project_id }),
    )?;
    Ok(t)
}

const TASK_COLS: &str = "id, project_id, title, spec_json, priority, status, lease_owner, lease_expires, retry_budget, cancel_requested, git_json, route_json, created_at, updated_at";

fn row_to_task(r: &rusqlite::Row) -> rusqlite::Result<Task> {
    let spec_json: String = r.get(3)?;
    let status: String = r.get(5)?;
    let git_json: Option<String> = r.get(10)?;
    let route_json: Option<String> = r.get(11)?;
    Ok(Task {
        id: r.get(0)?,
        project_id: r.get(1)?,
        title: r.get(2)?,
        spec: serde_json::from_str(&spec_json).unwrap_or_else(|_| TaskSpec {
            goal: String::new(),
            rationale: String::new(),
            scope: vec![],
            non_scope: vec![],
            acceptance_criteria: vec![],
            verification_commands: vec![],
            risk_tier: 0,
            estimated_minutes: 0,
            checkpointable: false,
            allowed_agents: vec![],
            pinned_agent: None,
        }),
        priority: r.get(4)?,
        status: TaskStatus::parse(&status).unwrap_or(TaskStatus::Draft),
        lease_owner: r.get(6)?,
        lease_expires: r.get(7)?,
        retry_budget: r.get(8)?,
        cancel_requested: r.get::<_, i64>(9)? != 0,
        git: git_json.and_then(|s| serde_json::from_str(&s).ok()),
        route: route_json.and_then(|s| serde_json::from_str(&s).ok()),
        created_at: r.get(12)?,
        updated_at: r.get(13)?,
    })
}

pub fn task_get(conn: &Connection, id: &str) -> Result<Task> {
    conn.query_row(
        &format!("SELECT {TASK_COLS} FROM tasks WHERE id = ?1"),
        [id],
        row_to_task,
    )
    .optional()?
    .with_context(|| format!("task not found: {id}"))
}

pub fn task_list(conn: &Connection, project_id: Option<&str>) -> Result<Vec<Task>> {
    let (sql, args): (String, Vec<String>) = match project_id {
        Some(p) => (
            format!("SELECT {TASK_COLS} FROM tasks WHERE project_id = ?1 ORDER BY priority DESC, created_at"),
            vec![p.to_string()],
        ),
        None => (
            format!("SELECT {TASK_COLS} FROM tasks ORDER BY priority DESC, created_at"),
            vec![],
        ),
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(args), row_to_task)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn task_set_git(conn: &Connection, id: &str, git: &TaskGit) -> Result<()> {
    conn.execute(
        "UPDATE tasks SET git_json = ?1, updated_at = ?2 WHERE id = ?3",
        params![serde_json::to_string(git)?, crate::ids::now(), id],
    )?;
    Ok(())
}

pub fn task_set_route(conn: &Connection, id: &str, route: &serde_json::Value) -> Result<()> {
    conn.execute(
        "UPDATE tasks SET route_json = ?1, updated_at = ?2 WHERE id = ?3",
        params![serde_json::to_string(route)?, crate::ids::now(), id],
    )?;
    crate::events::append(conn, Some(id), None, "route", route)?;
    Ok(())
}

pub fn task_lease(conn: &Connection, id: &str, owner: &str, seconds: i64) -> Result<()> {
    let expires = (chrono::Utc::now() + chrono::Duration::seconds(seconds))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    conn.execute(
        "UPDATE tasks SET lease_owner = ?1, lease_expires = ?2 WHERE id = ?3",
        params![owner, expires, id],
    )?;
    Ok(())
}

pub fn task_heartbeat(conn: &Connection, id: &str, extend_seconds: i64) -> Result<()> {
    let expires = (chrono::Utc::now() + chrono::Duration::seconds(extend_seconds))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    conn.execute(
        "UPDATE tasks SET heartbeat_at = ?1, lease_expires = ?2 WHERE id = ?3",
        params![crate::ids::now(), expires, id],
    )?;
    Ok(())
}

pub fn task_request_cancel(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("UPDATE tasks SET cancel_requested = 1 WHERE id = ?1", [id])?;
    crate::events::append(conn, Some(id), None, "cancel_requested", &serde_json::json!({}))?;
    Ok(())
}

pub fn task_cancel_requested(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn.query_row("SELECT cancel_requested FROM tasks WHERE id = ?1", [id], |r| {
        r.get::<_, i64>(0)
    })? != 0)
}

pub fn task_decrement_retry(conn: &Connection, id: &str) -> Result<i64> {
    conn.execute(
        "UPDATE tasks SET retry_budget = retry_budget - 1 WHERE id = ?1 AND retry_budget > 0",
        [id],
    )?;
    Ok(conn.query_row("SELECT retry_budget FROM tasks WHERE id = ?1", [id], |r| r.get(0))?)
}

// ---------- dependencies ----------

pub fn dep_add(conn: &Connection, task_id: &str, depends_on: &str) -> Result<()> {
    if task_id == depends_on {
        anyhow::bail!("task cannot depend on itself");
    }
    // Cycle check: walk from depends_on; if we reach task_id, adding would cycle.
    let mut seen = HashSet::new();
    let mut stack = vec![depends_on.to_string()];
    while let Some(cur) = stack.pop() {
        if cur == task_id {
            anyhow::bail!("dependency cycle: {task_id} <-> {depends_on}");
        }
        if !seen.insert(cur.clone()) {
            continue;
        }
        let mut stmt = conn.prepare("SELECT depends_on FROM task_deps WHERE task_id = ?1")?;
        let next: Vec<String> = stmt
            .query_map([&cur], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        stack.extend(next);
    }
    conn.execute(
        "INSERT OR IGNORE INTO task_deps (task_id, depends_on) VALUES (?1, ?2)",
        params![task_id, depends_on],
    )?;
    Ok(())
}

/// Unmet dependencies = deps not in `completed`.
pub fn deps_unmet(conn: &Connection, task_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT d.depends_on FROM task_deps d
         JOIN tasks t ON t.id = d.depends_on
         WHERE d.task_id = ?1 AND t.status != 'completed'",
    )?;
    let rows = stmt
        .query_map([task_id], |r| r.get(0))?
        .collect::<std::result::Result<Vec<String>, _>>()?;
    Ok(rows)
}

// ---------- runs ----------

pub fn run_create(
    conn: &Connection,
    id: &str,
    task_id: &str,
    attempt: i64,
    mode: &str,
    backend: &str,
    evidence_dir: &str,
) -> Result<Run> {
    let r = Run {
        id: id.into(),
        task_id: task_id.into(),
        attempt,
        mode: mode.into(),
        backend: backend.into(),
        started_at: Some(crate::ids::now()),
        ended_at: None,
        exit_status: None,
        usage: None,
        evidence_dir: evidence_dir.into(),
    };
    conn.execute(
        "INSERT INTO runs (id, task_id, attempt, mode, backend, started_at, evidence_dir)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![r.id, r.task_id, r.attempt, r.mode, r.backend, r.started_at, r.evidence_dir],
    )?;
    Ok(r)
}

pub fn run_finish(
    conn: &Connection,
    run_id: &str,
    exit_status: &str,
    usage: Option<&serde_json::Value>,
) -> Result<()> {
    conn.execute(
        "UPDATE runs SET ended_at = ?1, exit_status = ?2, usage_json = ?3 WHERE id = ?4",
        params![
            crate::ids::now(),
            exit_status,
            usage.map(serde_json::to_string).transpose()?,
            run_id
        ],
    )?;
    Ok(())
}

pub fn run_list(conn: &Connection, task_id: &str) -> Result<Vec<Run>> {
    let mut stmt = conn.prepare(
        "SELECT id, task_id, attempt, mode, backend, started_at, ended_at, exit_status, usage_json, evidence_dir
         FROM runs WHERE task_id = ?1 ORDER BY started_at",
    )?;
    let rows = stmt
        .query_map([task_id], |r| {
            let usage: Option<String> = r.get(8)?;
            Ok(Run {
                id: r.get(0)?,
                task_id: r.get(1)?,
                attempt: r.get(2)?,
                mode: r.get(3)?,
                backend: r.get(4)?,
                started_at: r.get(5)?,
                ended_at: r.get(6)?,
                exit_status: r.get(7)?,
                usage: usage.and_then(|s| serde_json::from_str(&s).ok()),
                evidence_dir: r.get(9)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---------- approvals ----------

pub fn approval_create(
    conn: &Connection,
    task_id: Option<&str>,
    action: &serde_json::Value,
    ttl_minutes: i64,
) -> Result<Approval> {
    let a = Approval {
        id: crate::ids::new_id(),
        task_id: task_id.map(String::from),
        requested_at: crate::ids::now(),
        action: action.clone(),
        expires_at: (chrono::Utc::now() + chrono::Duration::minutes(ttl_minutes))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        status: "pending".into(),
        decided_at: None,
        decided_via: None,
    };
    conn.execute(
        "INSERT INTO approvals (id, task_id, requested_at, action_json, expires_at, status)
         VALUES (?1, ?2, ?3, ?4, ?5, 'pending')",
        params![a.id, a.task_id, a.requested_at, serde_json::to_string(action)?, a.expires_at],
    )?;
    crate::events::append(conn, task_id, None, "approval_requested", action)?;
    Ok(a)
}

pub fn approval_list_pending(conn: &Connection) -> Result<Vec<Approval>> {
    let mut stmt = conn.prepare(
        "SELECT id, task_id, requested_at, action_json, expires_at, status, decided_at, decided_via
         FROM approvals WHERE status = 'pending' ORDER BY requested_at",
    )?;
    let rows = stmt
        .query_map([], |r| {
            let action: String = r.get(3)?;
            Ok(Approval {
                id: r.get(0)?,
                task_id: r.get(1)?,
                requested_at: r.get(2)?,
                action: serde_json::from_str(&action).unwrap_or(serde_json::Value::Null),
                expires_at: r.get(4)?,
                status: r.get(5)?,
                decided_at: r.get(6)?,
                decided_via: r.get(7)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Decide an approval. Expired pending approvals cannot be approved.
pub fn approval_decide(conn: &Connection, id: &str, approve: bool, via: &str) -> Result<String> {
    let (status, expires_at): (String, String) = conn.query_row(
        "SELECT status, expires_at FROM approvals WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if status != "pending" {
        anyhow::bail!("approval {id} is {status}, not pending");
    }
    let now = crate::ids::now();
    let new_status = if approve && now > expires_at {
        "expired".to_string()
    } else if approve {
        "approved".to_string()
    } else {
        "denied".to_string()
    };
    conn.execute(
        "UPDATE approvals SET status = ?1, decided_at = ?2, decided_via = ?3 WHERE id = ?4",
        params![new_status, now, via, id],
    )?;
    let task_id: Option<String> =
        conn.query_row("SELECT task_id FROM approvals WHERE id = ?1", [id], |r| r.get(0))?;
    crate::events::append(
        conn,
        task_id.as_deref(),
        None,
        "approval_decided",
        &serde_json::json!({ "approval_id": id, "status": new_status, "via": via }),
    )?;
    Ok(new_status)
}

/// Latest decided approval status for a task, if any.
pub fn approval_status_for_task(conn: &Connection, task_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT status FROM approvals WHERE task_id = ?1 ORDER BY requested_at DESC LIMIT 1",
            [task_id],
            |r| r.get(0),
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::ProjectPolicy;

    fn conn() -> Connection {
        let dir = tempfile::tempdir().unwrap();
        let c = crate::db::open(&dir.path().join("s.db")).unwrap();
        std::mem::forget(dir); // keep tempdir alive for test duration (leaked; fine in tests)
        c
    }

    fn spec() -> TaskSpec {
        TaskSpec {
            goal: "g".into(),
            rationale: String::new(),
            scope: vec![],
            non_scope: vec![],
            acceptance_criteria: vec!["works".into()],
            verification_commands: vec![vec!["true".into()]],
            risk_tier: 1,
            estimated_minutes: 5,
            checkpointable: false,
            allowed_agents: vec![],
            pinned_agent: None,
        }
    }

    #[test]
    fn project_task_roundtrip() {
        let c = conn();
        let p = project_add(&c, "demo", "/tmp/demo", "normal", &ProjectPolicy::default()).unwrap();
        let t = task_add(&c, &p.id, "t1", &spec()).unwrap();
        let got = task_get(&c, &t.id).unwrap();
        assert_eq!(got.title, "t1");
        assert_eq!(got.status, TaskStatus::Ready);
        assert_eq!(project_get(&c, "demo").unwrap().id, p.id);
    }

    #[test]
    fn dependency_cycle_rejected() {
        let c = conn();
        let p = project_add(&c, "demo", "/tmp/demo", "normal", &ProjectPolicy::default()).unwrap();
        let a = task_add(&c, &p.id, "a", &spec()).unwrap();
        let b = task_add(&c, &p.id, "b", &spec()).unwrap();
        let d = task_add(&c, &p.id, "d", &spec()).unwrap();
        dep_add(&c, &b.id, &a.id).unwrap();
        dep_add(&c, &d.id, &b.id).unwrap();
        assert!(dep_add(&c, &a.id, &d.id).is_err()); // a -> d -> b -> a
        assert_eq!(deps_unmet(&c, &b.id).unwrap(), vec![a.id.clone()]);
    }

    #[test]
    fn approval_expiry_cannot_be_approved() {
        let c = conn();
        let a = approval_create(&c, None, &serde_json::json!({"action":"x"}), -1).unwrap();
        assert_eq!(approval_decide(&c, &a.id, true, "cli").unwrap(), "expired");
    }

    #[test]
    fn task_requires_criteria_and_verification() {
        let c = conn();
        let p = project_add(&c, "demo", "/tmp/demo", "normal", &ProjectPolicy::default()).unwrap();
        let mut s = spec();
        s.acceptance_criteria.clear();
        assert!(task_add(&c, &p.id, "bad", &s).is_err());
        let mut s2 = spec();
        s2.verification_commands.clear();
        assert!(task_add(&c, &p.id, "bad2", &s2).is_err());
    }
}
