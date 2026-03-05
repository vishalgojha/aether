use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct RunRequest {
    pub workflow: String,
    #[serde(default)]
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApproveRequest {
    pub run_id: String,
    pub step_id: String,
    pub actor: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunOutcome {
    pub run_id: String,
    pub status: RunStatus,
    pub tokens_used: u64,
    pub estimated_cost_usd: f64,
    pub completed_steps: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Succeeded,
    Failed,
    WaitingApproval,
    BudgetExceeded,
    Killed,
}

#[derive(Debug, Clone)]
pub struct StepDecision {
    pub step_id: String,
    pub action: String,
    pub confidence: f32,
    pub risk_score: f32,
    pub estimated_tokens: u32,
    pub estimated_cost_usd: f64,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionPath {
    Supervisor,
    DebateFallback,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedRun {
    pub run_id: String,
    pub workflow: String,
    pub variant_id: Option<String>,
    pub status: RunStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub total_tokens: u64,
    pub total_cost_usd: f64,
    pub step_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecord {
    pub seq: i64,
    pub run_id: String,
    pub event_type: String,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub prev_hash: Option<String>,
    pub event_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingApproval {
    pub run_id: String,
    pub step_id: String,
    pub action: String,
    pub threshold_usd: f64,
    pub status: String,
    pub created_at: DateTime<Utc>,
}
