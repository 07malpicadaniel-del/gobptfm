use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::DomainError;
use crate::identifiers::{CriCode, IdempotencyKey, TenantId};
use crate::money::MoneyCents;

/// Momentos contables de los ingresos conforme a la LGCG y normatividad CONAC.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AccountingMoment {
    /// Ley de Ingresos Estimada (8110).
    Estimated,
    /// Ley de Ingresos Modificada (8130).
    Modified,
    /// Ley de Ingresos Devengada (8140).
    Accrued,
    /// Ley de Ingresos Recaudada (8150).
    Collected,
}

/// Naturaleza del movimiento en la partida doble.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EntrySide {
    /// Cargo (Debe).
    Debit,
    /// Abono (Haber).
    Credit,
}

/// Línea individual de una póliza contable (asiento de diario).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerLine {
    pub account_code: String,
    pub cri_code: Option<CriCode>,
    pub moment: AccountingMoment,
    pub side: EntrySide,
    pub amount: MoneyCents,
}

impl LedgerLine {
    /// Crea una línea contable validando que el monto sea estrictamente positivo (`> 0`).
    ///
    /// # Errors
    /// Devuelve [`DomainError::NegativeAmountNotAllowed`] si `amount.as_cents() <= 0`.
    pub fn try_new(
        account_code: impl Into<String>,
        cri_code: Option<CriCode>,
        moment: AccountingMoment,
        side: EntrySide,
        amount: MoneyCents,
    ) -> Result<Self, DomainError> {
        if !amount.is_positive() {
            return Err(DomainError::NegativeAmountNotAllowed(amount.as_cents()));
        }
        Ok(Self {
            account_code: account_code.into(),
            cri_code,
            moment,
            side,
            amount,
        })
    }
}

/// Póliza contable inmutable (Asiento de Diario) con garantía matemática de partida doble.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub id: Uuid,
    pub tenant_id: TenantId,
    pub idempotency_key: IdempotencyKey,
    pub created_at: DateTime<Utc>,
    pub description: String,
    pub lines: Vec<LedgerLine>,
}

impl JournalEntry {
    /// Construye un nuevo [`JournalEntry`] generando automáticamente `Uuid::now_v7()` y `Utc::now()`,
    /// verificando el invariante de partida doble al centavo.
    ///
    /// # Errors
    /// - [`DomainError::EmptyJournalEntry`] si `lines` está vacío.
    /// - [`DomainError::NegativeAmountNotAllowed`] si alguna línea tiene un monto `<= 0`.
    /// - [`DomainError::ArithmeticOverflow`] si la suma de cargos o abonos desborda `i64`.
    /// - [`DomainError::UnbalancedJournalEntry`] si la suma total de cargos difiere de la suma de abonos.
    pub fn try_new(
        tenant_id: TenantId,
        idempotency_key: IdempotencyKey,
        description: impl Into<String>,
        lines: Vec<LedgerLine>,
    ) -> Result<Self, DomainError> {
        Self::try_with_metadata(
            Uuid::now_v7(),
            tenant_id,
            idempotency_key,
            Utc::now(),
            description,
            lines,
        )
    }

    /// Construye un [`JournalEntry`] con `id` y `created_at` explícitos, validando la partida doble.
    ///
    /// # Errors
    /// Retorna error si las líneas están vacías, contienen montos `<= 0` o no cuadran cargos y abonos.
    pub fn try_with_metadata(
        id: Uuid,
        tenant_id: TenantId,
        idempotency_key: IdempotencyKey,
        created_at: DateTime<Utc>,
        description: impl Into<String>,
        lines: Vec<LedgerLine>,
    ) -> Result<Self, DomainError> {
        if lines.is_empty() {
            return Err(DomainError::EmptyJournalEntry);
        }

        let mut total_debits = MoneyCents::ZERO;
        let mut total_credits = MoneyCents::ZERO;

        for line in &lines {
            if !line.amount.is_positive() {
                return Err(DomainError::NegativeAmountNotAllowed(
                    line.amount.as_cents(),
                ));
            }
            match line.side {
                EntrySide::Debit => {
                    total_debits = total_debits.checked_add(line.amount)?;
                }
                EntrySide::Credit => {
                    total_credits = total_credits.checked_add(line.amount)?;
                }
            }
        }

        if total_debits != total_credits {
            return Err(DomainError::UnbalancedJournalEntry {
                total_debits: total_debits.as_cents(),
                total_credits: total_credits.as_cents(),
            });
        }

        Ok(Self {
            id,
            tenant_id,
            idempotency_key,
            created_at,
            description: description.into(),
            lines,
        })
    }

    /// Devuelve el total cuadrado de la póliza (suma de cargos == suma de abonos).
    ///
    /// # Errors
    /// Devuelve [`DomainError::ArithmeticOverflow`] en caso de desbordamiento.
    pub fn total_balanced_amount(&self) -> Result<MoneyCents, DomainError> {
        let mut debits = MoneyCents::ZERO;
        for line in &self.lines {
            if line.side == EntrySide::Debit {
                debits = debits.checked_add(line.amount)?;
            }
        }
        Ok(debits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_context() -> Result<(TenantId, IdempotencyKey, CriCode), DomainError> {
        let tenant = TenantId::try_new("mun_xalapa")?;
        let idem = IdempotencyKey::try_new("idem-webhook-20261109-0001")?;
        let cri = CriCode::try_new("1.2.01.01")?;
        Ok((tenant, idem, cri))
    }

    #[test]
    fn test_journal_entry_accepts_balanced_multi_line_conac_entry() -> Result<(), DomainError> {
        let (tenant, idem, cri) = sample_context()?;
        let amount = MoneyCents::from_cents(185_050); // $1,850.50 MXN

        // Simultaneous Accrued + Collected CONAC double-entry
        let lines = vec![
            LedgerLine::try_new(
                "1112-BANCOS-TESORERIA",
                None,
                AccountingMoment::Collected,
                EntrySide::Debit,
                amount,
            )?,
            LedgerLine::try_new(
                "1124-INGRESOS-POR-RECUPERAR",
                Some(cri.clone()),
                AccountingMoment::Collected,
                EntrySide::Credit,
                amount,
            )?,
            LedgerLine::try_new(
                "8150-LEY-INGRESOS-RECAUDADA",
                Some(cri.clone()),
                AccountingMoment::Collected,
                EntrySide::Debit,
                amount,
            )?,
            LedgerLine::try_new(
                "8140-LEY-INGRESOS-DEVENGADA",
                Some(cri),
                AccountingMoment::Accrued,
                EntrySide::Credit,
                amount,
            )?,
        ];

        let entry = JournalEntry::try_new(
            tenant,
            idem,
            "Recaudación Impuesto Predial Urbano 2026",
            lines,
        )?;
        assert_eq!(
            entry.total_balanced_amount()?,
            MoneyCents::from_cents(370_100)
        );
        Ok(())
    }

    #[test]
    fn test_journal_entry_rejects_unbalanced_by_single_cent() -> Result<(), DomainError> {
        let (tenant, idem, cri) = sample_context()?;
        let lines = vec![
            LedgerLine::try_new(
                "1112-BANCOS",
                None,
                AccountingMoment::Collected,
                EntrySide::Debit,
                MoneyCents::from_cents(10_000),
            )?,
            LedgerLine::try_new(
                "4112-IMPUESTOS-PATRIMONIO",
                Some(cri),
                AccountingMoment::Collected,
                EntrySide::Credit,
                MoneyCents::from_cents(9_999),
            )?,
        ];

        let result = JournalEntry::try_new(tenant, idem, "Póliza descuadrada por 1 centavo", lines);
        assert_eq!(
            result,
            Err(DomainError::UnbalancedJournalEntry {
                total_debits: 10_000,
                total_credits: 9_999,
            })
        );
        Ok(())
    }

    #[test]
    fn test_journal_entry_rejects_empty_or_zero_lines() -> Result<(), DomainError> {
        let (tenant, idem, _cri) = sample_context()?;
        assert_eq!(
            JournalEntry::try_new(tenant.clone(), idem.clone(), "Vacía", vec![]),
            Err(DomainError::EmptyJournalEntry)
        );

        let zero_line = LedgerLine {
            account_code: "1112".to_owned(),
            cri_code: None,
            moment: AccountingMoment::Collected,
            side: EntrySide::Debit,
            amount: MoneyCents::ZERO,
        };
        assert_eq!(
            JournalEntry::try_new(tenant, idem, "Cero", vec![zero_line]),
            Err(DomainError::NegativeAmountNotAllowed(0))
        );
        Ok(())
    }
}
