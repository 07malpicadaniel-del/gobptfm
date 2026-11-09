-- =============================================================================
-- Migration: 0001_init_ledger_and_outbox.sql
-- Purpose:   CONAC Append-Only Double-Entry Ledger, Payment State Machine,
--            and Transactional Outbox for Municipal Edge Synchronization.
-- =============================================================================

-- 1. Inmutabilidad Contable CONAC (Trigger Append-Only)
CREATE OR REPLACE FUNCTION prevent_append_only_mutation()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'CONAC_IMMUTABILITY_VIOLATION: table % is append-only; use compensating journal entries', TG_TABLE_NAME;
END;
$$;

-- 2. Padrón Multi-Departamento de Objetos Imponibles
CREATE TABLE IF NOT EXISTS tax_objects (
    id UUID PRIMARY KEY,
    tenant_id VARCHAR(32) NOT NULL,
    department VARCHAR(32) NOT NULL,
    external_id VARCHAR(64) NOT NULL,
    owner_verification_hash TEXT NOT NULL,
    cadastral_or_base_value_cents BIGINT NOT NULL CHECK (cadastral_or_base_value_cents >= 0),
    active_benefit VARCHAR(32) NOT NULL,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT uq_tax_objects_tenant_dept_ext UNIQUE (tenant_id, department, external_id)
);

-- 3. Cotizaciones Fiscales Deterministas
CREATE TABLE IF NOT EXISTS tax_quotes (
    quote_id UUID PRIMARY KEY,
    tenant_id VARCHAR(32) NOT NULL,
    tax_object_id UUID NOT NULL REFERENCES tax_objects(id),
    fiscal_year INT NOT NULL,
    items_json JSONB NOT NULL,
    subtotal_cents BIGINT NOT NULL,
    total_discount_cents BIGINT NOT NULL,
    total_to_pay_cents BIGINT NOT NULL CHECK (total_to_pay_cents >= 0),
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_tax_quotes_tenant_object
    ON tax_quotes (tenant_id, tax_object_id);

-- 4. Pagos y Máquina de Estados
CREATE TABLE IF NOT EXISTS payments (
    payment_id UUID PRIMARY KEY,
    tenant_id VARCHAR(32) NOT NULL,
    quote_id UUID NOT NULL UNIQUE REFERENCES tax_quotes(quote_id),
    idempotency_key VARCHAR(128) NOT NULL,
    amount_cents BIGINT NOT NULL CHECK (amount_cents >= 0),
    status VARCHAR(32) NOT NULL,
    legacy_receipt_folio VARCHAR(128),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT uq_payments_tenant_idempotency UNIQUE (tenant_id, idempotency_key)
);

-- 5. Bitácora Inmutable de Eventos de Pago (APPEND-ONLY)
CREATE TABLE IF NOT EXISTS payment_events (
    event_id UUID PRIMARY KEY,
    payment_id UUID NOT NULL REFERENCES payments(payment_id),
    tenant_id VARCHAR(32) NOT NULL,
    previous_status VARCHAR(32),
    new_status VARCHAR(32) NOT NULL,
    reason TEXT,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_payment_events_payment_id
    ON payment_events (payment_id, occurred_at);

DROP TRIGGER IF EXISTS trg_payment_events_append_only ON payment_events;
CREATE TRIGGER trg_payment_events_append_only
BEFORE UPDATE OR DELETE ON payment_events
FOR EACH ROW
EXECUTE FUNCTION prevent_append_only_mutation();

-- 6. Cabecera de Pólizas Contables de Diario (APPEND-ONLY)
CREATE TABLE IF NOT EXISTS journal_entries (
    entry_id UUID PRIMARY KEY,
    tenant_id VARCHAR(32) NOT NULL,
    payment_id UUID REFERENCES payments(payment_id),
    idempotency_key VARCHAR(128) NOT NULL,
    reverses_entry_id UUID REFERENCES journal_entries(entry_id),
    description TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT uq_journal_entries_tenant_idempotency UNIQUE (tenant_id, idempotency_key)
);

CREATE INDEX IF NOT EXISTS idx_journal_entries_payment_id
    ON journal_entries (payment_id);

DROP TRIGGER IF EXISTS trg_journal_entries_append_only ON journal_entries;
CREATE TRIGGER trg_journal_entries_append_only
BEFORE UPDATE OR DELETE ON journal_entries
FOR EACH ROW
EXECUTE FUNCTION prevent_append_only_mutation();

-- 7. Líneas de Partida Doble y Armonización Contable CONAC (APPEND-ONLY)
CREATE TABLE IF NOT EXISTS ledger_lines (
    line_id UUID PRIMARY KEY,
    entry_id UUID NOT NULL REFERENCES journal_entries(entry_id),
    tenant_id VARCHAR(32) NOT NULL,
    account_code VARCHAR(64) NOT NULL,
    cri_code VARCHAR(32),
    moment VARCHAR(32) NOT NULL,
    side VARCHAR(16) NOT NULL CHECK (side IN ('Debit', 'Credit')),
    amount_cents BIGINT NOT NULL CHECK (amount_cents > 0)
);

CREATE INDEX IF NOT EXISTS idx_ledger_lines_entry_id
    ON ledger_lines (entry_id);

CREATE INDEX IF NOT EXISTS idx_ledger_lines_tenant_account
    ON ledger_lines (tenant_id, account_code);

DROP TRIGGER IF EXISTS trg_ledger_lines_append_only ON ledger_lines;
CREATE TRIGGER trg_ledger_lines_append_only
BEFORE UPDATE OR DELETE ON ledger_lines
FOR EACH ROW
EXECUTE FUNCTION prevent_append_only_mutation();

-- 8. Cola Transaccional de Salida hacia el Municipio (Transactional Outbox)
CREATE TABLE IF NOT EXISTS sync_outbox (
    outbox_id UUID PRIMARY KEY,
    tenant_id VARCHAR(32) NOT NULL,
    payment_id UUID NOT NULL UNIQUE REFERENCES payments(payment_id),
    external_tax_id VARCHAR(64) NOT NULL,
    amount_cents BIGINT NOT NULL CHECK (amount_cents >= 0),
    status VARCHAR(32) NOT NULL,
    attempt_count INT NOT NULL DEFAULT 0,
    last_error TEXT,
    legacy_folio VARCHAR(128),
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Índice parcial de sub-milisegundo para despacho gRPC de pendientes
CREATE INDEX IF NOT EXISTS idx_sync_outbox_pending_dispatch
    ON sync_outbox (tenant_id, next_attempt_at)
    WHERE status = 'PendingSync';

