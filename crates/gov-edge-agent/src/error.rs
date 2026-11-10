use gov_domain::DomainError;
use thiserror::Error;

/// Errores del agente municipal (`gov-edge-agent`), su spool local `redb` y el adaptador legacy.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EdgeError {
    #[error("domain invariant error: {0}")]
    Domain(#[from] DomainError),

    #[error("local redb spool storage error: {0}")]
    SpoolStorageError(String),

    #[error("serialization error in edge agent: {0}")]
    SerializationError(String),

    #[error("municipal legacy database is unavailable: {0}")]
    LegacyDbUnavailable(String),

    #[error("legacy municipal account not found: {0}")]
    LegacyAccountNotFound(String),

    #[error("conflict: account '{external_tax_id}' was already paid in physical window with folio '{existing_folio}'")]
    LegacyConflictPaidInWindow {
        external_tax_id: String,
        existing_folio: String,
    },
}

impl From<EdgeError> for DomainError {
    fn from(err: EdgeError) -> Self {
        match err {
            EdgeError::Domain(domain_err) => domain_err,
            EdgeError::LegacyAccountNotFound(ext_id) => Self::InvalidExternalId(ext_id),
            EdgeError::LegacyConflictPaidInWindow {
                external_tax_id,
                existing_folio,
            } => Self::InvalidStateTransition {
                from: format!("PaidInWindow({external_tax_id}:{existing_folio})"),
                to: "Authorized".to_owned(),
            },
            EdgeError::LegacyDbUnavailable(reason) => Self::InvalidStateTransition {
                from: "LegacyDbOffline".to_owned(),
                to: reason,
            },
            EdgeError::SpoolStorageError(reason) => Self::InvalidStateTransition {
                from: "LocalSpool".to_owned(),
                to: reason,
            },
            EdgeError::SerializationError(reason) => Self::InvalidStateTransition {
                from: "Serialization".to_owned(),
                to: reason,
            },
        }
    }
}
