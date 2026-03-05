use std::time::{Duration, Instant};

use chrono::Utc;
use serde_json::json;
use thiserror::Error;
use tokio::time::sleep;
use tracing::{error, info, instrument, warn};
use uuid::Uuid;

use crate::{
    config::AppConfig,
    metrics::AppMetrics,
    state::StateStore,
    types::{
        DecisionPath, EventRecord, PendingApproval, PersistedRun, RunOutcome, RunRequest,
        RunStatus, StepDecision,
    },
};

#[derive(Clone)]
pub struct Orchestrator {
    pub config: AppConfig,
    pub state: StateStore,
    pub metrics: AppMetrics,
}

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("rate limited")]
    RateLimited,
    #[error("tool timeout")]
    Timeout,
    #[error("fatal tool error: {0}")]
    Fatal(String),
}

impl Orchestrator {
    pub fn new(config: AppConfig, state: StateStore, metrics: AppMetrics) -> Self {
        Self {
            config,
            state,
            metrics,
        }
    }

    #[instrument(skip(self, req), fields(workflow = %req.workflow))]
    pub async fn run_new(&self, req: RunRequest) -> anyhow::Result<RunOutcome> {
        let started = Instant::now();
        let run_id = Uuid::new_v4().to_string();
        let workflow = req.workflow.clone();
        let variant_id = extract_variant_id(&req.input);
        self.state
            .create_run_with_variant(&run_id, &workflow, variant_id.as_deref())?;
        self.state.append_event(
            &run_id,
            "run_created",
            &json!({ "workflow": req.workflow, "variant_id": variant_id, "input": req.input }),
        )?;
        let result = self.execute(run_id, workflow, 0, 0, 0.0).await;
        self.observe_run_duration("run_new", &result, started.elapsed().as_secs_f64());
        if let Ok(outcome) = &result {
            info!(
                run_id = %outcome.run_id,
                status = ?outcome.status,
                completed_steps = outcome.completed_steps,
                "run completed"
            );
        }
        result
    }

    #[instrument(skip(self), fields(run_id = %run_id))]
    pub async fn replay(&self, run_id: &str) -> anyhow::Result<RunOutcome> {
        let started = Instant::now();
        let run = self
            .state
            .get_run(run_id)?
            .ok_or_else(|| anyhow::anyhow!("run not found"))?;
        self.state.append_event(
            run_id,
            "replay_requested",
            &json!({ "at": Utc::now().to_rfc3339(), "current_status": run.status.clone() }),
        )?;
        let result = self
            .execute(
                run_id.to_string(),
                run.workflow,
                run.step_count,
                run.total_tokens,
                run.total_cost_usd,
            )
            .await;
        self.observe_run_duration("replay", &result, started.elapsed().as_secs_f64());
        if let Ok(outcome) = &result {
            info!(
                run_id = %outcome.run_id,
                status = ?outcome.status,
                completed_steps = outcome.completed_steps,
                "replay completed"
            );
        }
        result
    }

    #[instrument(skip(self, reason), fields(run_id = %run_id, step_id = %step_id, actor = %actor))]
    pub fn approve(
        &self,
        run_id: &str,
        step_id: &str,
        actor: &str,
        reason: &str,
    ) -> anyhow::Result<bool> {
        let reason = reason.trim();
        if reason.is_empty() {
            anyhow::bail!("approval reason required");
        }

        let approved = self.state.approve(run_id, step_id, actor, reason)?;
        if approved {
            self.metrics.pending_approvals.dec();
            self.metrics.approvals_granted.inc();
            self.state.append_event(
                run_id,
                "approval_granted",
                &json!({ "step_id": step_id, "actor": actor, "reason": reason }),
            )?;
            info!(
                run_id = %run_id,
                step_id = %step_id,
                actor = %actor,
                reason = %reason,
                "approval granted with rationale"
            );
        } else {
            info!(
                run_id = %run_id,
                step_id = %step_id,
                actor = %actor,
                "approval request not found"
            );
        }
        Ok(approved)
    }

    pub fn get_run(&self, run_id: &str) -> anyhow::Result<Option<PersistedRun>> {
        self.state.get_run(run_id)
    }

    pub fn list_runs(&self, limit: usize) -> anyhow::Result<Vec<PersistedRun>> {
        self.state.list_runs(limit)
    }

    pub fn list_events(&self, run_id: &str) -> anyhow::Result<Vec<EventRecord>> {
        self.state.list_events(run_id)
    }

    pub fn list_pending_approvals(&self, limit: usize) -> anyhow::Result<Vec<PendingApproval>> {
        self.state.list_pending_approvals(limit)
    }

    pub fn verify_audit_chain(&self, run_id: &str) -> anyhow::Result<bool> {
        self.state.verify_chain(run_id)
    }

    async fn execute(
        &self,
        run_id: String,
        workflow: String,
        starting_step: u32,
        mut total_tokens: u64,
        mut total_cost_usd: f64,
    ) -> anyhow::Result<RunOutcome> {
        if self.config.kill_switch_active() {
            self.finish(
                &run_id,
                RunStatus::Killed,
                total_tokens,
                total_cost_usd,
                starting_step,
                "killed",
            )?;
            return Ok(RunOutcome {
                run_id,
                status: RunStatus::Killed,
                tokens_used: total_tokens,
                estimated_cost_usd: total_cost_usd,
                completed_steps: starting_step,
            });
        }

        if self.state.tokens_used_today()? >= self.config.per_day_token_cap {
            self.finish(
                &run_id,
                RunStatus::BudgetExceeded,
                total_tokens,
                total_cost_usd,
                starting_step,
                "budget_exceeded",
            )?;
            return Ok(RunOutcome {
                run_id,
                status: RunStatus::BudgetExceeded,
                tokens_used: total_tokens,
                estimated_cost_usd: total_cost_usd,
                completed_steps: starting_step,
            });
        }

        self.metrics.runs_started.inc();
        self.state.update_run(
            &run_id,
            RunStatus::Running,
            total_tokens,
            total_cost_usd,
            starting_step,
        )?;

        for step in starting_step..self.config.max_steps {
            if self.config.kill_switch_active() {
                self.finish(
                    &run_id,
                    RunStatus::Killed,
                    total_tokens,
                    total_cost_usd,
                    step,
                    "killed",
                )?;
                return Ok(RunOutcome {
                    run_id,
                    status: RunStatus::Killed,
                    tokens_used: total_tokens,
                    estimated_cost_usd: total_cost_usd,
                    completed_steps: step,
                });
            }

            let started = std::time::Instant::now();
            let mut decision_path = DecisionPath::Supervisor;
            let mut decision = self.supervisor_decide(&workflow, step)?;
            if decision.risk_score >= self.config.high_risk_score || decision.confidence < 0.6 {
                decision = self.debate_fallback(decision, step);
                decision_path = DecisionPath::DebateFallback;
            }

            let path_label = match decision_path {
                DecisionPath::Supervisor => "supervisor",
                DecisionPath::DebateFallback => "debate",
            };
            self.metrics
                .decision_path
                .with_label_values(&[path_label])
                .inc();

            if total_tokens + decision.estimated_tokens as u64 > self.config.per_run_token_cap {
                self.state.append_event(
                    &run_id,
                    "budget_exceeded",
                    &json!({
                        "step": step,
                        "per_run_token_cap": self.config.per_run_token_cap,
                        "attempted_additional_tokens": decision.estimated_tokens
                    }),
                )?;
                self.finish(
                    &run_id,
                    RunStatus::BudgetExceeded,
                    total_tokens,
                    total_cost_usd,
                    step,
                    "budget_exceeded",
                )?;
                return Ok(RunOutcome {
                    run_id,
                    status: RunStatus::BudgetExceeded,
                    tokens_used: total_tokens,
                    estimated_cost_usd: total_cost_usd,
                    completed_steps: step,
                });
            }

            if needs_human_approval(&decision, self.config.approval_ad_spend_usd) {
                self.metrics.pending_approvals.inc();
                self.metrics.approvals_requested.inc();
                self.state.create_approval_request(
                    &run_id,
                    &decision.step_id,
                    &decision.action,
                    self.config.approval_ad_spend_usd,
                )?;
                self.state.append_event(
                    &run_id,
                    "approval_required",
                    &json!({
                        "step": step,
                        "step_id": decision.step_id,
                        "action": decision.action,
                        "payload": decision.payload
                    }),
                )?;
                self.finish(
                    &run_id,
                    RunStatus::WaitingApproval,
                    total_tokens,
                    total_cost_usd,
                    step,
                    "waiting_approval",
                )?;
                return Ok(RunOutcome {
                    run_id,
                    status: RunStatus::WaitingApproval,
                    tokens_used: total_tokens,
                    estimated_cost_usd: total_cost_usd,
                    completed_steps: step,
                });
            }

            if decision.action == "execute_approved_action"
                && !self.state.is_approved(&run_id, &decision.step_id)?
            {
                warn!(
                    run_id = %run_id,
                    step,
                    step_id = %decision.step_id,
                    "action blocked pending approval"
                );
                self.finish(
                    &run_id,
                    RunStatus::WaitingApproval,
                    total_tokens,
                    total_cost_usd,
                    step,
                    "waiting_approval",
                )?;
                return Ok(RunOutcome {
                    run_id,
                    status: RunStatus::WaitingApproval,
                    tokens_used: total_tokens,
                    estimated_cost_usd: total_cost_usd,
                    completed_steps: step,
                });
            }

            match self.execute_with_retry(&run_id, step, &decision).await {
                Ok(()) => {
                    total_tokens += decision.estimated_tokens as u64;
                    total_cost_usd += decision.estimated_cost_usd;
                    self.metrics
                        .tokens_used
                        .inc_by(decision.estimated_tokens as u64);
                    self.metrics
                        .cost_microusd
                        .inc_by((decision.estimated_cost_usd * 1_000_000.0).round() as u64);

                    let latency = started.elapsed().as_secs_f64();
                    self.metrics
                        .step_latency_seconds
                        .with_label_values(&[decision.action.as_str(), "ok"])
                        .observe(latency);
                    self.state.append_event(
                        &run_id,
                        "step_succeeded",
                        &json!({
                            "step": step,
                            "decision_path": path_label,
                            "action": decision.action,
                            "tokens": decision.estimated_tokens,
                            "cost_usd": decision.estimated_cost_usd,
                            "latency_s": latency,
                        }),
                    )?;
                    self.state.update_run(
                        &run_id,
                        RunStatus::Running,
                        total_tokens,
                        total_cost_usd,
                        step + 1,
                    )?;

                    if decision.action == "finish_workflow" {
                        self.finish(
                            &run_id,
                            RunStatus::Succeeded,
                            total_tokens,
                            total_cost_usd,
                            step + 1,
                            "succeeded",
                        )?;
                        return Ok(RunOutcome {
                            run_id,
                            status: RunStatus::Succeeded,
                            tokens_used: total_tokens,
                            estimated_cost_usd: total_cost_usd,
                            completed_steps: step + 1,
                        });
                    }
                }
                Err(err) => {
                    let latency = started.elapsed().as_secs_f64();
                    self.metrics
                        .step_latency_seconds
                        .with_label_values(&[decision.action.as_str(), "error"])
                        .observe(latency);
                    self.metrics
                        .step_failures
                        .with_label_values(&["tool_error"])
                        .inc();
                    self.state.append_event(
                        &run_id,
                        "step_failed",
                        &json!({
                            "step": step,
                            "action": decision.action,
                            "error": err.to_string(),
                            "latency_s": latency
                        }),
                    )?;
                    self.finish(
                        &run_id,
                        RunStatus::Failed,
                        total_tokens,
                        total_cost_usd,
                        step,
                        "failed",
                    )?;
                    return Ok(RunOutcome {
                        run_id,
                        status: RunStatus::Failed,
                        tokens_used: total_tokens,
                        estimated_cost_usd: total_cost_usd,
                        completed_steps: step,
                    });
                }
            }
        }

        self.finish(
            &run_id,
            RunStatus::Failed,
            total_tokens,
            total_cost_usd,
            self.config.max_steps,
            "failed",
        )?;
        Ok(RunOutcome {
            run_id,
            status: RunStatus::Failed,
            tokens_used: total_tokens,
            estimated_cost_usd: total_cost_usd,
            completed_steps: self.config.max_steps,
        })
    }

    fn finish(
        &self,
        run_id: &str,
        status: RunStatus,
        total_tokens: u64,
        total_cost_usd: f64,
        step_count: u32,
        status_label: &str,
    ) -> anyhow::Result<()> {
        self.state.update_run(
            run_id,
            status.clone(),
            total_tokens,
            total_cost_usd,
            step_count,
        )?;
        self.state.append_event(
            run_id,
            "run_finished",
            &json!({
                "status": status,
                "tokens_used": total_tokens,
                "estimated_cost_usd": total_cost_usd,
                "completed_steps": step_count
            }),
        )?;
        self.metrics
            .runs_finished
            .with_label_values(&[status_label])
            .inc();
        if let Err(err) = self.state.append_variant_observation(run_id) {
            warn!(
                run_id = %run_id,
                error = %err,
                "failed to append variant observation"
            );
        }
        Ok(())
    }

    fn observe_run_duration(
        &self,
        operation: &str,
        result: &anyhow::Result<RunOutcome>,
        elapsed_seconds: f64,
    ) {
        let status = match result {
            Ok(outcome) => run_status_label(&outcome.status),
            Err(_) => "error",
        };
        self.metrics
            .run_duration_seconds
            .with_label_values(&[operation, status])
            .observe(elapsed_seconds);
    }

    fn supervisor_decide(&self, workflow: &str, step: u32) -> anyhow::Result<StepDecision> {
        let action = match (workflow, step) {
            ("growth", 0) => "socialflow_fetch_accounts",
            ("growth", 1) => "socialflow_generate_campaign_plan",
            ("growth", 2) => "socialflow_launch_campaign",
            (_, s) if s > 4 => "finish_workflow",
            _ => "generic_tool_call",
        };

        let payload = if action == "socialflow_launch_campaign" {
            json!({
                "spend_usd": 500.0,
                "platform": "meta",
                "objective": "lead_generation"
            })
        } else {
            json!({ "workflow": workflow, "step": step })
        };

        Ok(StepDecision {
            step_id: format!("step-{step}"),
            action: action.to_string(),
            confidence: if action == "generic_tool_call" {
                0.55
            } else {
                0.82
            },
            risk_score: if action == "socialflow_launch_campaign" {
                0.81
            } else {
                0.32
            },
            estimated_tokens: 1_100 + (step * 120),
            estimated_cost_usd: 0.02 + (step as f64 * 0.01),
            payload,
        })
    }

    fn debate_fallback(&self, mut decision: StepDecision, step: u32) -> StepDecision {
        let original_payload = decision.payload.clone();
        decision.confidence = 0.78;
        decision.risk_score = 0.49;
        decision.payload = json!({
            "strategy": "debate_consensus",
            "step": step,
            "proposed_action": decision.action,
            "original_payload": original_payload,
            "constraints": ["budget_cap", "approval_gate", "risk_limit"]
        });
        decision
    }

    async fn execute_with_retry(
        &self,
        run_id: &str,
        step: u32,
        decision: &StepDecision,
    ) -> Result<(), ExecutionError> {
        let mut backoff_ms = 300u64;
        for attempt in 1..=self.config.max_retry_attempts {
            match tokio::time::timeout(
                Duration::from_millis(self.config.tool_timeout_ms),
                self.execute_step(run_id, step, decision, attempt),
            )
            .await
            {
                Ok(Ok(())) => return Ok(()),
                Ok(Err(ExecutionError::RateLimited)) => {
                    warn!(
                        run_id = %run_id,
                        step,
                        attempt,
                        action = %decision.action,
                        "rate limited, retrying"
                    );
                    sleep(Duration::from_millis(backoff_ms)).await;
                    backoff_ms = (backoff_ms * 2).min(5_000);
                }
                Ok(Err(other)) => return Err(other),
                Err(_) => {
                    warn!(
                        run_id = %run_id,
                        step,
                        attempt,
                        action = %decision.action,
                        "tool timeout"
                    );
                    if attempt == self.config.max_retry_attempts {
                        return Err(ExecutionError::Timeout);
                    }
                }
            }
        }
        Err(ExecutionError::RateLimited)
    }

    async fn execute_step(
        &self,
        run_id: &str,
        step: u32,
        decision: &StepDecision,
        attempt: u32,
    ) -> Result<(), ExecutionError> {
        info!(
            run_id = %run_id,
            step,
            attempt,
            action = %decision.action,
            "executing step"
        );
        if decision.action == "socialflow_generate_campaign_plan" && attempt == 1 {
            return Err(ExecutionError::RateLimited);
        }
        if decision.action == "generic_tool_call" && step > 8 {
            error!(
                run_id = %run_id,
                step,
                action = %decision.action,
                "simulated fatal error"
            );
            return Err(ExecutionError::Fatal("simulated tool error".to_string()));
        }
        Ok(())
    }
}

fn needs_human_approval(decision: &StepDecision, threshold: f64) -> bool {
    let spend = decision
        .payload
        .get("spend_usd")
        .and_then(|v| v.as_f64())
        .or_else(|| {
            decision
                .payload
                .get("original_payload")
                .and_then(|v| v.get("spend_usd"))
                .and_then(|v| v.as_f64())
        })
        .unwrap_or(0.0);
    let is_bulk = decision.action.contains("bulk");
    spend > threshold || is_bulk
}

fn run_status_label(status: &RunStatus) -> &'static str {
    match status {
        RunStatus::Running => "running",
        RunStatus::WaitingApproval => "waiting_approval",
        RunStatus::BudgetExceeded => "budget_exceeded",
        RunStatus::Killed => "killed",
        RunStatus::Succeeded => "succeeded",
        RunStatus::Failed => "failed",
    }
}

fn extract_variant_id(input: &serde_json::Value) -> Option<String> {
    [
        input.get("variant_id"),
        input.get("variantId"),
        input.get("variant"),
    ]
    .into_iter()
    .flatten()
    .find_map(|value| {
        value
            .as_str()
            .map(str::trim)
            .filter(|candidate| !candidate.is_empty())
            .map(ToOwned::to_owned)
    })
}
