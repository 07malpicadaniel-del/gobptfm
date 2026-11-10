use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};

use chrono::Utc;
use gov_domain::{
    DepartmentKind, DomainError, ExternalTaxId, IdempotencyKey, JournalEntry, MoneyCents,
    MunicipalBridgePort, PaymentStatus, TaxObject, TaxObjectId, TaxQuote, TaxRuleEnginePort,
    TenantId,
};
use gov_ledger::build_collection_journal_entry;
use gov_receipt::{bytes_to_hex, ReceiptPayload, ReceiptSigner, SignedOfficialReceipt};
use gov_tax_engine::RhaiTaxRuleEngine;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::dto::{
    AuthorizePaymentResponse, AuthorizePaymentWebhookRequest, CreateQuoteRequest,
    CreateQuoteResponse,
};
use crate::error::ApiError;

/// Calcula el hash SHA-256 normalizado (`trim` + `UPPERCASE`) del secreto de verificación del contribuyente
/// (ej. últimos dígitos de recibo anterior o RFC/CURP) para cumplir con la LGPDPPSO sin exponer PII.
#[must_use]
pub fn hash_verification_secret(secret: &str) -> String {
    let normalized: String = secret.trim().chars().flat_map(char::to_uppercase).collect();
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    let digest = hasher.finalize();
    bytes_to_hex(&digest)
}

#[derive(Debug, Default)]
struct TransactionalMemoryStore {
    objects_by_ext: HashMap<(TenantId, DepartmentKind, ExternalTaxId), TaxObject>,
    objects_by_id: HashMap<TaxObjectId, TaxObject>,
    quotes_by_id: HashMap<Uuid, TaxQuote>,
    journal_entries: Vec<JournalEntry>,
    payments_by_idempotency: HashMap<(TenantId, IdempotencyKey), AuthorizePaymentResponse>,
    receipts_by_payment: HashMap<(TenantId, Uuid), SignedOfficialReceipt>,
}

/// Servicio coordinador hexagonal en la nube (`gov-core-api`).
pub struct CorePaymentService {
    tax_engine: Arc<RhaiTaxRuleEngine>,
    receipt_signer: Arc<ReceiptSigner>,
    bridge: Arc<dyn MunicipalBridgePort>,
    store: RwLock<TransactionalMemoryStore>,
}

impl std::fmt::Debug for CorePaymentService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CorePaymentService")
            .field("tax_engine", &self.tax_engine)
            .field("receipt_signer", &self.receipt_signer)
            .finish_non_exhaustive()
    }
}

impl CorePaymentService {
    /// Construye una nueva instancia de [`CorePaymentService`].
    #[must_use]
    pub fn new(
        tax_engine: Arc<RhaiTaxRuleEngine>,
        receipt_signer: Arc<ReceiptSigner>,
        bridge: Arc<dyn MunicipalBridgePort>,
    ) -> Self {
        Self {
            tax_engine,
            receipt_signer,
            bridge,
            store: RwLock::new(TransactionalMemoryStore::default()),
        }
    }

    /// Registra un [`TaxObject`] en el padrón operativo.
    pub fn register_tax_object(&self, tax_object: TaxObject) {
        let mut guard = self.store.write().unwrap_or_else(PoisonError::into_inner);
        let key = (
            tax_object.tenant_id.clone(),
            tax_object.department,
            tax_object.external_id.clone(),
        );
        guard.objects_by_id.insert(tax_object.id, tax_object.clone());
        guard.objects_by_ext.insert(key, tax_object);
    }

    /// Devuelve cuántas pólizas contables CONAC se han registrado en total.
    #[must_use]
    pub fn journal_entries_count(&self) -> usize {
        let guard = self.store.read().unwrap_or_else(PoisonError::into_inner);
        guard.journal_entries.len()
    }

    /// Evalúa y persiste una cotización fiscal tras validar el hash de privacidad del titular.
    ///
    /// # Errors
    /// Devuelve [`ApiError`] si el identificador es inválido, el predio no existe, el secreto de
    /// privacidad no coincide (`403 FORBIDDEN`) o falla la regla fiscal.
    pub async fn create_quote(
        &self,
        tenant_id: TenantId,
        req: CreateQuoteRequest,
    ) -> Result<CreateQuoteResponse, ApiError> {
        let external_id = ExternalTaxId::try_new(&req.external_tax_id)?;
        let uma_value = MoneyCents::from_non_negative(req.uma_daily_cents)?;

        let tax_object = {
            let guard = self.store.read().unwrap_or_else(PoisonError::into_inner);
            let key = (tenant_id.clone(), req.department, external_id.clone());
            guard.objects_by_ext.get(&key).cloned()
        }
        .ok_or_else(|| ApiError::TaxObjectNotFound {
            tenant_id: tenant_id.to_string(),
            external_id: external_id.to_string(),
        })?;

        // Verificación de privacidad LGPDPPSO
        let candidate_hash = hash_verification_secret(&req.verification_secret);
        tax_object.verify_owner_hash(&candidate_hash)?;

        let quote = self
            .tax_engine
            .evaluate_quote(&tax_object, req.fiscal_year, uma_value, req.payment_date)
            .await?;

        {
            let mut guard = self.store.write().unwrap_or_else(PoisonError::into_inner);
            guard.quotes_by_id.insert(quote.quote_id, quote.clone());
        }

        Ok(CreateQuoteResponse { quote })
    }

    /// Procesa un webhook bancario de forma idempotente:
    /// 1. Si `(tenant_id, idempotency_key)` ya existe, devuelve la respuesta original sin re-ejecutar efectos.
    /// 2. Verifica monto exacto y vigencia de la cotización.
    /// 3. Ejecuta preflight contra `MunicipalBridgePort`.
    /// 4. Genera la póliza CONAC de partida doble (`Accrued` + `Collected`).
    /// 5. Inyecta el pago en el municipio y firma el recibo oficial con Ed25519.
    ///
    /// # Errors
    /// Devuelve [`ApiError`] ante violación de monto, expiración, conflicto en ventanilla o error de dominio.
    pub async fn authorize_payment_webhook(
        &self,
        tenant_id: TenantId,
        req: AuthorizePaymentWebhookRequest,
    ) -> Result<AuthorizePaymentResponse, ApiError> {
        let idempotency_key = IdempotencyKey::try_new(req.idempotency_key)?;

        // 1. Verificación rápida de idempotencia
        {
            let guard = self.store.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(existing) = guard
                .payments_by_idempotency
                .get(&(tenant_id.clone(), idempotency_key.clone()))
            {
                return Ok(existing.clone());
            }
        }

        // 2. Obtener cotización y objeto imponible
        let (quote, tax_object) = {
            let guard = self.store.read().unwrap_or_else(PoisonError::into_inner);
            let q = guard
                .quotes_by_id
                .get(&req.quote_id)
                .cloned()
                .ok_or(ApiError::QuoteNotFound(req.quote_id))?;

            if q.tenant_id != tenant_id {
                return Err(ApiError::QuoteNotFound(req.quote_id));
            }

            let obj = guard
                .objects_by_id
                .get(&q.tax_object_id)
                .cloned()
                .ok_or_else(|| ApiError::TaxObjectNotFound {
                    tenant_id: tenant_id.to_string(),
                    external_id: q.tax_object_id.to_string(),
                })?;
            (q, obj)
        };

        let now = Utc::now();
        quote.ensure_not_expired(now)?;

        // 3. Verificar coincidencia exacta al centavo
        if req.paid_amount_cents != quote.total_to_pay.as_cents() {
            return Err(ApiError::PaymentAmountMismatch {
                expected_cents: quote.total_to_pay.as_cents(),
                paid_cents: req.paid_amount_cents,
            });
        }

        // 4. Preflight check hacia el agente municipal
        let preflight_ok = self
            .bridge
            .preflight_verify_debt(&tenant_id, &tax_object.external_id, quote.total_to_pay)
            .await?;

        if !preflight_ok {
            return Err(ApiError::PreflightConflict(format!(
                "preflight rejected for account '{}'",
                tax_object.external_id
            )));
        }

        let authorized_status = PaymentStatus::Quoted.transition_to(PaymentStatus::Authorized)?;

        // 5. Construir póliza contable CONAC de partida doble (Accrued + Collected)
        let journal_entry = build_collection_journal_entry(
            tenant_id.clone(),
            idempotency_key.clone(),
            &quote,
            "1112-BANCOS-TESORERIA",
            "1124-DEUDORES-FISCALES",
            "4112-INGRESOS",
            now,
        )?;

        let payment_id = Uuid::now_v7();

        // 6. Inyectar pago en el sistema legacy municipal (o en su cola local redb si el DB local está en mantenimiento)
        let legacy_folio = match self
            .bridge
            .inject_payment_to_legacy(
                &tenant_id,
                &tax_object.external_id,
                payment_id,
                &idempotency_key,
                quote.total_to_pay,
            )
            .await
        {
            Ok(folio) => folio,
            Err(DomainError::InvalidStateTransition { from, .. }) if from == "LegacyDbOffline" => {
                format!("LOCAL-SPOOL-{payment_id}")
            }
            Err(other_err) => return Err(ApiError::Domain(other_err)),
        };

        let final_status =
            authorized_status.transition_to(PaymentStatus::SyncedToMunicipality)?;

        // 7. Emitir y firmar criptográficamente el recibo oficial Ed25519
        let receipt_payload = ReceiptPayload {
            receipt_id: Uuid::now_v7(),
            payment_id,
            quote_id: quote.quote_id,
            tenant_id: tenant_id.clone(),
            department: tax_object.department,
            external_tax_id: tax_object.external_id,
            fiscal_year: quote.fiscal_year,
            items: quote.items,
            subtotal: quote.subtotal,
            total_discount: quote.total_discount,
            total_paid: quote.total_to_pay,
            legacy_folio,
            issued_at_unix: now.timestamp(),
        };
        let signed_receipt = self.receipt_signer.sign_receipt(receipt_payload);

        let response = AuthorizePaymentResponse {
            payment_id,
            status: final_status,
            journal_entry: journal_entry.clone(),
            receipt: signed_receipt.clone(),
        };

        // 8. Persistir de forma atómica bajo el lock de escritura
        let mut guard = self.store.write().unwrap_or_else(PoisonError::into_inner);
        let idem_pair = (tenant_id.clone(), idempotency_key);
        if let Some(already_stored) = guard.payments_by_idempotency.get(&idem_pair) {
            return Ok(already_stored.clone());
        }

        guard.journal_entries.push(journal_entry);
        guard
            .receipts_by_payment
            .insert((tenant_id, payment_id), signed_receipt);
        guard
            .payments_by_idempotency
            .insert(idem_pair, response.clone());

        Ok(response)
    }

    /// Obtiene un [`SignedOfficialReceipt`] previamente emitido para `(tenant_id, payment_id)`.
    ///
    /// # Errors
    /// Devuelve [`ApiError::ReceiptNotFound`] si el recibo no existe.
    pub fn get_signed_receipt(
        &self,
        tenant_id: &TenantId,
        payment_id: Uuid,
    ) -> Result<SignedOfficialReceipt, ApiError> {
        let guard = self.store.read().unwrap_or_else(PoisonError::into_inner);
        guard
            .receipts_by_payment
            .get(&(tenant_id.clone(), payment_id))
            .cloned()
            .ok_or(ApiError::ReceiptNotFound(payment_id))
    }
}

/// Estado compartido inyectado en el `axum::Router`.
#[derive(Debug, Clone)]
pub struct AppState {
    pub service: Arc<CorePaymentService>,
}

impl AppState {
    #[must_use]
    pub const fn new(service: Arc<CorePaymentService>) -> Self {
        Self { service }
    }
}
