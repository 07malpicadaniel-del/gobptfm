//! Cloud Core Library & REST/Webhooks Orchestrator (`gov-core-api`).

pub mod dto;
pub mod error;
pub mod routes;
pub mod state;

pub use dto::{
    AuthorizePaymentResponse, AuthorizePaymentWebhookRequest, CreateQuoteRequest,
    CreateQuoteResponse, VerifyReceiptRequest, VerifyReceiptResponse,
};
pub use error::{ApiError, ErrorResponseBody};
pub use routes::build_router;
pub use state::{hash_verification_secret, AppState, CorePaymentService};

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use axum::body::{to_bytes, Body};
    use axum::http::{header, Request, StatusCode};
    use chrono::NaiveDate;
    use gov_domain::{
        DepartmentKind, ExternalTaxId, MoneyCents, PaymentStatus, TaxObject, TaxObjectId,
        TaxpayerBenefit, TenantId,
    };
    use gov_edge_agent::{
        EdgeCoordinator, InMemoryLegacyAdapter, LegacyAccountRecord, LocalPaymentSpool,
    };
    use gov_receipt::ReceiptSigner;
    use gov_tax_engine::{standard_property_tax_script, RhaiTaxRuleEngine};
    use serde_json::json;
    use tower::ServiceExt;
    use uuid::Uuid;

    use super::*;

    type E2eHarness = (
        axum::Router,
        Arc<CorePaymentService>,
        Arc<InMemoryLegacyAdapter>,
        PathBuf,
    );

    const OWNER_SECRET: &str = "RFC-VERA800101-991";
    const UMA_DAILY_CENTS: i64 = 11_314;

    fn setup_e2e_harness(label: &str) -> Result<E2eHarness, Box<dyn std::error::Error>> {
        let tenant = TenantId::try_new("mun_veracruz")?;
        let ext_id = ExternalTaxId::try_new("30-193-001-01-0100")?;

        let tax_engine = Arc::new(RhaiTaxRuleEngine::new());
        tax_engine.register_rule(
            tenant.clone(),
            DepartmentKind::PropertyTax,
            2026,
            standard_property_tax_script(),
            30,
        )?;

        let spool_path = std::env::temp_dir().join(format!(
            "gov_core_e2e_{label}_{}.redb",
            Uuid::now_v7()
        ));
        let spool = Arc::new(LocalPaymentSpool::open(&spool_path)?);
        let legacy_adapter = Arc::new(InMemoryLegacyAdapter::new());

        // Valor catastral: $800,000.00 MXN (80_000_000 centavos) con beneficio INAPAM
        // Impuesto Base (25 bps) = $2,000.00 (200_000) + Adicional 15% ($300.00 = 30_000)
        // - Descuento INAPAM 50% ($1,000.00 = 100_000) = Total Neto $1,300.00 (130_000 centavos)
        let owner_hash = hash_verification_secret(OWNER_SECRET);
        legacy_adapter.upsert_account(LegacyAccountRecord {
            external_tax_id: ext_id.clone(),
            department: DepartmentKind::PropertyTax,
            owner_verification_hash: owner_hash.clone(),
            cadastral_value: MoneyCents::from_cents(80_000_000),
            current_debt: MoneyCents::from_cents(130_000),
            active_benefit: TaxpayerBenefit::InapamSenior,
            is_paid_current_year: false,
            paid_folio: None,
            updated_at_unix: 1_762_720_000,
        });

        let bridge = Arc::new(EdgeCoordinator::new(
            tenant.clone(),
            spool,
            Arc::clone(&legacy_adapter),
        ));

        let signer = Arc::new(ReceiptSigner::from_seed_bytes(&[0x5A_u8; 32]));
        let service = Arc::new(CorePaymentService::new(tax_engine, signer, bridge));

        service.register_tax_object(TaxObject {
            id: TaxObjectId::new_v7(),
            tenant_id: tenant,
            department: DepartmentKind::PropertyTax,
            external_id: ext_id,
            owner_verification_hash: owner_hash,
            cadastral_or_base_value: MoneyCents::from_cents(80_000_000),
            active_benefit: TaxpayerBenefit::InapamSenior,
            metadata: json!({"municipio": "Veracruz", "uso": "Habitacional"}),
        });

        let router = build_router(AppState::new(Arc::clone(&service)));
        Ok((router, service, legacy_adapter, spool_path))
    }

    #[tokio::test]
    async fn test_e2e_1_cadastral_privacy_verification_blocks_wrong_secret(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (router, _service, _adapter, spool_path) = setup_e2e_harness("privacy_403")?;

        let jan_15 = NaiveDate::from_ymd_opt(2026, 1, 15)
            .ok_or("invalid test date")?;
        let req_body = CreateQuoteRequest {
            department: DepartmentKind::PropertyTax,
            external_tax_id: "30-193-001-01-0100".to_owned(),
            verification_secret: "SECRETO-INCORRECTO-000".to_owned(),
            fiscal_year: 2026,
            payment_date: jan_15,
            uma_daily_cents: UMA_DAILY_CENTS,
        };

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/tenants/mun_veracruz/quotes")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&req_body)?))?;

        let response = router.oneshot(request).await?;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let _ = std::fs::remove_file(spool_path);
        Ok(())
    }

    #[tokio::test]
    async fn test_e2e_2_full_flow_quote_inapam_webhook_conac_legacy_and_pdf(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (router, service, adapter, spool_path) = setup_e2e_harness("full_flow")?;

        // 1. Cotizar predio en Enero con beneficio INAPAM
        let jan_15 = NaiveDate::from_ymd_opt(2026, 1, 15)
            .ok_or("invalid test date")?;
        let quote_req = CreateQuoteRequest {
            department: DepartmentKind::PropertyTax,
            external_tax_id: "30-193-001-01-0100".to_owned(),
            verification_secret: OWNER_SECRET.to_owned(),
            fiscal_year: 2026,
            payment_date: jan_15,
            uma_daily_cents: UMA_DAILY_CENTS,
        };

        let resp_quote = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/tenants/mun_veracruz/quotes")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&quote_req)?))?,
            )
            .await?;
        assert_eq!(resp_quote.status(), StatusCode::OK);

        let quote_bytes = to_bytes(resp_quote.into_body(), 1024 * 64).await?;
        let quote_dto: CreateQuoteResponse = serde_json::from_slice(&quote_bytes)?;
        assert_eq!(quote_dto.quote.subtotal, MoneyCents::from_cents(230_000));
        assert_eq!(
            quote_dto.quote.total_discount,
            MoneyCents::from_cents(100_000)
        );
        assert_eq!(
            quote_dto.quote.total_to_pay,
            MoneyCents::from_cents(130_000)
        );

        // 2. Disparar Webhook Bancario Idempotente
        let webhook_req = AuthorizePaymentWebhookRequest {
            quote_id: quote_dto.quote.quote_id,
            idempotency_key: "idem-spei-banxico-20260115-0001".to_owned(),
            paid_amount_cents: 130_000,
        };

        let resp_pay = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/tenants/mun_veracruz/webhooks/payments")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&webhook_req)?))?,
            )
            .await?;
        assert_eq!(resp_pay.status(), StatusCode::OK);

        let pay_bytes = to_bytes(resp_pay.into_body(), 1024 * 64).await?;
        let pay_dto: AuthorizePaymentResponse = serde_json::from_slice(&pay_bytes)?;

        assert_eq!(pay_dto.status, PaymentStatus::SyncedToMunicipality);
        assert_eq!(
            pay_dto.journal_entry.total_balanced_amount()?,
            MoneyCents::from_cents(260_000)
        );
        assert_eq!(
            pay_dto.receipt.payload.legacy_folio,
            "LEGACY-FOLIO-mun_veracruz-2026-1"
        );
        assert_eq!(pay_dto.receipt.verify(), Ok(()));
        assert_eq!(service.journal_entries_count(), 1);
        assert_eq!(adapter.applied_mutations_count(), 1);

        // 3. Descargar el PDF Oficial firmado
        let pdf_uri = format!(
            "/api/v1/tenants/mun_veracruz/receipts/{}/pdf",
            pay_dto.payment_id
        );
        let resp_pdf = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(&pdf_uri)
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(resp_pdf.status(), StatusCode::OK);
        assert_eq!(
            resp_pdf.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()),
            Some("application/pdf")
        );

        let pdf_bytes = to_bytes(resp_pdf.into_body(), 1024 * 128).await?;
        assert!(pdf_bytes.starts_with(b"%PDF-1.4\n"));
        assert!(pdf_bytes.ends_with(b"%%EOF\n"));

        // 4. Verificar autenticidad del recibo vía endpoint público
        let verify_req = VerifyReceiptRequest {
            receipt: pay_dto.receipt,
        };
        let resp_verify = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/receipts/verify")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&verify_req)?))?,
            )
            .await?;
        assert_eq!(resp_verify.status(), StatusCode::OK);

        let _ = std::fs::remove_file(spool_path);
        Ok(())
    }

    #[tokio::test]
    async fn test_e2e_3_bank_webhook_idempotency_under_retries(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (router, service, adapter, spool_path) = setup_e2e_harness("webhook_idempotency")?;

        let jan_15 = NaiveDate::from_ymd_opt(2026, 1, 15)
            .ok_or("invalid test date")?;
        let quote_req = CreateQuoteRequest {
            department: DepartmentKind::PropertyTax,
            external_tax_id: "30-193-001-01-0100".to_owned(),
            verification_secret: OWNER_SECRET.to_owned(),
            fiscal_year: 2026,
            payment_date: jan_15,
            uma_daily_cents: UMA_DAILY_CENTS,
        };

        let resp_quote = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/tenants/mun_veracruz/quotes")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&quote_req)?))?,
            )
            .await?;
        let quote_bytes = to_bytes(resp_quote.into_body(), 1024 * 64).await?;
        let quote_dto: CreateQuoteResponse = serde_json::from_slice(&quote_bytes)?;

        let webhook_req = AuthorizePaymentWebhookRequest {
            quote_id: quote_dto.quote.quote_id,
            idempotency_key: "idem-spei-retry-test-2026-9999".to_owned(),
            paid_amount_cents: 130_000,
        };

        // Primer envío del webhook
        let resp_1 = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/tenants/mun_veracruz/webhooks/payments")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&webhook_req)?))?,
            )
            .await?;
        assert_eq!(resp_1.status(), StatusCode::OK);
        let dto_1: AuthorizePaymentResponse =
            serde_json::from_slice(&to_bytes(resp_1.into_body(), 1024 * 64).await?)?;

        // Segundo envío idéntico (reintento del banco)
        let resp_2 = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/tenants/mun_veracruz/webhooks/payments")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&webhook_req)?))?,
            )
            .await?;
        assert_eq!(resp_2.status(), StatusCode::OK);
        let dto_2: AuthorizePaymentResponse =
            serde_json::from_slice(&to_bytes(resp_2.into_body(), 1024 * 64).await?)?;

        assert_eq!(dto_1.payment_id, dto_2.payment_id);
        assert_eq!(
            dto_1.receipt.payload.receipt_id,
            dto_2.receipt.payload.receipt_id
        );
        assert_eq!(
            dto_1.receipt.payload.legacy_folio,
            dto_2.receipt.payload.legacy_folio
        );
        assert_eq!(service.journal_entries_count(), 1);
        assert_eq!(adapter.applied_mutations_count(), 1);

        let _ = std::fs::remove_file(spool_path);
        Ok(())
    }
}
