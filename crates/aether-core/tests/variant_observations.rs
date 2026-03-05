use std::{fs, path::PathBuf, sync::Arc};

use aether_core::{
    config::AppConfig, engine::Orchestrator, metrics::AppMetrics, state::StateStore,
    types::RunRequest,
};
use tempfile::NamedTempFile;

#[tokio::test]
async fn run_finish_writes_variant_observation_jsonl() {
    let file = NamedTempFile::new().expect("temp db");
    let db_path = file.path().to_string_lossy().to_string();
    let config = AppConfig {
        db_path: db_path.clone(),
        ..AppConfig::default()
    };
    let state = Arc::new(StateStore::new(&config.db_path).expect("state"));
    let metrics = AppMetrics::new().expect("metrics");
    let orch = Orchestrator::new(config, (*state).clone(), metrics);

    let output = orch
        .run_new(RunRequest {
            workflow: "growth".to_string(),
            input: serde_json::json!({ "variant_id": "creative-planner-v1" }),
        })
        .await
        .expect("run");

    let observations_path = PathBuf::from(&db_path)
        .parent()
        .expect("db parent")
        .join("variant-observations.jsonl");
    let raw = fs::read_to_string(&observations_path).expect("observations file");
    let last_line = raw
        .lines()
        .filter(|line| !line.trim().is_empty())
        .last()
        .expect("observation line");
    let observation: serde_json::Value = serde_json::from_str(last_line).expect("valid json line");

    assert_eq!(
        observation.get("run_id").and_then(|value| value.as_str()),
        Some(output.run_id.as_str())
    );
    assert_eq!(
        observation.get("workflow").and_then(|value| value.as_str()),
        Some("growth")
    );
    assert_eq!(
        observation
            .get("variant_id")
            .and_then(|value| value.as_str()),
        Some("creative-planner-v1")
    );
    assert_eq!(
        observation.get("status").and_then(|value| value.as_str()),
        Some("waiting_approval")
    );
}
