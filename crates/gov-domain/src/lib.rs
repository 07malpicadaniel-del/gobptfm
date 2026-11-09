//! Pure domain types, financial newtypes, CONAC accounting moments, errors, and repository/adapter traits.
//! Strictly decoupled from any I/O, database, or transport libraries (`sqlx`, `axum`, `tonic`).

pub mod conac;
pub mod error;
pub mod identifiers;
pub mod money;
pub mod payment;
pub mod ports;
pub mod tax_object;

pub use conac::{AccountingMoment, EntrySide, JournalEntry, LedgerLine};
pub use error::DomainError;
pub use identifiers::{CriCode, ExternalTaxId, IdempotencyKey, TaxObjectId, TenantId};
pub use money::MoneyCents;
pub use payment::{PaymentStatus, SyncOutboxStatus};
pub use ports::{LedgerRepositoryPort, MunicipalBridgePort, TaxRuleEnginePort};
pub use tax_object::{DepartmentKind, QuoteLineItem, TaxObject, TaxQuote, TaxpayerBenefit};
