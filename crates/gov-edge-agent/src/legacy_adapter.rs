use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{PoisonError, RwLock};

use async_trait::async_trait;
use chrono::Utc;
use gov_domain::{DepartmentKind, ExternalTaxId, MoneyCents, TaxpayerBenefit};
use gov_proto::InjectPaymentCommand;
use serde::{Deserialize, Serialize};

use crate::error::EdgeError;

/// Registro de cuenta en el padrón legacy municipal (SQL Server / Oracle / DBF).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyAccountRecord {
    pub external_tax_id: ExternalTaxId,
    pub department: DepartmentKind,
    pub owner_verification_hash: String,
    pub cadastral_value: MoneyCents,
    pub current_debt: MoneyCents,
    pub active_benefit: TaxpayerBenefit,
    pub is_paid_current_year: bool,
    pub paid_folio: Option<String>,
    pub updated_at_unix: i64,
}

/// Puerto adaptador hacia la base de datos legacy local del ayuntamiento.
#[async_trait]
pub trait LegacyMunicipalDbAdapter: Send + Sync {
    /// Verifica conectividad con el motor de base de datos municipal.
    async fn health_check(&self) -> Result<(), EdgeError>;

    /// Obtiene el estado actual de una cuenta en el padrón municipal.
    async fn fetch_account(
        &self,
        external_tax_id: &ExternalTaxId,
    ) -> Result<LegacyAccountRecord, EdgeError>;

    /// Aplica un pago en el sistema legacy municipal y retorna el folio local asignado.
    async fn apply_payment(&self, cmd: &InjectPaymentCommand) -> Result<String, EdgeError>;

    /// Obtiene las cuentas modificadas en el padrón local desde `since_unix`.
    async fn fetch_modified_since(
        &self,
        since_unix: i64,
    ) -> Result<Vec<LegacyAccountRecord>, EdgeError>;
}

/// Adaptador en memoria con soporte para simulación de apagones, caídas de red y pagos en ventanilla física.
#[derive(Debug)]
pub struct InMemoryLegacyAdapter {
    online: AtomicBool,
    seq: AtomicU64,
    applied_mutations: AtomicU64,
    accounts: RwLock<HashMap<ExternalTaxId, LegacyAccountRecord>>,
    applied_by_payment_id: RwLock<HashMap<String, String>>,
}

impl Default for InMemoryLegacyAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryLegacyAdapter {
    /// Crea un nuevo [`InMemoryLegacyAdapter`] inicialmente en línea (`online = true`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            online: AtomicBool::new(true),
            seq: AtomicU64::new(0),
            applied_mutations: AtomicU64::new(0),
            accounts: RwLock::new(HashMap::new()),
            applied_by_payment_id: RwLock::new(HashMap::new()),
        }
    }

    /// Simula el encendido o apagado del servidor SQL Server / Oracle municipal.
    pub fn set_online(&self, is_online: bool) {
        self.online.store(is_online, Ordering::SeqCst);
    }

    /// Indica si el adaptador se encuentra en línea.
    #[must_use]
    pub fn is_online(&self) -> bool {
        self.online.load(Ordering::SeqCst)
    }

    /// Devuelve cuántas veces se ha aplicado efectivamente un pago sobre el padrón legacy.
    #[must_use]
    pub fn applied_mutations_count(&self) -> u64 {
        self.applied_mutations.load(Ordering::SeqCst)
    }

    /// Inserta o actualiza una cuenta en el padrón legacy simulado.
    pub fn upsert_account(&self, record: LegacyAccountRecord) {
        let mut guard = self.accounts.write().unwrap_or_else(PoisonError::into_inner);
        guard.insert(record.external_tax_id.clone(), record);
    }

    /// Simula que un ciudadano acaba de pagar presencialmente en una caja física del palacio municipal.
    ///
    /// # Errors
    /// Devuelve [`EdgeError::LegacyAccountNotFound`] si la cuenta no existe.
    pub fn simulate_physical_window_payment(
        &self,
        external_tax_id: &ExternalTaxId,
        window_folio: impl Into<String>,
    ) -> Result<(), EdgeError> {
        let mut guard = self.accounts.write().unwrap_or_else(PoisonError::into_inner);
        let account = guard
            .get_mut(external_tax_id)
            .ok_or_else(|| EdgeError::LegacyAccountNotFound(external_tax_id.to_string()))?;

        account.is_paid_current_year = true;
        account.current_debt = MoneyCents::ZERO;
        account.paid_folio = Some(window_folio.into());
        account.updated_at_unix = Utc::now().timestamp();
        Ok(())
    }

    fn ensure_online(&self) -> Result<(), EdgeError> {
        if self.is_online() {
            Ok(())
        } else {
            Err(EdgeError::LegacyDbUnavailable(
                "municipal SQL Server instance is unreachable (simulated power/network outage)"
                    .to_owned(),
            ))
        }
    }
}

#[async_trait]
impl LegacyMunicipalDbAdapter for InMemoryLegacyAdapter {
    async fn health_check(&self) -> Result<(), EdgeError> {
        self.ensure_online()
    }

    async fn fetch_account(
        &self,
        external_tax_id: &ExternalTaxId,
    ) -> Result<LegacyAccountRecord, EdgeError> {
        self.ensure_online()?;
        let guard = self.accounts.read().unwrap_or_else(PoisonError::into_inner);
        guard
            .get(external_tax_id)
            .cloned()
            .ok_or_else(|| EdgeError::LegacyAccountNotFound(external_tax_id.to_string()))
    }

    async fn apply_payment(&self, cmd: &InjectPaymentCommand) -> Result<String, EdgeError> {
        self.ensure_online()?;

        // Idempotencia a nivel de DB legacy por payment_id
        {
            let applied_guard = self
                .applied_by_payment_id
                .read()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(existing_folio) = applied_guard.get(&cmd.payment_id) {
                return Ok(existing_folio.clone());
            }
        }

        let ext_id = ExternalTaxId::try_new(&cmd.external_tax_id)?;
        let mut accounts_guard = self.accounts.write().unwrap_or_else(PoisonError::into_inner);
        let account = accounts_guard
            .get_mut(&ext_id)
            .ok_or_else(|| EdgeError::LegacyAccountNotFound(ext_id.to_string()))?;

        if account.is_paid_current_year {
            let existing_folio = account
                .paid_folio
                .clone()
                .unwrap_or_else(|| "CAJA-VENTANILLA-SIN-FOLIO".to_owned());
            return Err(EdgeError::LegacyConflictPaidInWindow {
                external_tax_id: ext_id.to_string(),
                existing_folio,
            });
        }

        let next_seq = self.seq.fetch_add(1, Ordering::SeqCst).saturating_add(1);
        let folio = format!(
            "LEGACY-FOLIO-{}-{}-{next_seq}",
            cmd.tenant_id, cmd.fiscal_year
        );

        account.is_paid_current_year = true;
        account.current_debt = MoneyCents::ZERO;
        account.paid_folio = Some(folio.clone());
        account.updated_at_unix = cmd.paid_at_unix;

        drop(accounts_guard);

        let mut applied_guard = self
            .applied_by_payment_id
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        applied_guard.insert(cmd.payment_id.clone(), folio.clone());
        self.applied_mutations.fetch_add(1, Ordering::SeqCst);

        Ok(folio)
    }

    async fn fetch_modified_since(
        &self,
        since_unix: i64,
    ) -> Result<Vec<LegacyAccountRecord>, EdgeError> {
        self.ensure_online()?;
        let guard = self.accounts.read().unwrap_or_else(PoisonError::into_inner);
        let modified = guard
            .values()
            .filter(|acc| acc.updated_at_unix >= since_unix)
            .cloned()
            .collect();
        Ok(modified)
    }
}

