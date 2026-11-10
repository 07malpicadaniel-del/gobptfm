use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use gov_domain::DomainError;
use gov_receipt::ReceiptError;
use gov_tax_engine::TaxEngineError;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Cuerpo estructurado de error HTTP JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorResponseBody {
    pub error_code: String,
    pub message: String,
}

/// Errores de la capa HTTP/REST (`gov-core-api`) con mapeo determinista hacia códigos de estado HTTP.
#[derive(Debug, Error)]
pub enum ApiError {
    #[error("{0}")]
    Domain(#[from] DomainError),

    #[error("{0}")]
    TaxEngine(#[from] TaxEngineError),

    #[error("{0}")]
    Receipt(#[from] ReceiptError),

    #[error("tax object not found for tenant '{tenant_id}' and external_id '{external_id}'")]
    TaxObjectNotFound {
        tenant_id: String,
        external_id: String,
    },

    #[error("tax quote not found: {0}")]
    QuoteNotFound(Uuid),

    #[error("official receipt not found for payment_id: {0}")]
    ReceiptNotFound(Uuid),

    #[error("paid amount ({paid_cents} cents) does not match quote total_to_pay ({expected_cents} cents)")]
    PaymentAmountMismatch {
        expected_cents: i64,
        paid_cents: i64,
    },

    #[error("municipal legacy preflight check rejected payment: debt mismatch or already paid in window")]
    PreflightConflict(String),

    #[error("internal server error: {0}")]
    Internal(String),
}

impl ApiError {
    #[must_use]
    pub fn status_and_code(&self) -> (StatusCode, &'static str) {
        match self {
            Self::Domain(domain_err) => match domain_err {
                DomainError::PrivacyVerificationFailed => {
                    (StatusCode::FORBIDDEN, "PRIVACY_VERIFICATION_FAILED")
                }
                DomainError::QuoteExpired => (StatusCode::CONFLICT, "QUOTE_EXPIRED"),
                DomainError::InvalidTenantId(_)
                | DomainError::InvalidExternalId(_)
                | DomainError::InvalidIdempotencyKey(_)
                | DomainError::InvalidCriCode(_)
                | DomainError::NegativeAmountNotAllowed(_) => {
                    (StatusCode::BAD_REQUEST, "DOMAIN_VALIDATION_ERROR")
                }
                DomainError::InvalidStateTransition { from, .. }
                    if from.starts_with("PaidInWindow") =>
                {
                    (StatusCode::CONFLICT, "LEGACY_WINDOW_CONFLICT")
                }
                DomainError::InvalidStateTransition { .. } => {
                    (StatusCode::CONFLICT, "INVALID_STATE_TRANSITION")
                }
                DomainError::ArithmeticOverflow
                | DomainError::UnbalancedJournalEntry { .. }
                | DomainError::EmptyJournalEntry
                | DomainError::TaxRuleEvaluationFailed(_) => {
                    (StatusCode::INTERNAL_SERVER_ERROR, "DOMAIN_INVARIANT_ERROR")
                }
            },
            Self::TaxEngine(tax_err) => match tax_err {
                TaxEngineError::RuleNotFound { .. } => {
                    (StatusCode::NOT_FOUND, "TAX_RULE_NOT_FOUND")
                }
                TaxEngineError::Domain(d) => Self::Domain(d.clone()).status_and_code(),
                TaxEngineError::ScriptCompilationError(_)
                | TaxEngineError::ScriptEvaluationError(_)
                | TaxEngineError::InvalidScriptOutput(_) => {
                    (StatusCode::UNPROCESSABLE_ENTITY, "TAX_RULE_EVALUATION_ERROR")
                }
            },
            Self::Receipt(receipt_err) => match receipt_err {
                ReceiptError::TamperedCanonicalChain
                | ReceiptError::SignatureVerificationFailed
                | ReceiptError::InvalidPublicKeyBytes
                | ReceiptError::InvalidSignatureFormat => {
                    (StatusCode::BAD_REQUEST, "RECEIPT_VERIFICATION_FAILED")
                }
                ReceiptError::Domain(d) => Self::Domain(d.clone()).status_and_code(),
                ReceiptError::InvalidSigningKeyBytes | ReceiptError::PdfGenerationError(_) => {
                    (StatusCode::INTERNAL_SERVER_ERROR, "RECEIPT_GENERATION_ERROR")
                }
            },
            Self::TaxObjectNotFound { .. } => (StatusCode::NOT_FOUND, "TAX_OBJECT_NOT_FOUND"),
            Self::QuoteNotFound(_) => (StatusCode::NOT_FOUND, "QUOTE_NOT_FOUND"),
            Self::ReceiptNotFound(_) => (StatusCode::NOT_FOUND, "RECEIPT_NOT_FOUND"),
            Self::PaymentAmountMismatch { .. } => {
                (StatusCode::CONFLICT, "PAYMENT_AMOUNT_MISMATCH")
            }
            Self::PreflightConflict(_) => (StatusCode::CONFLICT, "PREFLIGHT_DEBT_CONFLICT"),
            Self::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_SERVER_ERROR"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = self.status_and_code();
        let body = ErrorResponseBody {
            error_code: code.to_owned(),
            message: self.to_string(),
        };
        (status, Json(body)).into_response()
    }
}
