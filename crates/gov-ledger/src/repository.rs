use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gov_domain::{
    AccountingMoment, DomainError, EntrySide, ExternalTaxId, IdempotencyKey, JournalEntry,
    LedgerRepositoryPort, MoneyCents, PaymentStatus, SyncOutboxStatus, TaxQuote, TenantId,
};
use sqlx::{PgPool, Row};
use tracing::instrument;
use uuid::Uuid;

use crate::error::LedgerError;
use crate::tenant_schema::set_local_tenant_search_path;

/// Registro reclamado de la cola transaccional `sync_outbox` para envío vía gRPC al municipio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxRecord {
    pub outbox_id: Uuid,
    pub tenant_id: TenantId,
    pub payment_id: Uuid,
    pub external_tax_id: ExternalTaxId,
    pub amount: MoneyCents,
    pub status: SyncOutboxStatus,
    pub attempt_count: i32,
    pub last_error: Option<String>,
    pub legacy_folio: Option<String>,
    pub next_attempt_at: DateTime<Utc>,
}

/// Valida las precondiciones de dominio y de máquina de estados antes de abrir una transacción de cobro.
///
/// # Errors
/// Devuelve [`LedgerError`] si la cotización expiró, la transición desde `Quoted` a `Authorized` falla,
/// el `tenant_id` no coincide o la póliza no corresponde al doble devengado/recaudado de la cotización.
pub fn validate_pre_authorization(
    quote: &TaxQuote,
    journal_entry: &JournalEntry,
    now: DateTime<Utc>,
) -> Result<PaymentStatus, LedgerError> {
    quote.ensure_not_expired(now)?;
    let next_status = PaymentStatus::Quoted.transition_to(PaymentStatus::Authorized)?;

    if quote.tenant_id != journal_entry.tenant_id {
        return Err(LedgerError::Domain(DomainError::InvalidTenantId(
            journal_entry.tenant_id.to_string(),
        )));
    }

    let expected_balanced = quote.total_to_pay.checked_mul_i64(2)?;
    let entry_balanced = journal_entry.total_balanced_amount()?;
    if entry_balanced != expected_balanced {
        return Err(LedgerError::Domain(DomainError::UnbalancedJournalEntry {
            total_debits: entry_balanced.as_cents(),
            total_credits: expected_balanced.as_cents(),
        }));
    }

    Ok(next_status)
}

/// Valida en memoria una transición de estado de pago antes de tocar la base de datos.
///
/// # Errors
/// Devuelve [`LedgerError::Domain`] con [`DomainError::InvalidStateTransition`] si la transición es ilegal.
pub fn validate_status_transition(
    current: PaymentStatus,
    next: PaymentStatus,
) -> Result<PaymentStatus, LedgerError> {
    Ok(current.transition_to(next)?)
}

fn parse_payment_status(raw: &str) -> Result<PaymentStatus, LedgerError> {
    match raw {
        "Quoted" => Ok(PaymentStatus::Quoted),
        "Authorized" => Ok(PaymentStatus::Authorized),
        "SyncedToMunicipality" => Ok(PaymentStatus::SyncedToMunicipality),
        "Reconciled" => Ok(PaymentStatus::Reconciled),
        "Failed" => Ok(PaymentStatus::Failed),
        "Reversed" => Ok(PaymentStatus::Reversed),
        other => Err(LedgerError::Domain(DomainError::InvalidStateTransition {
            from: other.to_owned(),
            to: "KnownPaymentStatus".to_owned(),
        })),
    }
}

fn parse_outbox_status(raw: &str) -> Result<SyncOutboxStatus, LedgerError> {
    match raw {
        "PendingSync" => Ok(SyncOutboxStatus::PendingSync),
        "InFlight" => Ok(SyncOutboxStatus::InFlight),
        "CommittedRemote" => Ok(SyncOutboxStatus::CommittedRemote),
        "ConflictManualReview" => Ok(SyncOutboxStatus::ConflictManualReview),
        other => Err(LedgerError::Domain(DomainError::InvalidStateTransition {
            from: other.to_owned(),
            to: "KnownSyncOutboxStatus".to_owned(),
        })),
    }
}

const fn moment_as_str(moment: AccountingMoment) -> &'static str {
    match moment {
        AccountingMoment::Estimated => "Estimated",
        AccountingMoment::Modified => "Modified",
        AccountingMoment::Accrued => "Accrued",
        AccountingMoment::Collected => "Collected",
    }
}

const fn side_as_str(side: EntrySide) -> &'static str {
    match side {
        EntrySide::Debit => "Debit",
        EntrySide::Credit => "Credit",
    }
}

const fn outbox_status_as_str(status: SyncOutboxStatus) -> &'static str {
    match status {
        SyncOutboxStatus::PendingSync => "PendingSync",
        SyncOutboxStatus::InFlight => "InFlight",
        SyncOutboxStatus::CommittedRemote => "CommittedRemote",
        SyncOutboxStatus::ConflictManualReview => "ConflictManualReview",
    }
}

/// Repositorio PostgreSQL transaccional para el motor contable CONAC y el patrón Transactional Outbox.
#[derive(Debug, Clone)]
pub struct PgLedgerRepository {
    pool: PgPool,
}

impl PgLedgerRepository {
    /// Crea una nueva instancia de [`PgLedgerRepository`].
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Devuelve una referencia al [`PgPool`] subyacente.
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Autoriza un pago de forma atómica e idempotente dentro de una transacción ACID:
    /// 1. Valida que la cotización no haya expirado y verifica la transición `Quoted -> Authorized`.
    /// 2. Verifica idempotencia por `(tenant_id, idempotency_key)` en `payments`.
    /// 3. Inserta `payments`, `payment_events` (append-only), `journal_entries` (append-only),
    ///    `ledger_lines` (append-only) y `sync_outbox` (`PendingSync`).
    ///
    /// # Errors
    /// Devuelve [`LedgerError`] si falla alguna validación de dominio o la transacción en PostgreSQL.
    #[instrument(skip(self, quote, journal_entry), fields(tenant = %quote.tenant_id, quote_id = %quote.quote_id))]
    pub async fn authorize_payment_atomic(
        &self,
        quote: &TaxQuote,
        idempotency_key: &IdempotencyKey,
        external_tax_id: &ExternalTaxId,
        journal_entry: &JournalEntry,
    ) -> Result<Uuid, LedgerError> {
        let now = Utc::now();
        let authorized_status = validate_pre_authorization(quote, journal_entry, now)?;

        let mut tx = self.pool.begin().await?;
        set_local_tenant_search_path(&mut tx, &quote.tenant_id).await?;

        // 1. Verificación idempotente por (tenant_id, idempotency_key)
        let existing_row = sqlx::query(
            "SELECT payment_id FROM payments WHERE tenant_id = $1 AND idempotency_key = $2",
        )
        .bind(quote.tenant_id.as_str())
        .bind(idempotency_key.as_str())
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(row) = existing_row {
            let existing_id: Uuid = row.try_get("payment_id")?;
            tx.commit().await?;
            return Ok(existing_id);
        }

        // 2. Asegurar persistencia de la cotización si aún no estaba persistida
        let items_json = serde_json::to_value(&quote.items)?;
        sqlx::query(
            "INSERT INTO tax_quotes (
                quote_id, tenant_id, tax_object_id, fiscal_year, items_json,
                subtotal_cents, total_discount_cents, total_to_pay_cents, expires_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT (quote_id) DO NOTHING",
        )
        .bind(quote.quote_id)
        .bind(quote.tenant_id.as_str())
        .bind(quote.tax_object_id.as_uuid())
        .bind(quote.fiscal_year)
        .bind(items_json)
        .bind(quote.subtotal.as_cents())
        .bind(quote.total_discount.as_cents())
        .bind(quote.total_to_pay.as_cents())
        .bind(quote.expires_at)
        .execute(&mut *tx)
        .await?;

        // 3. Insertar registro en payments
        let payment_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO payments (
                payment_id, tenant_id, quote_id, idempotency_key, amount_cents, status, created_at, updated_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $7)",
        )
        .bind(payment_id)
        .bind(quote.tenant_id.as_str())
        .bind(quote.quote_id)
        .bind(idempotency_key.as_str())
        .bind(quote.total_to_pay.as_cents())
        .bind(authorized_status.to_string())
        .bind(now)
        .execute(&mut *tx)
        .await?;

        // 4. Registrar evento inmutable en payment_events (APPEND-ONLY)
        let event_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO payment_events (
                event_id, payment_id, tenant_id, previous_status, new_status, reason, occurred_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(event_id)
        .bind(payment_id)
        .bind(quote.tenant_id.as_str())
        .bind(Some(PaymentStatus::Quoted.to_string()))
        .bind(authorized_status.to_string())
        .bind(Some("Pago autorizado vía webhook idempotente"))
        .bind(now)
        .execute(&mut *tx)
        .await?;

        // 5. Insertar cabecera de póliza en journal_entries (APPEND-ONLY)
        sqlx::query(
            "INSERT INTO journal_entries (
                entry_id, tenant_id, payment_id, idempotency_key, reverses_entry_id, description, created_at
            ) VALUES ($1, $2, $3, $4, NULL, $5, $6)",
        )
        .bind(journal_entry.id)
        .bind(journal_entry.tenant_id.as_str())
        .bind(Some(payment_id))
        .bind(journal_entry.idempotency_key.as_str())
        .bind(&journal_entry.description)
        .bind(journal_entry.created_at)
        .execute(&mut *tx)
        .await?;

        // 6. Insertar cada línea de partida doble en ledger_lines (APPEND-ONLY)
        for line in &journal_entry.lines {
            let line_id = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO ledger_lines (
                    line_id, entry_id, tenant_id, account_code, cri_code, moment, side, amount_cents
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(line_id)
            .bind(journal_entry.id)
            .bind(journal_entry.tenant_id.as_str())
            .bind(&line.account_code)
            .bind(line.cri_code.as_ref().map(gov_domain::CriCode::as_str))
            .bind(moment_as_str(line.moment))
            .bind(side_as_str(line.side))
            .bind(line.amount.as_cents())
            .execute(&mut *tx)
            .await?;
        }

        // 7. Encolar en sync_outbox con estado PendingSync
        let outbox_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO sync_outbox (
                outbox_id, tenant_id, payment_id, external_tax_id, amount_cents, status,
                attempt_count, next_attempt_at, created_at, updated_at
            ) VALUES ($1, $2, $3, $4, $5, $6, 0, $7, $7, $7)",
        )
        .bind(outbox_id)
        .bind(quote.tenant_id.as_str())
        .bind(payment_id)
        .bind(external_tax_id.as_str())
        .bind(quote.total_to_pay.as_cents())
        .bind(outbox_status_as_str(SyncOutboxStatus::PendingSync))
        .bind(now)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(payment_id)
    }

    /// Registra una reversión / contracargo (`PaymentStatus::Reversed`) mediante contra-asiento compensatorio:
    /// bloquea el pago con `SELECT ... FOR UPDATE`, valida la transición hacia `Reversed`,
    /// inserta en `payment_events` y agrega la contra-póliza en `journal_entries` y `ledger_lines`
    /// apuntando a `reverses_entry_id`, sin ejecutar jamás `UPDATE` ni `DELETE` en tablas contables.
    ///
    /// # Errors
    /// Devuelve [`LedgerError`] si el pago no existe, la transición es ilegal o falla la transacción.
    #[instrument(skip(self, reversal_entry), fields(tenant = %tenant_id, payment_id = %payment_id))]
    pub async fn reverse_payment_atomic(
        &self,
        tenant_id: &TenantId,
        payment_id: Uuid,
        reversal_entry: &JournalEntry,
    ) -> Result<Uuid, LedgerError> {
        if reversal_entry.tenant_id != *tenant_id {
            return Err(LedgerError::Domain(DomainError::InvalidTenantId(
                reversal_entry.tenant_id.to_string(),
            )));
        }
        let balanced = reversal_entry.total_balanced_amount()?;
        if !balanced.is_positive() {
            return Err(LedgerError::Domain(DomainError::NegativeAmountNotAllowed(
                balanced.as_cents(),
            )));
        }

        let mut tx = self.pool.begin().await?;
        set_local_tenant_search_path(&mut tx, tenant_id).await?;

        // Idempotencia sobre la contra-póliza
        let existing_reversal = sqlx::query(
            "SELECT entry_id FROM journal_entries WHERE tenant_id = $1 AND idempotency_key = $2",
        )
        .bind(tenant_id.as_str())
        .bind(reversal_entry.idempotency_key.as_str())
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(row) = existing_reversal {
            let entry_id: Uuid = row.try_get("entry_id")?;
            tx.commit().await?;
            return Ok(entry_id);
        }

        // Bloquear fila en payments con FOR UPDATE
        let payment_row = sqlx::query(
            "SELECT status FROM payments WHERE tenant_id = $1 AND payment_id = $2 FOR UPDATE",
        )
        .bind(tenant_id.as_str())
        .bind(payment_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(LedgerError::PaymentNotFound(payment_id))?;

        let current_status_str: String = payment_row.try_get("status")?;
        let current_status = parse_payment_status(&current_status_str)?;
        let reversed_status = validate_status_transition(current_status, PaymentStatus::Reversed)?;

        // Obtener entry_id original asociado al pago
        let orig_entry_row = sqlx::query(
            "SELECT entry_id FROM journal_entries
             WHERE tenant_id = $1 AND payment_id = $2 AND reverses_entry_id IS NULL
             ORDER BY created_at ASC LIMIT 1",
        )
        .bind(tenant_id.as_str())
        .bind(payment_id)
        .fetch_optional(&mut *tx)
        .await?;

        let original_entry_id: Option<Uuid> = match orig_entry_row {
            Some(row) => Some(row.try_get("entry_id")?),
            None => None,
        };

        let now = Utc::now();

        // Actualizar estado operativo en payments
        sqlx::query(
            "UPDATE payments SET status = $1, updated_at = $2 WHERE tenant_id = $3 AND payment_id = $4",
        )
        .bind(reversed_status.to_string())
        .bind(now)
        .bind(tenant_id.as_str())
        .bind(payment_id)
        .execute(&mut *tx)
        .await?;

        // Insertar evento inmutable en payment_events (APPEND-ONLY)
        sqlx::query(
            "INSERT INTO payment_events (
                event_id, payment_id, tenant_id, previous_status, new_status, reason, occurred_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(Uuid::now_v7())
        .bind(payment_id)
        .bind(tenant_id.as_str())
        .bind(Some(current_status.to_string()))
        .bind(reversed_status.to_string())
        .bind(Some(&reversal_entry.description))
        .bind(now)
        .execute(&mut *tx)
        .await?;

        // Insertar contra-póliza en journal_entries apuntando a reverses_entry_id (APPEND-ONLY)
        sqlx::query(
            "INSERT INTO journal_entries (
                entry_id, tenant_id, payment_id, idempotency_key, reverses_entry_id, description, created_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(reversal_entry.id)
        .bind(tenant_id.as_str())
        .bind(Some(payment_id))
        .bind(reversal_entry.idempotency_key.as_str())
        .bind(original_entry_id)
        .bind(&reversal_entry.description)
        .bind(reversal_entry.created_at)
        .execute(&mut *tx)
        .await?;

        for line in &reversal_entry.lines {
            sqlx::query(
                "INSERT INTO ledger_lines (
                    line_id, entry_id, tenant_id, account_code, cri_code, moment, side, amount_cents
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(Uuid::now_v7())
            .bind(reversal_entry.id)
            .bind(tenant_id.as_str())
            .bind(&line.account_code)
            .bind(line.cri_code.as_ref().map(gov_domain::CriCode::as_str))
            .bind(moment_as_str(line.moment))
            .bind(side_as_str(line.side))
            .bind(line.amount.as_cents())
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(reversal_entry.id)
    }

    /// Reclama un lote de registros `PendingSync` en `sync_outbox` usando `FOR UPDATE SKIP LOCKED`
    /// y los transiciona a `InFlight`.
    ///
    /// # Errors
    /// Devuelve [`LedgerError`] si `batch_size <= 0` o falla la consulta en PostgreSQL.
    #[instrument(skip(self), fields(tenant = %tenant_id, batch_size))]
    pub async fn claim_outbox_batch(
        &self,
        tenant_id: &TenantId,
        batch_size: i64,
    ) -> Result<Vec<OutboxRecord>, LedgerError> {
        if batch_size <= 0 {
            return Err(LedgerError::Domain(DomainError::NegativeAmountNotAllowed(
                batch_size,
            )));
        }

        let mut tx = self.pool.begin().await?;
        set_local_tenant_search_path(&mut tx, tenant_id).await?;

        let rows = sqlx::query(
            "WITH claimed AS (
                SELECT outbox_id
                FROM sync_outbox
                WHERE tenant_id = $1
                  AND status = 'PendingSync'
                  AND next_attempt_at <= NOW()
                ORDER BY outbox_id ASC
                LIMIT $2
                FOR UPDATE SKIP LOCKED
            )
            UPDATE sync_outbox o
            SET status = 'InFlight',
                attempt_count = o.attempt_count + 1,
                updated_at = NOW()
            FROM claimed c
            WHERE o.outbox_id = c.outbox_id
            RETURNING
                o.outbox_id,
                o.tenant_id,
                o.payment_id,
                o.external_tax_id,
                o.amount_cents,
                o.status,
                o.attempt_count,
                o.last_error,
                o.legacy_folio,
                o.next_attempt_at",
        )
        .bind(tenant_id.as_str())
        .bind(batch_size)
        .fetch_all(&mut *tx)
        .await?;

        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            let raw_tenant: String = row.try_get("tenant_id")?;
            let raw_ext_id: String = row.try_get("external_tax_id")?;
            let raw_cents: i64 = row.try_get("amount_cents")?;
            let raw_status: String = row.try_get("status")?;

            records.push(OutboxRecord {
                outbox_id: row.try_get("outbox_id")?,
                tenant_id: TenantId::try_new(raw_tenant)?,
                payment_id: row.try_get("payment_id")?,
                external_tax_id: ExternalTaxId::try_new(raw_ext_id)?,
                amount: MoneyCents::from_non_negative(raw_cents)?,
                status: parse_outbox_status(&raw_status)?,
                attempt_count: row.try_get("attempt_count")?,
                last_error: row.try_get("last_error")?,
                legacy_folio: row.try_get("legacy_folio")?,
                next_attempt_at: row.try_get("next_attempt_at")?,
            });
        }

        tx.commit().await?;
        Ok(records)
    }

    /// Marca un elemento del `sync_outbox` como `CommittedRemote` tras recibir el ACK del municipio,
    /// transicionando el pago `Authorized -> SyncedToMunicipality` y registrando el `legacy_folio`.
    ///
    /// # Errors
    /// Devuelve [`LedgerError`] si el pago no existe, su estado no permite pasar a `SyncedToMunicipality`,
    /// o falla la transacción en PostgreSQL.
    #[instrument(skip(self), fields(tenant = %tenant_id, payment_id = %payment_id, legacy_folio))]
    pub async fn mark_outbox_synced(
        &self,
        tenant_id: &TenantId,
        payment_id: Uuid,
        legacy_folio: &str,
    ) -> Result<(), LedgerError> {
        let mut tx = self.pool.begin().await?;
        set_local_tenant_search_path(&mut tx, tenant_id).await?;

        let payment_row = sqlx::query(
            "SELECT status FROM payments WHERE tenant_id = $1 AND payment_id = $2 FOR UPDATE",
        )
        .bind(tenant_id.as_str())
        .bind(payment_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(LedgerError::PaymentNotFound(payment_id))?;

        let current_status_str: String = payment_row.try_get("status")?;
        let current_status = parse_payment_status(&current_status_str)?;
        let synced_status =
            validate_status_transition(current_status, PaymentStatus::SyncedToMunicipality)?;

        let now = Utc::now();

        sqlx::query(
            "UPDATE payments
             SET status = $1, legacy_receipt_folio = $2, updated_at = $3
             WHERE tenant_id = $4 AND payment_id = $5",
        )
        .bind(synced_status.to_string())
        .bind(legacy_folio)
        .bind(now)
        .bind(tenant_id.as_str())
        .bind(payment_id)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO payment_events (
                event_id, payment_id, tenant_id, previous_status, new_status, reason, occurred_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(Uuid::now_v7())
        .bind(payment_id)
        .bind(tenant_id.as_str())
        .bind(Some(current_status.to_string()))
        .bind(synced_status.to_string())
        .bind(Some(format!("Sincronizado con sistema municipal folio {legacy_folio}")))
        .bind(now)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "UPDATE sync_outbox
             SET status = $1, legacy_folio = $2, updated_at = $3
             WHERE tenant_id = $4 AND payment_id = $5",
        )
        .bind(outbox_status_as_str(SyncOutboxStatus::CommittedRemote))
        .bind(legacy_folio)
        .bind(now)
        .bind(tenant_id.as_str())
        .bind(payment_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }
}

#[async_trait]
impl LedgerRepositoryPort for PgLedgerRepository {
    async fn record_journal_entry(&self, entry: &JournalEntry) -> Result<Uuid, DomainError> {
        let balanced = entry.total_balanced_amount()?;
        if !balanced.is_positive() {
            return Err(DomainError::NegativeAmountNotAllowed(balanced.as_cents()));
        }

        let mut tx = self.pool.begin().await.map_err(LedgerError::from)?;
        set_local_tenant_search_path(&mut tx, &entry.tenant_id).await?;

        let existing = sqlx::query(
            "SELECT entry_id FROM journal_entries WHERE tenant_id = $1 AND idempotency_key = $2",
        )
        .bind(entry.tenant_id.as_str())
        .bind(entry.idempotency_key.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(LedgerError::from)?;

        if let Some(row) = existing {
            let existing_id: Uuid = row.try_get("entry_id").map_err(LedgerError::from)?;
            tx.commit().await.map_err(LedgerError::from)?;
            return Ok(existing_id);
        }

        sqlx::query(
            "INSERT INTO journal_entries (
                entry_id, tenant_id, payment_id, idempotency_key, reverses_entry_id, description, created_at
            ) VALUES ($1, $2, NULL, $3, NULL, $4, $5)",
        )
        .bind(entry.id)
        .bind(entry.tenant_id.as_str())
        .bind(entry.idempotency_key.as_str())
        .bind(&entry.description)
        .bind(entry.created_at)
        .execute(&mut *tx)
        .await
        .map_err(LedgerError::from)?;

        for line in &entry.lines {
            sqlx::query(
                "INSERT INTO ledger_lines (
                    line_id, entry_id, tenant_id, account_code, cri_code, moment, side, amount_cents
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(Uuid::now_v7())
            .bind(entry.id)
            .bind(entry.tenant_id.as_str())
            .bind(&line.account_code)
            .bind(line.cri_code.as_ref().map(gov_domain::CriCode::as_str))
            .bind(moment_as_str(line.moment))
            .bind(side_as_str(line.side))
            .bind(line.amount.as_cents())
            .execute(&mut *tx)
            .await
            .map_err(LedgerError::from)?;
        }

        tx.commit().await.map_err(LedgerError::from)?;
        Ok(entry.id)
    }

    async fn transition_payment_status(
        &self,
        tenant_id: &TenantId,
        payment_id: Uuid,
        idempotency_key: &IdempotencyKey,
        next_status: PaymentStatus,
    ) -> Result<PaymentStatus, DomainError> {
        // Rechazar inmediatamente cualquier transición hacia Quoted sin tocar la base de datos
        if next_status == PaymentStatus::Quoted {
            return Err(DomainError::InvalidStateTransition {
                from: "Any".to_owned(),
                to: PaymentStatus::Quoted.to_string(),
            });
        }

        let mut tx = self.pool.begin().await.map_err(LedgerError::from)?;
        set_local_tenant_search_path(&mut tx, tenant_id).await?;

        let row = sqlx::query(
            "SELECT status FROM payments WHERE tenant_id = $1 AND payment_id = $2 FOR UPDATE",
        )
        .bind(tenant_id.as_str())
        .bind(payment_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(LedgerError::from)?
        .ok_or_else(|| DomainError::from(LedgerError::PaymentNotFound(payment_id)))?;

        let current_str: String = row.try_get("status").map_err(LedgerError::from)?;
        let current_status = parse_payment_status(&current_str)?;
        let transitioned = validate_status_transition(current_status, next_status)?;

        let now = Utc::now();
        sqlx::query(
            "UPDATE payments SET status = $1, updated_at = $2 WHERE tenant_id = $3 AND payment_id = $4",
        )
        .bind(transitioned.to_string())
        .bind(now)
        .bind(tenant_id.as_str())
        .bind(payment_id)
        .execute(&mut *tx)
        .await
        .map_err(LedgerError::from)?;

        sqlx::query(
            "INSERT INTO payment_events (
                event_id, payment_id, tenant_id, previous_status, new_status, reason, occurred_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(Uuid::now_v7())
        .bind(payment_id)
        .bind(tenant_id.as_str())
        .bind(Some(current_status.to_string()))
        .bind(transitioned.to_string())
        .bind(Some(format!("IdempotencyKey: {idempotency_key}")))
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(LedgerError::from)?;

        tx.commit().await.map_err(LedgerError::from)?;
        Ok(transitioned)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use gov_domain::{CriCode, QuoteLineItem, TaxObjectId};

    use crate::builder::build_collection_journal_entry;

    use super::*;

    #[test]
    fn test_repository_state_transitions_reject_invalid_jumps_before_db() {
        assert!(matches!(
            validate_status_transition(PaymentStatus::Quoted, PaymentStatus::Reconciled),
            Err(LedgerError::Domain(DomainError::InvalidStateTransition { .. }))
        ));
        assert!(matches!(
            validate_status_transition(
                PaymentStatus::Authorized,
                PaymentStatus::Reconciled
            ),
            Err(LedgerError::Domain(DomainError::InvalidStateTransition { .. }))
        ));
        assert!(matches!(
            validate_status_transition(PaymentStatus::Failed, PaymentStatus::Authorized),
            Err(LedgerError::Domain(DomainError::InvalidStateTransition { .. }))
        ));
        assert_eq!(
            validate_status_transition(
                PaymentStatus::Authorized,
                PaymentStatus::SyncedToMunicipality
            )
            .ok(),
            Some(PaymentStatus::SyncedToMunicipality)
        );
    }

    #[test]
    fn test_validate_pre_authorization_rejects_expired_quote_before_db() -> Result<(), DomainError>
    {
        let now = Utc::now();
        let tenant = TenantId::try_new("mun_xalapa")?;
        let cri = CriCode::try_new("1.2.01.01")?;
        let idem = IdempotencyKey::try_new("idem-expired-quote-0001")?;

        let items = vec![QuoteLineItem {
            concept_code: "PREDIAL".to_owned(),
            description: "Predial".to_owned(),
            cri_code: cri,
            amount: MoneyCents::from_cents(100_000),
            is_discount: false,
        }];

        let quote = TaxQuote::try_build(
            tenant.clone(),
            TaxObjectId::new_v7(),
            2026,
            items,
            now + Duration::minutes(5),
        )?;

        let entry = build_collection_journal_entry(
            tenant,
            idem,
            &quote,
            "1112-BANCOS",
            "1124-DEUDORES",
            "4112-INGRESOS",
            now,
        )?;

        // Simular que el intento de autorización ocurre 10 minutos después (expirado)
        let expired_check = validate_pre_authorization(&quote, &entry, now + Duration::minutes(10));
        assert!(matches!(
            expired_check,
            Err(LedgerError::Domain(DomainError::QuoteExpired))
        ));

        Ok(())
    }
}
