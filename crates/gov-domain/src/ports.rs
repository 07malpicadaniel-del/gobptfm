use async_trait::async_trait;
use chrono::NaiveDate;
use uuid::Uuid;

use crate::conac::JournalEntry;
use crate::error::DomainError;
use crate::identifiers::{ExternalTaxId, IdempotencyKey, TenantId};
use crate::money::MoneyCents;
use crate::payment::PaymentStatus;
use crate::tax_object::{TaxObject, TaxQuote};

/// Puerto hexagonal para el motor determinista de reglas fiscales (`gov-tax-engine`).
#[async_trait]
pub trait TaxRuleEnginePort: Send + Sync {
    /// Calcula una cotización fiscal (`TaxQuote`) determinista para un objeto imponible.
    async fn evaluate_quote(
        &self,
        tax_object: &TaxObject,
        fiscal_year: i32,
        uma_value: MoneyCents,
        payment_date: NaiveDate,
    ) -> Result<TaxQuote, DomainError>;
}

/// Puerto hexagonal para el repositorio contable inmutable e idempotente (`gov-ledger`).
#[async_trait]
pub trait LedgerRepositoryPort: Send + Sync {
    /// Registra una póliza contable (`JournalEntry`) de forma idempotente dentro de una transacción ACID.
    async fn record_journal_entry(&self, entry: &JournalEntry) -> Result<Uuid, DomainError>;

    /// Actualiza el estado de un pago validando la máquina de estados [`PaymentStatus::transition_to`].
    async fn transition_payment_status(
        &self,
        tenant_id: &TenantId,
        payment_id: Uuid,
        idempotency_key: &IdempotencyKey,
        next_status: PaymentStatus,
    ) -> Result<PaymentStatus, DomainError>;
}

/// Puerto hexagonal para el puente bidireccional hacia el sistema legacy municipal (`gov-edge-agent`).
#[async_trait]
pub trait MunicipalBridgePort: Send + Sync {
    /// Verifica en tiempo real (preflight check) que el adeudo siga vigente en el padrón municipal
    /// y coincida con el monto esperado antes de autorizar o dispersar el cobro.
    async fn preflight_verify_debt(
        &self,
        tenant_id: &TenantId,
        external_id: &ExternalTaxId,
        expected_amount: MoneyCents,
    ) -> Result<bool, DomainError>;

    /// Inyecta un pago autorizado en la base de datos legacy municipal y devuelve el folio local asignado.
    async fn inject_payment_to_legacy(
        &self,
        tenant_id: &TenantId,
        external_id: &ExternalTaxId,
        payment_id: Uuid,
        idempotency_key: &IdempotencyKey,
        paid_amount: MoneyCents,
    ) -> Result<String, DomainError>;
}

