//! Double-entry immutable ledger engine, CONAC accounting compliance, idempotency, and transactional outbox (SQLx).

pub mod builder;
pub mod error;
pub mod repository;
pub mod tenant_schema;

pub use builder::{build_collection_journal_entry, build_reversal_journal_entry};
pub use error::LedgerError;
pub use repository::{
    validate_pre_authorization, validate_status_transition, OutboxRecord, PgLedgerRepository,
};
pub use tenant_schema::{
    build_set_search_path_sql, ensure_tenant_schema, sanitize_and_build_create_schema_sql,
    set_local_tenant_search_path, validate_tenant_schema_identifier,
};
