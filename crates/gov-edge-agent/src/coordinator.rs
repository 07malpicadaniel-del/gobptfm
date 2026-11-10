use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Datelike, Utc};
use gov_domain::{
    DomainError, ExternalTaxId, IdempotencyKey, MoneyCents, MunicipalBridgePort, TenantId,
};
use gov_proto::{
    AgentHeartbeat, InjectPaymentAck, InjectPaymentCommand, InjectionAckStatus,
    PreflightDebtCheckRequest, PreflightDebtCheckResponse,
};
use tracing::instrument;
use uuid::Uuid;

use crate::error::EdgeError;
use crate::legacy_adapter::LegacyMunicipalDbAdapter;
use crate::spool::LocalPaymentSpool;

/// Coordinador del agente municipal (`gov-edge-agent`) que orquesta la verificación preflight,
/// la cola persistente `redb` a prueba de apagones y la inyección en el sistema legacy municipal.
#[derive(Debug, Clone)]
pub struct EdgeCoordinator<A: LegacyMunicipalDbAdapter> {
    tenant_id: TenantId,
    spool: Arc<LocalPaymentSpool>,
    adapter: Arc<A>,
}

impl<A: LegacyMunicipalDbAdapter> EdgeCoordinator<A> {
    /// Construye un nuevo [`EdgeCoordinator`].
    #[must_use]
    pub const fn new(tenant_id: TenantId, spool: Arc<LocalPaymentSpool>, adapter: Arc<A>) -> Self {
        Self {
            tenant_id,
            spool,
            adapter,
        }
    }

    /// Devuelve referencia al [`LocalPaymentSpool`].
    #[must_use]
    pub fn spool(&self) -> &LocalPaymentSpool {
        &self.spool
    }

    /// Devuelve referencia al adaptador legacy.
    #[must_use]
    pub fn adapter(&self) -> &A {
        &self.adapter
    }

    /// Ejecuta la validación previa (`preflight check`) contra el padrón legacy municipal.
    #[instrument(skip(self), fields(tenant = %req.tenant_id, ext_id = %req.external_tax_id))]
    pub async fn handle_preflight_check(
        &self,
        req: &PreflightDebtCheckRequest,
    ) -> PreflightDebtCheckResponse {
        let ext_id = match ExternalTaxId::try_new(&req.external_tax_id) {
            Ok(id) => id,
            Err(err) => {
                return PreflightDebtCheckResponse {
                    request_id: req.request_id.clone(),
                    is_valid: false,
                    current_legacy_debt_cents: 0,
                    is_already_paid_in_window: false,
                    rejection_reason: err.to_string(),
                };
            }
        };

        let account = match self.adapter.fetch_account(&ext_id).await {
            Ok(acc) => acc,
            Err(err) => {
                return PreflightDebtCheckResponse {
                    request_id: req.request_id.clone(),
                    is_valid: false,
                    current_legacy_debt_cents: 0,
                    is_already_paid_in_window: false,
                    rejection_reason: err.to_string(),
                };
            }
        };

        if account.is_paid_current_year {
            let folio_info = account
                .paid_folio
                .unwrap_or_else(|| "VENTANILLA".to_owned());
            return PreflightDebtCheckResponse {
                request_id: req.request_id.clone(),
                is_valid: false,
                current_legacy_debt_cents: account.current_debt.as_cents(),
                is_already_paid_in_window: true,
                rejection_reason: format!(
                    "account already paid in physical municipal window (folio: {folio_info})"
                ),
            };
        }

        let current_cents = account.current_debt.as_cents();
        if current_cents == req.expected_amount_cents {
            PreflightDebtCheckResponse {
                request_id: req.request_id.clone(),
                is_valid: true,
                current_legacy_debt_cents: current_cents,
                is_already_paid_in_window: false,
                rejection_reason: String::new(),
            }
        } else {
            PreflightDebtCheckResponse {
                request_id: req.request_id.clone(),
                is_valid: false,
                current_legacy_debt_cents: current_cents,
                is_already_paid_in_window: false,
                rejection_reason: format!(
                    "expected debt {} cents does not match legacy debt {} cents",
                    req.expected_amount_cents, current_cents
                ),
            }
        }
    }

    /// Procesa un comando de inyección de pago con garantía de persistencia en `redb` ante caídas
    /// de la base de datos legacy municipal:
    /// 1. Verifica idempotencia en `COMPLETED_INJECTIONS`.
    /// 2. Persiste el comando en `PENDING_INJECTIONS` antes de tocar la base de datos legacy.
    /// 3. Intenta aplicar el pago en `adapter.apply_payment(...)`:
    ///    - Éxito -> marca completado en `redb` y retorna `InjectionAckStatus::Committed`.
    ///    - Base de datos municipal apagada/caída -> registra reintento y retorna `InjectionAckStatus::QueuedLocally`.
    ///    - Conflicto por pago previo en ventanilla -> retorna `InjectionAckStatus::RejectedConflict`.
    ///
    /// # Errors
    /// Devuelve [`EdgeError`] si falla el almacenamiento local `redb` o la cuenta no existe.
    #[instrument(skip(self), fields(payment_id = %cmd.payment_id, ext_id = %cmd.external_tax_id))]
    pub async fn handle_inject_payment(
        &self,
        cmd: InjectPaymentCommand,
    ) -> Result<InjectPaymentAck, EdgeError> {
        // 1. Idempotencia inmediata si el pago ya fue completado antes
        if let Some(existing_folio) = self.spool.get_completed_folio(&cmd.payment_id)? {
            return Ok(InjectPaymentAck {
                outbox_id: cmd.outbox_id,
                payment_id: cmd.payment_id,
                status: i32::from(InjectionAckStatus::Committed),
                legacy_receipt_folio: existing_folio,
                error_message: String::new(),
            });
        }

        // 2. Persistir en la cola local redb a prueba de apagones antes de intentar inyectar
        let _enqueued = self.spool.enqueue_pending(&cmd)?;

        // 3. Intentar inyección en el sistema legacy municipal
        match self.adapter.apply_payment(&cmd).await {
            Ok(folio) => {
                self.spool.mark_completed(&cmd.payment_id, &folio)?;
                Ok(InjectPaymentAck {
                    outbox_id: cmd.outbox_id,
                    payment_id: cmd.payment_id,
                    status: i32::from(InjectionAckStatus::Committed),
                    legacy_receipt_folio: folio,
                    error_message: String::new(),
                })
            }
            Err(EdgeError::LegacyDbUnavailable(reason)) => {
                let _retries = self.spool.record_retry_failure(&cmd.payment_id, &reason)?;
                Ok(InjectPaymentAck {
                    outbox_id: cmd.outbox_id,
                    payment_id: cmd.payment_id,
                    status: i32::from(InjectionAckStatus::QueuedLocally),
                    legacy_receipt_folio: String::new(),
                    error_message: reason,
                })
            }
            Err(EdgeError::LegacyConflictPaidInWindow {
                external_tax_id,
                existing_folio,
            }) => Ok(InjectPaymentAck {
                outbox_id: cmd.outbox_id,
                payment_id: cmd.payment_id,
                status: i32::from(InjectionAckStatus::RejectedConflict),
                legacy_receipt_folio: existing_folio.clone(),
                error_message: format!(
                    "account '{external_tax_id}' already paid in window with folio '{existing_folio}'"
                ),
            }),
            Err(other_err) => Err(other_err),
        }
    }

    /// Drena todos los comandos pendientes en `redb` e inyecta los pagos en la base de datos
    /// municipal cuando esta vuelve a estar en línea.
    ///
    /// # Errors
    /// Devuelve [`EdgeError`] si ocurre un error de lectura/escritura en `redb` o en el adaptador.
    #[instrument(skip(self), fields(tenant = %self.tenant_id))]
    pub async fn flush_pending_spool(&self) -> Result<usize, EdgeError> {
        let pending_entries = self.spool.list_pending()?;
        let mut flushed_count: usize = 0;

        for entry in pending_entries {
            match self.adapter.apply_payment(&entry.command).await {
                Ok(folio) => {
                    self.spool
                        .mark_completed(&entry.command.payment_id, &folio)?;
                    flushed_count = flushed_count.saturating_add(1);
                }
                Err(EdgeError::LegacyDbUnavailable(reason)) => {
                    let _ = self
                        .spool
                        .record_retry_failure(&entry.command.payment_id, &reason)?;
                }
                Err(other_err) => return Err(other_err),
            }
        }

        Ok(flushed_count)
    }

    /// Construye un mensaje de telemetría [`AgentHeartbeat`] con el estado actual del spool y la DB.
    ///
    /// # Errors
    /// Devuelve [`EdgeError`] si falla la consulta al spool `redb`.
    pub async fn build_heartbeat(&self) -> Result<AgentHeartbeat, EdgeError> {
        let pending_spool_count = self.spool.pending_count()?;
        let legacy_db_reachable = self.adapter.health_check().await.is_ok();
        Ok(AgentHeartbeat {
            tenant_id: self.tenant_id.to_string(),
            agent_version: env!("CARGO_PKG_VERSION").to_owned(),
            pending_spool_count,
            legacy_db_reachable,
            timestamp_unix: Utc::now().timestamp(),
        })
    }
}

#[async_trait]
impl<A: LegacyMunicipalDbAdapter> MunicipalBridgePort for EdgeCoordinator<A> {
    async fn preflight_verify_debt(
        &self,
        tenant_id: &TenantId,
        external_id: &ExternalTaxId,
        expected_amount: MoneyCents,
    ) -> Result<bool, DomainError> {
        let req = PreflightDebtCheckRequest {
            request_id: Uuid::now_v7().to_string(),
            tenant_id: tenant_id.to_string(),
            department: "PropertyTax".to_owned(),
            external_tax_id: external_id.to_string(),
            expected_amount_cents: expected_amount.as_cents(),
        };
        let resp = self.handle_preflight_check(&req).await;
        Ok(resp.is_valid && !resp.is_already_paid_in_window)
    }

    async fn inject_payment_to_legacy(
        &self,
        tenant_id: &TenantId,
        external_id: &ExternalTaxId,
        payment_id: Uuid,
        idempotency_key: &IdempotencyKey,
        paid_amount: MoneyCents,
    ) -> Result<String, DomainError> {
        let now = Utc::now();
        let cmd = InjectPaymentCommand {
            outbox_id: Uuid::now_v7().to_string(),
            payment_id: payment_id.to_string(),
            tenant_id: tenant_id.to_string(),
            department: "PropertyTax".to_owned(),
            external_tax_id: external_id.to_string(),
            idempotency_key: idempotency_key.to_string(),
            amount_cents: paid_amount.as_cents(),
            fiscal_year: now.year(),
            paid_at_unix: now.timestamp(),
        };

        let ack = self.handle_inject_payment(cmd).await?;
        match ack.ack_status() {
            InjectionAckStatus::Committed => Ok(ack.legacy_receipt_folio),
            InjectionAckStatus::QueuedLocally => Err(DomainError::from(
                EdgeError::LegacyDbUnavailable(ack.error_message),
            )),
            InjectionAckStatus::RejectedConflict | InjectionAckStatus::Unspecified => {
                Err(DomainError::from(EdgeError::LegacyConflictPaidInWindow {
                    external_tax_id: external_id.to_string(),
                    existing_folio: ack.legacy_receipt_folio,
                }))
            }
        }
    }
}

