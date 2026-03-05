use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use tracing::error;

use crate::{
    engine::Orchestrator,
    types::{ApproveRequest, RunRequest},
};

#[derive(Clone)]
pub struct HttpState {
    pub orchestrator: Arc<Orchestrator>,
}

pub fn router(state: HttpState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(metrics))
        .route("/ui", get(ui_dashboard))
        .route("/v1/runs", get(list_runs).post(create_run))
        .route("/v1/runs/:run_id", get(get_run))
        .route("/v1/runs/:run_id/events", get(list_run_events))
        .route("/v1/runs/:run_id/replay", post(replay_run))
        .route("/v1/runs/:run_id/audit/verify", get(verify_audit))
        .route("/v1/approvals/pending", get(list_pending_approvals))
        .route("/v1/approve", post(approve))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    limit: Option<usize>,
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({ "status": "ok" })))
}

async fn metrics(State(state): State<HttpState>) -> impl IntoResponse {
    match state.orchestrator.metrics.render() {
        Ok(payload) => (StatusCode::OK, payload).into_response(),
        Err(err) => {
            error!(error = %err, "failed to render metrics");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "metrics unavailable".to_string(),
            )
                .into_response()
        }
    }
}

async fn create_run(
    State(state): State<HttpState>,
    Json(req): Json<RunRequest>,
) -> impl IntoResponse {
    match state.orchestrator.run_new(req).await {
        Ok(outcome) => (StatusCode::ACCEPTED, Json(outcome)).into_response(),
        Err(err) => {
            error!(error = %err, "run creation failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": err.to_string() })),
            )
                .into_response()
        }
    }
}

async fn list_runs(
    Query(query): Query<ListQuery>,
    State(state): State<HttpState>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(25).min(200);
    match state.orchestrator.list_runs(limit) {
        Ok(runs) => (StatusCode::OK, Json(json!({ "runs": runs }))).into_response(),
        Err(err) => {
            error!(error = %err, "failed to list runs");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": err.to_string() })),
            )
                .into_response()
        }
    }
}

async fn replay_run(
    Path(run_id): Path<String>,
    State(state): State<HttpState>,
) -> impl IntoResponse {
    match state.orchestrator.replay(&run_id).await {
        Ok(outcome) => (StatusCode::OK, Json(outcome)).into_response(),
        Err(err) => {
            error!(run_id = %run_id, error = %err, "replay failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": err.to_string() })),
            )
                .into_response()
        }
    }
}

async fn get_run(Path(run_id): Path<String>, State(state): State<HttpState>) -> impl IntoResponse {
    match state.orchestrator.get_run(&run_id) {
        Ok(Some(run)) => (StatusCode::OK, Json(run)).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "run not found" })),
        )
            .into_response(),
        Err(err) => {
            error!(run_id = %run_id, error = %err, "failed to load run");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": err.to_string() })),
            )
                .into_response()
        }
    }
}

async fn list_run_events(
    Path(run_id): Path<String>,
    State(state): State<HttpState>,
) -> impl IntoResponse {
    match state.orchestrator.list_events(&run_id) {
        Ok(events) => (
            StatusCode::OK,
            Json(json!({ "run_id": run_id, "events": events })),
        )
            .into_response(),
        Err(err) => {
            error!(run_id = %run_id, error = %err, "failed to list run events");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": err.to_string() })),
            )
                .into_response()
        }
    }
}

async fn approve(
    State(state): State<HttpState>,
    Json(req): Json<ApproveRequest>,
) -> impl IntoResponse {
    let reason = req.reason.clone().unwrap_or_default();
    if reason.trim().is_empty() {
        state
            .orchestrator
            .metrics
            .approval_actions
            .with_label_values(&["approve", "invalid_reason"])
            .inc();
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "approval reason required" })),
        )
            .into_response();
    }

    match state
        .orchestrator
        .approve(&req.run_id, &req.step_id, &req.actor, &reason)
    {
        Ok(true) => {
            state
                .orchestrator
                .metrics
                .approval_actions
                .with_label_values(&["approve", "granted"])
                .inc();
            (
                StatusCode::OK,
                Json(json!({
                    "status": "approved",
                    "run_id": req.run_id,
                    "step_id": req.step_id,
                    "actor": req.actor,
                    "reason": reason
                })),
            )
                .into_response()
        }
        Ok(false) => {
            state
                .orchestrator
                .metrics
                .approval_actions
                .with_label_values(&["approve", "not_found"])
                .inc();
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "pending approval not found" })),
            )
                .into_response()
        }
        Err(err) => {
            error!(error = %err, "approval failed");
            if err.to_string().contains("approval reason required") {
                state
                    .orchestrator
                    .metrics
                    .approval_actions
                    .with_label_values(&["approve", "invalid_reason"])
                    .inc();
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": err.to_string() })),
                )
                    .into_response();
            }
            state
                .orchestrator
                .metrics
                .approval_actions
                .with_label_values(&["approve", "error"])
                .inc();
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": err.to_string() })),
            )
                .into_response()
        }
    }
}

async fn verify_audit(
    Path(run_id): Path<String>,
    State(state): State<HttpState>,
) -> impl IntoResponse {
    match state.orchestrator.verify_audit_chain(&run_id) {
        Ok(valid) => (
            StatusCode::OK,
            Json(json!({ "run_id": run_id, "audit_chain_valid": valid })),
        )
            .into_response(),
        Err(err) => {
            error!(run_id = %run_id, error = %err, "audit verification failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": err.to_string() })),
            )
                .into_response()
        }
    }
}

async fn list_pending_approvals(
    Query(query): Query<ListQuery>,
    State(state): State<HttpState>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(25).min(200);
    match state.orchestrator.list_pending_approvals(limit) {
        Ok(approvals) => (StatusCode::OK, Json(json!({ "approvals": approvals }))).into_response(),
        Err(err) => {
            error!(error = %err, "failed to list pending approvals");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": err.to_string() })),
            )
                .into_response()
        }
    }
}

async fn ui_dashboard() -> impl IntoResponse {
    Html(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width,initial-scale=1" />
  <title>Aether Ops</title>
  <style>
    :root { --bg:#0e1016; --card:#161a24; --muted:#90a0b6; --text:#e7edf6; --ok:#33d17a; --warn:#e5a50a; --err:#ff6b6b; --accent:#5bc0eb; }
    body { margin:0; font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; background: radial-gradient(circle at top, #1b2333 0%, var(--bg) 58%); color:var(--text); }
    header { padding:16px 20px; border-bottom:1px solid #2a3347; display:flex; justify-content:space-between; align-items:center; }
    .grid { display:grid; gap:12px; padding:14px; grid-template-columns:repeat(auto-fit,minmax(320px,1fr)); }
    .card { background:linear-gradient(170deg,#161a24,#111521); border:1px solid #293249; border-radius:10px; padding:10px; }
    h1,h2 { margin:0; font-size:15px; }
    table { width:100%; border-collapse:collapse; margin-top:8px; font-size:12px; }
    th,td { text-align:left; padding:6px; border-bottom:1px solid #263045; vertical-align:top; }
    .muted { color:var(--muted); }
    code { color:var(--accent); }
    .ok { color:var(--ok); } .warn { color:var(--warn); } .err { color:var(--err); }
    button,input { font: inherit; }
    .row { display:flex; gap:8px; align-items:center; margin-top:8px; }
    input { background:#0d1220; border:1px solid #2d3952; color:var(--text); padding:6px; border-radius:6px; }
    button { background:#1f6feb; color:white; border:none; border-radius:6px; padding:7px 10px; cursor:pointer; }
    pre { max-height:220px; overflow:auto; background:#0b0f19; padding:8px; border-radius:8px; border:1px solid #28324a; font-size:11px; }
  </style>
</head>
<body>
  <header>
    <h1>Aether Ops Dashboard</h1>
    <div class="muted">Auto-refresh: 10s</div>
  </header>
  <section class="grid">
    <article class="card">
      <h2>Recent Runs</h2>
      <table id="runs"><thead><tr><th>Run</th><th>Status</th><th>Workflow</th><th>Cost</th><th>Tokens</th></tr></thead><tbody></tbody></table>
    </article>
    <article class="card">
      <h2>Pending Approvals</h2>
      <table id="approvals"><thead><tr><th>Run</th><th>Step</th><th>Action</th><th>Created</th></tr></thead><tbody></tbody></table>
    </article>
    <article class="card">
      <h2>Run Inspector</h2>
      <div class="row">
        <input id="runId" placeholder="run-id" size="36"/>
        <button id="loadRun">Load</button>
      </div>
      <div class="row"><span class="muted">Audit chain:</span><code id="audit">unknown</code></div>
      <pre id="events">[]</pre>
    </article>
  </section>
  <script>
    async function getJson(path){ const r = await fetch(path); if(!r.ok) throw new Error(await r.text()); return r.json(); }
    function statusClass(status){ if(status==='succeeded') return 'ok'; if(status==='waiting_approval'||status==='budget_exceeded') return 'warn'; if(status==='failed'||status==='killed') return 'err'; return ''; }

    async function refreshRuns(){
      const { runs } = await getJson('/v1/runs?limit=20');
      const tbody = document.querySelector('#runs tbody');
      tbody.innerHTML = '';
      for(const run of runs){
        const tr = document.createElement('tr');
        tr.innerHTML = `<td><code>${run.run_id.slice(0,8)}</code></td><td class=\"${statusClass(run.status)}\">${run.status}</td><td>${run.workflow}</td><td>$${Number(run.total_cost_usd).toFixed(4)}</td><td>${run.total_tokens}</td>`;
        tr.onclick = () => { document.getElementById('runId').value = run.run_id; loadInspector(); };
        tbody.appendChild(tr);
      }
    }

    async function refreshApprovals(){
      const { approvals } = await getJson('/v1/approvals/pending?limit=20');
      const tbody = document.querySelector('#approvals tbody');
      tbody.innerHTML = '';
      for(const a of approvals){
        const tr = document.createElement('tr');
        tr.innerHTML = `<td><code>${a.run_id.slice(0,8)}</code></td><td>${a.step_id}</td><td>${a.action}</td><td>${a.created_at}</td>`;
        tbody.appendChild(tr);
      }
    }

    async function loadInspector(){
      const runId = document.getElementById('runId').value.trim();
      if(!runId) return;
      const [eventsPayload, auditPayload] = await Promise.all([
        getJson(`/v1/runs/${runId}/events`),
        getJson(`/v1/runs/${runId}/audit/verify`)
      ]);
      document.getElementById('events').textContent = JSON.stringify(eventsPayload.events, null, 2);
      document.getElementById('audit').textContent = String(auditPayload.audit_chain_valid);
    }

    async function refreshAll(){ try { await Promise.all([refreshRuns(), refreshApprovals()]); } catch (e) { console.error(e); } }
    document.getElementById('loadRun').onclick = () => loadInspector();
    refreshAll();
    setInterval(refreshAll, 10000);
  </script>
</body>
</html>"#,
    )
}
