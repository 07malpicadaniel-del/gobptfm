//! Municipal Edge Agent Library: Power-loss safe local queue (`redb`), legacy municipal DB adapters,
//! and bidirectional bridge coordinator (`MunicipalBridgePort`).

pub mod coordinator;
pub mod error;
pub mod legacy_adapter;
pub mod spool;

pub use coordinator::EdgeCoordinator;
pub use error::EdgeError;
pub use legacy_adapter::{
    InMemoryLegacyAdapter, LegacyAccountRecord, LegacyMunicipalDbAdapter,
};
pub use spool::{
    LocalPaymentSpool, SpoolEntry, COMPLETED_INJECTIONS, PENDING_INJECTIONS,
};

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use gov_domain::{DepartmentKind, ExternalTaxId, MoneyCents, TaxpayerBenefit, TenantId};
    use gov_proto::{InjectPaymentCommand, InjectionAckStatus, PreflightDebtCheckRequest};
    use uuid::Uuid;

    use super::*;

    fn temp_spool_path(test_label: &str) -> PathBuf {
        let unique = Uuid::now_v7();
        std::env::temp_dir().join(format!("gov_edge_spool_{test_label}_{unique}.redb"))
    }

    type TestSetup = (
        EdgeCoordinator<InMemoryLegacyAdapter>,
        Arc<InMemoryLegacyAdapter>,
        Arc<LocalPaymentSpool>,
        PathBuf,
    );

    fn setup_coordinator(test_label: &str) -> Result<TestSetup, EdgeError> {
        let path = temp_spool_path(test_label);
        let spool = Arc::new(LocalPaymentSpool::open(&path)?);
        let adapter = Arc::new(InMemoryLegacyAdapter::new());
        let tenant = TenantId::try_new("mun_veracruz")?;

        let ext_id = ExternalTaxId::try_new("30-193-001-01-0100")?;
        adapter.upsert_account(LegacyAccountRecord {
            external_tax_id: ext_id,
            department: DepartmentKind::PropertyTax,
            owner_verification_hash: "hash_titular_veracruz_01".to_owned(),
            cadastral_value: MoneyCents::from_cents(120_000_000),
            current_debt: MoneyCents::from_cents(285_000),
            active_benefit: TaxpayerBenefit::None,
            is_paid_current_year: false,
            paid_folio: None,
            updated_at_unix: 1_762_720_000,
        });

        let coordinator = EdgeCoordinator::new(tenant, Arc::clone(&spool), Arc::clone(&adapter));
        Ok((coordinator, adapter, spool, path))
    }

    fn sample_inject_cmd(payment_id: &str) -> InjectPaymentCommand {
        InjectPaymentCommand {
            outbox_id: Uuid::now_v7().to_string(),
            payment_id: payment_id.to_owned(),
            tenant_id: "mun_veracruz".to_owned(),
            department: "PropertyTax".to_owned(),
            external_tax_id: "30-193-001-01-0100".to_owned(),
            idempotency_key: format!("idem-{payment_id}"),
            amount_cents: 285_000,
            fiscal_year: 2026,
            paid_at_unix: 1_762_725_000,
        }
    }

    #[tokio::test]
    async fn test_double_idempotency_three_identical_injections_apply_once() -> Result<(), EdgeError>
    {
        let (coordinator, adapter, spool, path) = setup_coordinator("idempotency")?;
        let payment_id = Uuid::now_v7().to_string();
        let cmd = sample_inject_cmd(&payment_id);

        let ack_1 = coordinator.handle_inject_payment(cmd.clone()).await?;
        let ack_2 = coordinator.handle_inject_payment(cmd.clone()).await?;
        let ack_3 = coordinator.handle_inject_payment(cmd).await?;

        assert_eq!(ack_1.ack_status(), InjectionAckStatus::Committed);
        assert_eq!(ack_2.ack_status(), InjectionAckStatus::Committed);
        assert_eq!(ack_3.ack_status(), InjectionAckStatus::Committed);

        // El mismo folio exacto se retorna en las 3 llamadas y la DB legacy solo se mutó 1 vez
        assert_eq!(
            ack_1.legacy_receipt_folio,
            "LEGACY-FOLIO-mun_veracruz-2026-1"
        );
        assert_eq!(ack_1.legacy_receipt_folio, ack_2.legacy_receipt_folio);
        assert_eq!(ack_2.legacy_receipt_folio, ack_3.legacy_receipt_folio);
        assert_eq!(adapter.applied_mutations_count(), 1);
        assert_eq!(spool.pending_count()?, 0);

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn test_chaos_municipal_db_blackout_queues_locally_and_flushes_on_recovery(
    ) -> Result<(), EdgeError> {
        let (coordinator, adapter, spool, path) = setup_coordinator("chaos_blackout")?;
        let payment_id = Uuid::now_v7().to_string();
        let cmd = sample_inject_cmd(&payment_id);

        // 1. Domingo: Se apaga el SQL Server local del municipio
        adapter.set_online(false);

        // 2. Llega el pago desde la nube -> se guarda en redb y responde QueuedLocally
        let ack_offline = coordinator.handle_inject_payment(cmd).await?;
        assert_eq!(ack_offline.ack_status(), InjectionAckStatus::QueuedLocally);
        assert_eq!(spool.pending_count()?, 1);
        assert_eq!(spool.get_completed_folio(&payment_id)?, None);

        // 3. Lunes 8:00 AM: Encienden el servidor SQL Server municipal y corre flush_pending_spool
        adapter.set_online(true);
        let flushed = coordinator.flush_pending_spool().await?;
        assert_eq!(flushed, 1);
        assert_eq!(spool.pending_count()?, 0);

        let completed_folio = spool.get_completed_folio(&payment_id)?;
        assert_eq!(
            completed_folio,
            Some("LEGACY-FOLIO-mun_veracruz-2026-1".to_owned())
        );

        let ext_id = ExternalTaxId::try_new("30-193-001-01-0100")?;
        let updated_acc = adapter.fetch_account(&ext_id).await?;
        assert!(updated_acc.is_paid_current_year);
        assert_eq!(updated_acc.current_debt, MoneyCents::ZERO);

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn test_preflight_blocks_double_payment_when_paid_in_physical_window(
    ) -> Result<(), EdgeError> {
        let (coordinator, adapter, _spool, path) = setup_coordinator("preflight_window")?;
        let ext_id = ExternalTaxId::try_new("30-193-001-01-0100")?;

        // Simular que el ciudadano pagó en caja física hace 2 minutos
        adapter.simulate_physical_window_payment(&ext_id, "CAJA-PALACIO-2026-0099")?;

        let preflight_req = PreflightDebtCheckRequest {
            request_id: Uuid::now_v7().to_string(),
            tenant_id: "mun_veracruz".to_owned(),
            department: "PropertyTax".to_owned(),
            external_tax_id: ext_id.to_string(),
            expected_amount_cents: 285_000,
        };

        let resp = coordinator.handle_preflight_check(&preflight_req).await;
        assert!(!resp.is_valid);
        assert!(resp.is_already_paid_in_window);
        assert!(resp.rejection_reason.contains("CAJA-PALACIO-2026-0099"));

        let _ = std::fs::remove_file(path);
        Ok(())
    }
}
