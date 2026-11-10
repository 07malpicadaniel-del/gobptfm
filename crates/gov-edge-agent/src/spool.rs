use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use gov_proto::InjectPaymentCommand;
use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

use crate::error::EdgeError;

/// Tabla `redb` de inyecciones pendientes hacia el sistema legacy municipal (`payment_id -> SpoolEntry JSON`).
pub const PENDING_INJECTIONS: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("pending_injections");

/// Tabla `redb` de inyecciones confirmadas (`payment_id -> legacy_receipt_folio`).
pub const COMPLETED_INJECTIONS: TableDefinition<'static, &str, &str> =
    TableDefinition::new("completed_injections");

/// Registro persistente en el spool local a prueba de apagones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpoolEntry {
    pub command: InjectPaymentCommand,
    pub retry_count: u32,
    pub last_error: Option<String>,
    pub queued_at_unix: i64,
}

fn map_redb_err(err: impl std::fmt::Display) -> EdgeError {
    EdgeError::SpoolStorageError(err.to_string())
}

/// Cola transaccional persistente en disco basada en `redb` para resistir apagones o caídas
/// del servidor SQL Server / Oracle / DBF local del municipio.
#[derive(Debug, Clone)]
pub struct LocalPaymentSpool {
    db: Arc<Database>,
}

impl LocalPaymentSpool {
    /// Abre o crea la base de datos `redb` en `path` e inicializa ambas tablas dentro de una
    /// transacción de escritura ACID inicial.
    ///
    /// # Errors
    /// Devuelve [`EdgeError::SpoolStorageError`] si no puede abrirse el archivo o crear las tablas.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, EdgeError> {
        let db = Database::create(path.as_ref()).map_err(map_redb_err)?;
        let write_tx = db.begin_write().map_err(map_redb_err)?;
        {
            let _pending = write_tx.open_table(PENDING_INJECTIONS).map_err(map_redb_err)?;
            let _completed = write_tx
                .open_table(COMPLETED_INJECTIONS)
                .map_err(map_redb_err)?;
        }
        write_tx.commit().map_err(map_redb_err)?;

        Ok(Self { db: Arc::new(db) })
    }

    /// Consulta si `payment_id` ya fue inyectado exitosamente en la base de datos municipal.
    ///
    /// # Errors
    /// Devuelve [`EdgeError::SpoolStorageError`] si ocurre un error de lectura en `redb`.
    pub fn get_completed_folio(&self, payment_id: &str) -> Result<Option<String>, EdgeError> {
        let read_tx = self.db.begin_read().map_err(map_redb_err)?;
        let table = read_tx
            .open_table(COMPLETED_INJECTIONS)
            .map_err(map_redb_err)?;
        let maybe_guard = table.get(payment_id).map_err(map_redb_err)?;
        Ok(maybe_guard.map(|g| g.value().to_owned()))
    }

    /// Encola de forma durable un [`InjectPaymentCommand`] en `PENDING_INJECTIONS`.
    /// Si ya existe en `COMPLETED_INJECTIONS`, no lo encola y retorna `Ok(false)`.
    ///
    /// # Errors
    /// Devuelve [`EdgeError`] si falla la serialización o la transacción en `redb`.
    pub fn enqueue_pending(&self, cmd: &InjectPaymentCommand) -> Result<bool, EdgeError> {
        let write_tx = self.db.begin_write().map_err(map_redb_err)?;
        let inserted = {
            let completed = write_tx
                .open_table(COMPLETED_INJECTIONS)
                .map_err(map_redb_err)?;
            if completed.get(cmd.payment_id.as_str()).map_err(map_redb_err)?.is_some() {
                false
            } else {
                let mut pending = write_tx
                    .open_table(PENDING_INJECTIONS)
                    .map_err(map_redb_err)?;
                let already_pending = pending
                    .get(cmd.payment_id.as_str())
                    .map_err(map_redb_err)?
                    .is_some();

                if already_pending {
                    false
                } else {
                    let entry = SpoolEntry {
                        command: cmd.clone(),
                        retry_count: 0,
                        last_error: None,
                        queued_at_unix: Utc::now().timestamp(),
                    };
                    let bytes = serde_json::to_vec(&entry)
                        .map_err(|e| EdgeError::SerializationError(e.to_string()))?;
                    pending
                        .insert(cmd.payment_id.as_str(), bytes.as_slice())
                        .map_err(map_redb_err)?;
                    true
                }
            }
        };
        write_tx.commit().map_err(map_redb_err)?;
        Ok(inserted)
    }

    /// En una **única transacción ACID de `redb`**, elimina `payment_id` de `PENDING_INJECTIONS`
    /// e inserta `(payment_id, legacy_folio)` en `COMPLETED_INJECTIONS`.
    ///
    /// # Errors
    /// Devuelve [`EdgeError::SpoolStorageError`] si falla la transacción en disco.
    pub fn mark_completed(&self, payment_id: &str, legacy_folio: &str) -> Result<(), EdgeError> {
        let write_tx = self.db.begin_write().map_err(map_redb_err)?;
        {
            let mut pending = write_tx
                .open_table(PENDING_INJECTIONS)
                .map_err(map_redb_err)?;
            let mut completed = write_tx
                .open_table(COMPLETED_INJECTIONS)
                .map_err(map_redb_err)?;

            pending.remove(payment_id).map_err(map_redb_err)?;
            completed
                .insert(payment_id, legacy_folio)
                .map_err(map_redb_err)?;
        }
        write_tx.commit().map_err(map_redb_err)?;
        Ok(())
    }

    /// Incrementa `retry_count` y persiste `last_error` para un pago en `PENDING_INJECTIONS`.
    ///
    /// # Errors
    /// Devuelve [`EdgeError`] si la entrada no existe o falla la escritura en `redb`.
    pub fn record_retry_failure(
        &self,
        payment_id: &str,
        error_msg: &str,
    ) -> Result<u32, EdgeError> {
        let write_tx = self.db.begin_write().map_err(map_redb_err)?;
        let updated_count = {
            let mut pending = write_tx
                .open_table(PENDING_INJECTIONS)
                .map_err(map_redb_err)?;
            let existing_bytes = {
                let guard = pending
                    .get(payment_id)
                    .map_err(map_redb_err)?
                    .ok_or_else(|| {
                        EdgeError::SpoolStorageError(format!(
                            "pending payment '{payment_id}' not found in spool"
                        ))
                    })?;
                guard.value().to_vec()
            };

            let mut entry: SpoolEntry = serde_json::from_slice(&existing_bytes)
                .map_err(|e| EdgeError::SerializationError(e.to_string()))?;
            entry.retry_count = entry.retry_count.saturating_add(1);
            entry.last_error = Some(error_msg.to_owned());

            let encoded = serde_json::to_vec(&entry)
                .map_err(|e| EdgeError::SerializationError(e.to_string()))?;
            pending
                .insert(payment_id, encoded.as_slice())
                .map_err(map_redb_err)?;
            entry.retry_count
        };
        write_tx.commit().map_err(map_redb_err)?;
        Ok(updated_count)
    }

    /// Lista todos los [`SpoolEntry`] actualmente pendientes en el spool local.
    ///
    /// # Errors
    /// Devuelve [`EdgeError`] si falla la lectura o deserialización.
    pub fn list_pending(&self) -> Result<Vec<SpoolEntry>, EdgeError> {
        let read_tx = self.db.begin_read().map_err(map_redb_err)?;
        let table = read_tx
            .open_table(PENDING_INJECTIONS)
            .map_err(map_redb_err)?;

        let mut entries = Vec::new();
        for item in table.iter().map_err(map_redb_err)? {
            let (_key_guard, val_guard) = item.map_err(map_redb_err)?;
            let entry: SpoolEntry = serde_json::from_slice(val_guard.value())
                .map_err(|e| EdgeError::SerializationError(e.to_string()))?;
            entries.push(entry);
        }
        Ok(entries)
    }

    /// Devuelve la cantidad exacta de pagos pendientes de inyectar en `PENDING_INJECTIONS`.
    ///
    /// # Errors
    /// Devuelve [`EdgeError::SpoolStorageError`] si falla la consulta en `redb`.
    pub fn pending_count(&self) -> Result<u64, EdgeError> {
        let read_tx = self.db.begin_read().map_err(map_redb_err)?;
        let table = read_tx
            .open_table(PENDING_INJECTIONS)
            .map_err(map_redb_err)?;
        let mut count: u64 = 0;
        for item in table.iter().map_err(map_redb_err)? {
            let _ = item.map_err(map_redb_err)?;
            count = count.saturating_add(1);
        }
        Ok(count)
    }
}

