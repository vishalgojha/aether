use aether_core::{
    config::AppConfig, engine::Orchestrator, metrics::AppMetrics, state::StateStore,
};
use tempfile::NamedTempFile;

fn setup_orchestrator() -> (Orchestrator, StateStore) {
    let file = NamedTempFile::new().expect("temp db");
    let config = AppConfig {
        db_path: file.path().to_string_lossy().to_string(),
        ..AppConfig::default()
    };
    let state = StateStore::new(&config.db_path).expect("state");
    let metrics = AppMetrics::new().expect("metrics");
    let orchestrator = Orchestrator::new(config, state.clone(), metrics);
    (orchestrator, state)
}

#[test]
fn approve_requires_non_empty_reason() {
    let (orchestrator, state) = setup_orchestrator();
    state.create_run("run-1", "growth").expect("create run");
    state
        .create_approval_request("run-1", "step-2", "socialflow_launch_campaign", 250.0)
        .expect("create approval request");

    let err = orchestrator
        .approve("run-1", "step-2", "ops", "   ")
        .expect_err("missing reason should fail");
    assert!(
        err.to_string().contains("approval reason required"),
        "unexpected error: {err}"
    );
}

#[test]
fn approve_event_includes_reason() {
    let (orchestrator, state) = setup_orchestrator();
    state.create_run("run-2", "growth").expect("create run");
    state
        .create_approval_request("run-2", "step-2", "socialflow_launch_campaign", 250.0)
        .expect("create approval request");

    let approved = orchestrator
        .approve(
            "run-2",
            "step-2",
            "ops",
            "reviewed pacing and budget controls",
        )
        .expect("approve should succeed");
    assert!(approved, "expected approval update");

    let events = state.list_events("run-2").expect("events");
    let approval_event = events
        .iter()
        .find(|event| event.event_type == "approval_granted")
        .expect("approval_granted event");
    assert_eq!(
        approval_event
            .payload
            .get("reason")
            .and_then(|value| value.as_str()),
        Some("reviewed pacing and budget controls")
    );
}
