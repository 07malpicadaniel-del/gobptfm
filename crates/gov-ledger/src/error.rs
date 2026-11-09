use gov_domain::DomainError;
use thiserror::Error;
use uuid::Uuid;

/// Errores de la capa de persistencia contable (`gov-ledger`).
#[derive(Debug, Error)]
pub enum LedgerError {
    #[error("domain invariant error: {0}")]
    Domain(#[from] DomainError),

    #[error("database error: {0}")]
    Database(sqlx::Error),

    #[error("json serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("payment not found: {0}")]
    PaymentNotFound(Uuid),

    #[error("tax quote not found: {0}")]
    QuoteNotFound(Uuid),

    #[error("duplicate idempotency key conflict")]
    DuplicateIdempotencyConflict,

    #[error("CONAC append-only immutability violation: ledger and event tables cannot be updated or deleted")]
    AppendOnlyViolation,
}

impl From<sqlx::Error> for LedgerError {
    fn from(err: sqlx::Error) -> Self {
        if let sqlx::Error::Database(ref db_err) = err {
            let msg = db_err.message();
            if msg.contains("CONAC_IMMUTABILITY_VIOLATION") {
                return Self::AppendOnlyViolation;
            }
            if db_err.code().as_deref() == Some("23505")
                && (msg.contains("idempotency") || msg.contains("uq_journal_entries_tenant_idempotency"))
            {
                return Self::DuplicateIdempotencyConflict;
            }
        }
        Self::Database(err)
    }
}

impl From<LedgerError> for DomainError {
    fn from(err: LedgerError) -> Self {
        match err {
            LedgerError::Domain(domain_err) => domain_err,
            LedgerError::DuplicateIdempotencyConflict => {
                Self::InvalidIdempotencyKey("duplicate idempotency key conflict".to_owned())
            }
            LedgerError::PaymentNotFound(id) => Self::InvalidStateTransition {
                from: format!("MissingPayment({id})"),
                to: "TargetStatus".to_owned(),
            },
            LedgerError::QuoteNotFound(id) => Self::InvalidStateTransition {
                from: format!("MissingQuote({id})"),
                to: "Authorized".to_owned(),
            },
            LedgerError::AppendOnlyViolation => Self::InvalidStateTransition {
                from: "AppendOnlyLedger".to_owned(),
                to: "MutationForbidden".to_owned(),
            },
            LedgerError::Database(db_err) => Self::InvalidStateTransition {
                from: "DatabaseOperation".to_owned(),
                to: db_err.to_string(),
            },
            LedgerError::SerializationError(ser_err) => Self::InvalidStateTransition {
                from: "Serialization".to_owned(),
                to: ser_err.to_string(),
            },
        }
    }
}

