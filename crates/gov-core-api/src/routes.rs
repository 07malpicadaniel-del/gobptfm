use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use gov_domain::TenantId;
use gov_receipt::render_official_receipt_pdf;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::dto::{
    AuthorizePaymentResponse, AuthorizePaymentWebhookRequest, CreateQuoteRequest,
    CreateQuoteResponse, VerifyReceiptRequest, VerifyReceiptResponse,
};
use crate::error::ApiError;
use crate::state::AppState;

/// Construye el [`axum::Router`] principal de `gov-core-api`.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route(
            "/api/v1/tenants/{tenant_id}/quotes",
            post(create_quote_handler),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/webhooks/payments",
            post(authorize_payment_webhook_handler),
        )
        .route(
            "/api/v1/tenants/{tenant_id}/receipts/{payment_id}/pdf",
            get(download_receipt_pdf_handler),
        )
        .route("/api/v1/receipts/verify", post(verify_receipt_handler))
        .with_state(state)
}

async fn health_handler() -> (StatusCode, Json<Value>) {
    (
        StatusCode::OK,
        Json(json!({
            "status": "ok",
            "service": "gov-core-api",
            "version": env!("CARGO_PKG_VERSION"),
        })),
    )
}

async fn create_quote_handler(
    State(state): State<AppState>,
    Path(raw_tenant_id): Path<String>,
    Json(req): Json<CreateQuoteRequest>,
) -> Result<(StatusCode, Json<CreateQuoteResponse>), ApiError> {
    let tenant_id = TenantId::try_new(raw_tenant_id)?;
    let resp = state.service.create_quote(tenant_id, req).await?;
    Ok((StatusCode::OK, Json(resp)))
}

async fn authorize_payment_webhook_handler(
    State(state): State<AppState>,
    Path(raw_tenant_id): Path<String>,
    Json(req): Json<AuthorizePaymentWebhookRequest>,
) -> Result<(StatusCode, Json<AuthorizePaymentResponse>), ApiError> {
    let tenant_id = TenantId::try_new(raw_tenant_id)?;
    let resp = state
        .service
        .authorize_payment_webhook(tenant_id, req)
        .await?;
    Ok((StatusCode::OK, Json(resp)))
}

async fn download_receipt_pdf_handler(
    State(state): State<AppState>,
    Path((raw_tenant_id, payment_id)): Path<(String, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let tenant_id = TenantId::try_new(raw_tenant_id)?;
    let receipt = state.service.get_signed_receipt(&tenant_id, payment_id)?;
    let pdf_bytes = render_official_receipt_pdf(&receipt)?;

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/pdf")],
        pdf_bytes,
    ))
}

async fn verify_receipt_handler(
    Json(req): Json<VerifyReceiptRequest>,
) -> Result<(StatusCode, Json<VerifyReceiptResponse>), ApiError> {
    req.receipt.verify()?;
    Ok((
        StatusCode::OK,
        Json(VerifyReceiptResponse {
            is_authentic: true,
            sha256_digest_hex: req.receipt.sha256_digest_hex,
            legacy_folio: req.receipt.payload.legacy_folio,
        }),
    ))
}
