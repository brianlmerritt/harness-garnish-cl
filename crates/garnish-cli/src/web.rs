//! First web UX (ADR-0007): a thin loopback-only axum app over the same
//! store/state functions the CLI uses — one policy path. Reads: projects,
//! tasks, runs, events, quota, costs. Writes: exactly approval decisions and
//! pause/resume/cancel/retry. Bearer-token auth on every /api call; the
//! token lives in <data_dir>/web-token (0600). No remote exposure — users
//! who want that tunnel via SSH/Tailscale.

use anyhow::{Context, Result};
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Json},
    routing::{get, post},
    Router,
};
use garnish_core::{paths, state, store, TaskStatus};
use std::collections::HashMap;
use std::sync::Arc;

const INDEX_HTML: &str = include_str!("web/index.html");

#[derive(Clone)]
struct WebState {
    token: Arc<String>,
}

fn load_or_create_token() -> Result<String> {
    let path = paths::data_dir().join("web-token");
    if let Ok(t) = std::fs::read_to_string(&path) {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Ok(t);
        }
    }
    // 32 bytes of OS randomness, hex-encoded, file mode 0600.
    let mut buf = [0u8; 32];
    getrandom(&mut buf)?;
    let token: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::create_dir_all(paths::data_dir())?;
    std::fs::write(&path, &token)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(token)
}

fn getrandom(buf: &mut [u8]) -> Result<()> {
    let mut f = std::fs::File::open("/dev/urandom").context("opening /dev/urandom")?;
    std::io::Read::read_exact(&mut f, buf)?;
    Ok(())
}

fn open_db() -> Result<rusqlite::Connection> {
    garnish_core::db::open(&paths::db_path())
}

fn authed(st: &WebState, headers: &HeaderMap, query: &HashMap<String, String>) -> bool {
    let presented = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string)
        .or_else(|| query.get("token").cloned());
    presented.as_deref() == Some(st.token.as_str())
}

type ApiResult = std::result::Result<Json<serde_json::Value>, (StatusCode, String)>;

fn err500(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

fn unauthorized() -> (StatusCode, String) {
    (StatusCode::UNAUTHORIZED, "missing or invalid token".into())
}

async fn index() -> impl IntoResponse {
    Html(INDEX_HTML)
}

async fn overview(
    State(st): State<WebState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    if !authed(&st, &headers, &q) {
        return Err(unauthorized());
    }
    let conn = open_db().map_err(err500)?;
    let projects = store::project_list(&conn).map_err(err500)?;
    let tasks = store::task_list(&conn, None).map_err(err500)?;
    let approvals = store::approval_list_pending(&conn).map_err(err500)?;
    let quota = store::quota_snapshots_recent(&conn, 12).map_err(err500)?;
    let costs = store::cost_summary(&conn, None).map_err(err500)?;
    Ok(Json(serde_json::json!({
        "projects": projects,
        "tasks": tasks,
        "approvals": approvals,
        "quota": quota,
        "costs": costs,
    })))
}

async fn task_detail(
    State(st): State<WebState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    Path(id): Path<String>,
) -> ApiResult {
    if !authed(&st, &headers, &q) {
        return Err(unauthorized());
    }
    let conn = open_db().map_err(err500)?;
    let task = store::task_get(&conn, &id).map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    let runs = store::run_list(&conn, &id).map_err(err500)?;
    let events = store::events_for_task(&conn, &id, 50).map_err(err500)?;
    Ok(Json(serde_json::json!({ "task": task, "runs": runs, "events": events })))
}

/// Task actions — the same operations, through the same functions, as the
/// CLI. Anything else (starting runs, editing policy) is deliberately absent.
async fn task_action(
    State(st): State<WebState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    Path((id, action)): Path<(String, String)>,
) -> ApiResult {
    if !authed(&st, &headers, &q) {
        return Err(unauthorized());
    }
    let conn = open_db().map_err(err500)?;
    let task = store::task_get(&conn, &id).map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    let outcome = match action.as_str() {
        "pause" => {
            if task.status == TaskStatus::Running {
                store::task_request_pause(&conn, &id).map_err(err500)?;
                "pause requested; task stops at the next safe point"
            } else {
                state::transition(&conn, &id, task.status, TaskStatus::Paused, "web pause")
                    .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;
                "paused"
            }
        }
        "resume" | "retry" => {
            state::transition(&conn, &id, task.status, TaskStatus::Ready, "web resume/retry")
                .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;
            "ready"
        }
        "cancel" => {
            if task.status == TaskStatus::Running {
                store::task_request_cancel(&conn, &id).map_err(err500)?;
                "cancellation requested"
            } else {
                state::transition(&conn, &id, task.status, TaskStatus::Cancelled, "web cancel")
                    .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;
                "cancelled"
            }
        }
        other => return Err((StatusCode::BAD_REQUEST, format!("unknown action {other}"))),
    };
    Ok(Json(serde_json::json!({ "task": id, "result": outcome })))
}

async fn approval_action(
    State(st): State<WebState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    Path((id, decision)): Path<(String, String)>,
) -> ApiResult {
    if !authed(&st, &headers, &q) {
        return Err(unauthorized());
    }
    let approve = match decision.as_str() {
        "approve" => true,
        "deny" => false,
        other => return Err((StatusCode::BAD_REQUEST, format!("unknown decision {other}"))),
    };
    let conn = open_db().map_err(err500)?;
    let status = store::approval_decide(&conn, &id, approve, "web")
        .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;
    Ok(Json(serde_json::json!({ "approval": id, "status": status })))
}

pub async fn serve(port: u16) -> Result<()> {
    let token = load_or_create_token()?;
    let st = WebState { token: Arc::new(token.clone()) };
    let app = Router::new()
        .route("/", get(index))
        .route("/api/overview", get(overview))
        .route("/api/task/:id", get(task_detail))
        .route("/api/task/:id/:action", post(task_action))
        .route("/api/approval/:id/:decision", post(approval_action))
        .with_state(st);

    // Loopback only — never 0.0.0.0. Remote access is the user's tunnel.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .with_context(|| format!("binding 127.0.0.1:{port}"))?;
    let addr = listener.local_addr()?;
    println!("garnish web: http://{addr}/?token={token}");
    println!("(loopback only; token stored in {})", paths::data_dir().join("web-token").display());
    axum::serve(listener, app).await?;
    Ok(())
}
