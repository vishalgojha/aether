use prometheus::{
    register_histogram_vec_with_registry, register_int_counter_vec_with_registry,
    register_int_counter_with_registry, register_int_gauge_with_registry, Encoder, HistogramVec,
    IntCounter, IntCounterVec, IntGauge, Registry, TextEncoder,
};

#[derive(Clone)]
pub struct AppMetrics {
    pub registry: Registry,
    pub runs_started: IntCounter,
    pub runs_finished: IntCounterVec,
    pub run_duration_seconds: HistogramVec,
    pub step_failures: IntCounterVec,
    pub step_latency_seconds: HistogramVec,
    pub tokens_used: IntCounter,
    pub cost_microusd: IntCounter,
    pub pending_approvals: IntGauge,
    pub approvals_requested: IntCounter,
    pub approvals_granted: IntCounter,
    pub decision_path: IntCounterVec,
}

impl AppMetrics {
    pub fn new() -> anyhow::Result<Self> {
        let registry = Registry::new_custom(Some("aether".to_string()), None)?;
        let runs_started = register_int_counter_with_registry!(
            "runs_started_total",
            "Total runs started",
            registry
        )?;
        let runs_finished = register_int_counter_vec_with_registry!(
            "runs_finished_total",
            "Total finished runs by status",
            &["status"],
            registry
        )?;
        let run_duration_seconds = register_histogram_vec_with_registry!(
            "run_duration_seconds",
            "Run duration in seconds by operation and status",
            &["operation", "status"],
            registry
        )?;
        let step_failures = register_int_counter_vec_with_registry!(
            "step_failures_total",
            "Step failures by reason",
            &["reason"],
            registry
        )?;
        let step_latency_seconds = register_histogram_vec_with_registry!(
            "step_latency_seconds",
            "Latency per step",
            &["tool", "result"],
            registry
        )?;
        let tokens_used = register_int_counter_with_registry!(
            "tokens_used_total",
            "Total estimated tokens consumed",
            registry
        )?;
        let cost_microusd = register_int_counter_with_registry!(
            "cost_microusd_total",
            "Total estimated cost in micro USD",
            registry
        )?;
        let pending_approvals = register_int_gauge_with_registry!(
            "pending_approvals",
            "Current number of pending approvals",
            registry
        )?;
        let approvals_requested = register_int_counter_with_registry!(
            "approvals_requested_total",
            "Total approval requests created",
            registry
        )?;
        let approvals_granted = register_int_counter_with_registry!(
            "approvals_granted_total",
            "Total approvals granted",
            registry
        )?;
        let decision_path = register_int_counter_vec_with_registry!(
            "decision_path_total",
            "Decision path counter",
            &["path"],
            registry
        )?;

        Ok(Self {
            registry,
            runs_started,
            runs_finished,
            run_duration_seconds,
            step_failures,
            step_latency_seconds,
            tokens_used,
            cost_microusd,
            pending_approvals,
            approvals_requested,
            approvals_granted,
            decision_path,
        })
    }

    pub fn render(&self) -> anyhow::Result<String> {
        let mut buffer = Vec::new();
        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();
        encoder.encode(&metric_families, &mut buffer)?;
        Ok(String::from_utf8(buffer)?)
    }
}
