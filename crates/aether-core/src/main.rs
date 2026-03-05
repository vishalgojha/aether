use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use tracing::info;

use aether_core::{
    config::AppConfig,
    engine::Orchestrator,
    http::{router, HttpState},
    metrics::AppMetrics,
    observability::init_observability,
    state::StateStore,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _observability = init_observability("aether-core")?;
    let config = AppConfig::from_env();
    std::fs::create_dir_all("state")?;

    let state = StateStore::new(&config.db_path)?;
    let metrics = AppMetrics::new()?;
    let orchestrator = Arc::new(Orchestrator::new(config.clone(), state, metrics));
    let state = HttpState {
        orchestrator: orchestrator.clone(),
    };
    let app = router(state);

    let addr: SocketAddr = config.server_addr.parse()?;
    let listener = TcpListener::bind(addr).await?;
    info!(server_addr = %addr, "aether core listening");
    axum::serve(listener, app).await?;
    Ok(())
}
