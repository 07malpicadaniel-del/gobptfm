use chrono::NaiveDate;
use gov_domain::{DepartmentKind, JournalEntry, PaymentStatus, TaxQuote};
use gov_receipt::SignedOfficialReceipt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Solicitud para cotizar un adeudo municipal previa verificación de privacidad (LGPDPPSO).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateQuoteRequest {
    pub department: DepartmentKind,
    pub external_tax_id: String,
    pub verification_secret: String,
    pub fiscal_year: i32,
    pub payment_date: NaiveDate,
    pub uma_daily_cents: i64,
}

/// Respuesta con la cotización fiscal determinista generada por `gov-tax-engine`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateQuoteResponse {
    pub quote: TaxQuote,
}

/// Payload del webhook bancario idempotente (SPEI / Tarjeta / Ventanilla Digital).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizePaymentWebhookRequest {
    pub quote_id: Uuid,
    pub idempotency_key: String,
    pub paid_amount_cents: i64,
}

/// Respuesta de autorización contable CONAC, sincronización municipal y recibo oficial firmado.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizePaymentResponse {
    pub payment_id: Uuid,
    pub status: PaymentStatus,
    pub journal_entry: JournalEntry,
    pub receipt: SignedOfficialReceipt,
}

/// Solicitud de verificación pública de autenticidad de un recibo oficial.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyReceiptRequest {
    pub receipt: SignedOfficialReceipt,
}

/// Respuesta de verificación criptográfica Ed25519 + SHA-256.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyReceiptResponse {
    pub is_authentic: bool,
    pub sha256_digest_hex: String,
    pub legacy_folio: String,
}
