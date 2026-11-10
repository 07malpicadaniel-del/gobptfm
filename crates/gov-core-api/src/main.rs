//! Cloud Core Binary: Axum REST/Webhooks API + Hexagonal Coordinator for Municipal Edge Agents.

use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use gov_core_api::{build_router, AppState, CorePaymentService};
use gov_domain::{DepartmentKind, TenantId};
use gov_edge_agent::{EdgeCoordinator, InMemoryLegacyAdapter, LocalPaymentSpool};
use gov_receipt::ReceiptSigner;
use gov_tax_engine::{standard_property_tax_script, RhaiTaxRuleEngine};
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .init();

    let default_tenant = TenantId::try_new(
        env::var("GOV_DEFAULT_TENANT").unwrap_or_else(|_| "mun_veracruz".to_owned()),
    )?;

    let tax_engine = Arc::new(RhaiTaxRuleEngine::new());
    tax_engine.register_rule(
        default_tenant.clone(),
        DepartmentKind::PropertyTax,
        2026,
        standard_property_tax_script(),
        30,
    )?;

    let spool_path = env::var("GOV_CORE_FALLBACK_SPOOL").map_or_else(
        |_| std::env::temp_dir().join("gov_core_bridge_spool.redb"),
        PathBuf::from,
    );
    let spool = Arc::new(LocalPaymentSpool::open(spool_path)?);
    let adapter = Arc::new(InMemoryLegacyAdapter::new());
    let bridge = Arc::new(EdgeCoordinator::new(default_tenant, spool, adapter));

    let signer = Arc::new(ReceiptSigner::from_seed_bytes(&[0x33_u8; 32]));
    let service = Arc::new(CorePaymentService::new(tax_engine, signer, bridge));
    let app = build_router(AppState::new(service));

    let bind_addr = env::var("GOV_API_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_owned());
    let listener = TcpListener::bind(&bind_addr).await?;
    info!(bind_addr = %bind_addr, "gov-core-api listening");

    axum::serve(listener, app).await?;
    Ok(())
}
