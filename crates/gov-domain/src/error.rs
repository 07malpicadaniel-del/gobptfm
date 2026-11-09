use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Canonical domain error for GovTech Core operations.
/// Strictly typed and free of I/O or infrastructure coupling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
pub enum DomainError {
    #[error("arithmetic overflow occurred during financial calculation")]
    ArithmeticOverflow,

    #[error("negative or non-positive amount not allowed in this context: {0} cents")]
    NegativeAmountNotAllowed(i64),

    #[error("invalid tenant identifier '{0}': must be 3..=32 chars of [a-z0-9_]")]
    InvalidTenantId(String),

    #[error("invalid external tax identifier '{0}': must be 1..=64 printable characters after normalization")]
    InvalidExternalId(String),

    #[error("invalid idempotency key '{0}': must be 16..=128 printable ASCII characters")]
    InvalidIdempotencyKey(String),

    #[error("invalid CONAC CRI code '{0}': must be dot-separated numeric segments (e.g. '1.2.01.01')")]
    InvalidCriCode(String),

    #[error("unbalanced double-entry journal: total debits ({total_debits}) != total credits ({total_credits})")]
    UnbalancedJournalEntry {
        total_debits: i64,
        total_credits: i64,
    },

    #[error("journal entry cannot have zero ledger lines")]
    EmptyJournalEntry,

    #[error("invalid state transition from '{from}' to '{to}'")]
    InvalidStateTransition { from: String, to: String },

    #[error("tax quote has expired and can no longer be settled")]
    QuoteExpired,

    #[error("taxpayer privacy verification hash mismatch")]
    PrivacyVerificationFailed,
}
