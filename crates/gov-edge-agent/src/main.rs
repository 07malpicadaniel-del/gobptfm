//! Municipal Edge Agent Binary: Tonic gRPC Client + Power-loss safe local queue (`redb`) + Legacy DB Adapters.

use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use gov_domain::TenantId;
use gov_edge_agent::{EdgeCoordinator, EdgeError, InMemoryLegacyAdapter, LocalPaymentSpool};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), EdgeError> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .init();

    let raw_tenant =
        env::var("GOV_TENANT_ID").unwrap_or_else(|_| "mun_default_edge".to_owned());
    let tenant_id = TenantId::try_new(raw_tenant)?;

    let spool_path = env::var("GOV_SPOOL_PATH").map_or_else(
        |_| std::env::temp_dir().join(format!("gov_edge_{tenant_id}.redb")),
        PathBuf::from,
    );

    let spool = Arc::new(LocalPaymentSpool::open(&spool_path)?);
    let adapter = Arc::new(InMemoryLegacyAdapter::new());
    let coordinator = EdgeCoordinator::new(tenant_id.clone(), spool, adapter);

    let heartbeat = coordinator.build_heartbeat().await?;
    info!(
        tenant_id = %heartbeat.tenant_id,
        agent_version = %heartbeat.agent_version,
        pending_spool_count = heartbeat.pending_spool_count,
        legacy_db_reachable = heartbeat.legacy_db_reachable,
        spool_path = %spool_path.display(),
        "gov-edge-agent initialized and ready"
    );

    Ok(())
}
