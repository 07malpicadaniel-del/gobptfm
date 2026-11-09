use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use gov_domain::{
    AccountingMoment, CriCode, DomainError, EntrySide, IdempotencyKey, JournalEntry, LedgerLine,
    MoneyCents, TaxQuote, TenantId,
};
use uuid::Uuid;

/// Construye automáticamente una póliza contable CONAC de partida doble para un cobro fiscal,
/// registrando simultáneamente los momentos contables **Devengado (`Accrued`)** y **Recaudado (`Collected`)**.
///
/// ### Estructura del Asiento Generado:
/// 1. **Momento `Accrued` (Devengado)**:
///    - **Cargo (`Debit`)** a `debtor_account_code` por `quote.total_to_pay`.
///    - **Abono (`Credit`)** a `{revenue_account_prefix}-{cri_code}` por el importe neto de cada `CriCode`.
/// 2. **Momento `Collected` (Recaudado)**:
///    - **Cargo (`Debit`)** a `bank_account_code` por `quote.total_to_pay`.
///    - **Abono (`Credit`)** a `debtor_account_code` por `quote.total_to_pay` (saldando la cuenta por cobrar).
///
/// El total de cargos y el total de abonos equivalen exactamente a `2 * quote.total_to_pay`.
///
/// # Errors
/// Devuelve [`DomainError`] si la cotización expiró, el monto a pagar no es positivo o las líneas no cuadran.
pub fn build_collection_journal_entry(
    tenant_id: TenantId,
    idempotency_key: IdempotencyKey,
    quote: &TaxQuote,
    bank_account_code: &str,
    debtor_account_code: &str,
    revenue_account_prefix: &str,
    now: DateTime<Utc>,
) -> Result<JournalEntry, DomainError> {
    quote.ensure_not_expired(now)?;

    if !quote.total_to_pay.is_positive() {
        return Err(DomainError::NegativeAmountNotAllowed(
            quote.total_to_pay.as_cents(),
        ));
    }

    // 1. Calcular el importe neto devengado por cada CRI (Cargos brutos menos Descuentos asociados).
    let mut net_by_cri: BTreeMap<CriCode, MoneyCents> = BTreeMap::new();
    let mut unallocated_discount = MoneyCents::ZERO;

    for item in &quote.items {
        if !item.is_discount {
            let current = net_by_cri
                .get(&item.cri_code)
                .copied()
                .unwrap_or(MoneyCents::ZERO);
            let updated = current.checked_add(item.amount)?;
            net_by_cri.insert(item.cri_code.clone(), updated);
        }
    }

    for item in &quote.items {
        if item.is_discount {
            let current = net_by_cri
                .get(&item.cri_code)
                .copied()
                .unwrap_or(MoneyCents::ZERO);
            if current >= item.amount {
                let updated = current.checked_sub(item.amount)?;
                net_by_cri.insert(item.cri_code.clone(), updated);
            } else {
                let leftover = item.amount.checked_sub(current)?;
                net_by_cri.insert(item.cri_code.clone(), MoneyCents::ZERO);
                unallocated_discount = unallocated_discount.checked_add(leftover)?;
            }
        }
    }

    // Si algún descuento general excedió su CRI específico, se absorbe de los rubros con saldo positivo.
    if unallocated_discount.is_positive() {
        for amount in net_by_cri.values_mut() {
            if unallocated_discount.is_zero() {
                break;
            }
            if *amount >= unallocated_discount {
                *amount = amount.checked_sub(unallocated_discount)?;
                unallocated_discount = MoneyCents::ZERO;
            } else {
                unallocated_discount = unallocated_discount.checked_sub(*amount)?;
                *amount = MoneyCents::ZERO;
            }
        }
    }

    let mut lines = Vec::with_capacity(net_by_cri.len() + 3);

    // a) Momento Accrued (Devengado): Cargo a Deudores Fiscales por el total neto a pagar.
    lines.push(LedgerLine::try_new(
        debtor_account_code,
        None,
        AccountingMoment::Accrued,
        EntrySide::Debit,
        quote.total_to_pay,
    )?);

    // Abonos a las cuentas de ingresos por cada CRI con importe neto > 0.
    for (cri_code, net_amount) in net_by_cri {
        if net_amount.is_positive() {
            let revenue_account = format!("{revenue_account_prefix}-{cri_code}");
            lines.push(LedgerLine::try_new(
                revenue_account,
                Some(cri_code),
                AccountingMoment::Accrued,
                EntrySide::Credit,
                net_amount,
            )?);
        }
    }

    // b) Momento Collected (Recaudado): Cargo a Bancos/Tesorería y Abono a Deudores Fiscales.
    lines.push(LedgerLine::try_new(
        bank_account_code,
        None,
        AccountingMoment::Collected,
        EntrySide::Debit,
        quote.total_to_pay,
    )?);

    lines.push(LedgerLine::try_new(
        debtor_account_code,
        None,
        AccountingMoment::Collected,
        EntrySide::Credit,
        quote.total_to_pay,
    )?);

    JournalEntry::try_with_metadata(
        Uuid::now_v7(),
        tenant_id,
        idempotency_key,
        now,
        format!(
            "Póliza CONAC Devengado/Recaudado cotización {} ejercicio {}",
            quote.quote_id, quote.fiscal_year
        ),
        lines,
    )
}

/// Construye una contra-póliza compensatoria (`Reversal`) invirtiendo matemáticamente cada línea
/// de la póliza original (`Debit` -> `Credit`, `Credit` -> `Debit`), cumpliendo con la prohibición
/// CONAC de ejecutar `UPDATE` o `DELETE` sobre el libro mayor.
///
/// # Errors
/// Devuelve [`DomainError`] si la póliza original es inválida o las líneas no cuadran.
pub fn build_reversal_journal_entry(
    original: &JournalEntry,
    reversal_idempotency_key: IdempotencyKey,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<JournalEntry, DomainError> {
    let mut reversed_lines = Vec::with_capacity(original.lines.len());

    for line in &original.lines {
        let inverted_side = match line.side {
            EntrySide::Debit => EntrySide::Credit,
            EntrySide::Credit => EntrySide::Debit,
        };
        reversed_lines.push(LedgerLine::try_new(
            line.account_code.clone(),
            line.cri_code.clone(),
            line.moment,
            inverted_side,
            line.amount,
        )?);
    }

    JournalEntry::try_with_metadata(
        Uuid::now_v7(),
        original.tenant_id.clone(),
        reversal_idempotency_key,
        now,
        format!("REVERSAL [{}]: {}", original.id, reason),
        reversed_lines,
    )
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use gov_domain::{QuoteLineItem, TaxObjectId};

    use super::*;

    fn sample_quote(now: DateTime<Utc>) -> Result<(TenantId, TaxQuote), DomainError> {
        let tenant = TenantId::try_new("mun_veracruz")?;
        let tax_object_id = TaxObjectId::new_v7();
        let cri_predial = CriCode::try_new("1.2.01.01")?;
        let cri_adicional = CriCode::try_new("1.7.01.01")?;

        let base_predial = MoneyCents::from_cents(250_000); // $2,500.00
        let adicional = base_predial.apply_basis_points(1_500)?; // 15% = $375.00
        let pronto_pago = base_predial.apply_basis_points(2_000)?; // 20% = $500.00

        let items = vec![
            QuoteLineItem {
                concept_code: "PREDIAL_2026".to_owned(),
                description: "Impuesto Predial Urbano 2026".to_owned(),
                cri_code: cri_predial.clone(),
                amount: base_predial,
                is_discount: false,
            },
            QuoteLineItem {
                concept_code: "ADICIONAL_15".to_owned(),
                description: "Adicional 15% Fomento".to_owned(),
                cri_code: cri_adicional,
                amount: adicional,
                is_discount: false,
            },
            QuoteLineItem {
                concept_code: "PRONTO_PAGO_20".to_owned(),
                description: "Descuento 20% Pronto Pago".to_owned(),
                cri_code: cri_predial,
                amount: pronto_pago,
                is_discount: true,
            },
        ];

        let quote = TaxQuote::try_build(
            tenant.clone(),
            tax_object_id,
            2026,
            items,
            now + Duration::hours(24),
        )?;
        Ok((tenant, quote))
    }

    #[test]
    fn test_build_collection_journal_entry_generates_balanced_accrued_and_collected(
    ) -> Result<(), DomainError> {
        let now = Utc::now();
        let (tenant, quote) = sample_quote(now)?;
        let idem = IdempotencyKey::try_new("idem-collect-2026-00001")?;

        // total_to_pay = $2,500 + $375 - $500 = $2,375.00 (237_500 cents)
        assert_eq!(quote.total_to_pay, MoneyCents::from_cents(237_500));

        let entry = build_collection_journal_entry(
            tenant,
            idem,
            &quote,
            "1112-BANCOS-TESORERIA",
            "1124-DEUDORES-FISCALES",
            "4112-INGRESOS",
            now,
        )?;

        // Total cargos y abonos deben ser exactamente 2 * total_to_pay (Devengado + Recaudado)
        let expected_double = quote.total_to_pay.checked_mul_i64(2)?;
        assert_eq!(entry.total_balanced_amount()?, expected_double);

        let accrued_debits: i64 = entry
            .lines
            .iter()
            .filter(|l| l.moment == AccountingMoment::Accrued && l.side == EntrySide::Debit)
            .map(|l| l.amount.as_cents())
            .sum();
        let accrued_credits: i64 = entry
            .lines
            .iter()
            .filter(|l| l.moment == AccountingMoment::Accrued && l.side == EntrySide::Credit)
            .map(|l| l.amount.as_cents())
            .sum();
        let collected_debits: i64 = entry
            .lines
            .iter()
            .filter(|l| l.moment == AccountingMoment::Collected && l.side == EntrySide::Debit)
            .map(|l| l.amount.as_cents())
            .sum();
        let collected_credits: i64 = entry
            .lines
            .iter()
            .filter(|l| l.moment == AccountingMoment::Collected && l.side == EntrySide::Credit)
            .map(|l| l.amount.as_cents())
            .sum();

        assert_eq!(accrued_debits, quote.total_to_pay.as_cents());
        assert_eq!(accrued_credits, quote.total_to_pay.as_cents());
        assert_eq!(collected_debits, quote.total_to_pay.as_cents());
        assert_eq!(collected_credits, quote.total_to_pay.as_cents());
        Ok(())
    }

    #[test]
    fn test_build_reversal_journal_entry_nets_all_accounts_to_zero() -> Result<(), DomainError> {
        let now = Utc::now();
        let (tenant, quote) = sample_quote(now)?;
        let idem_orig = IdempotencyKey::try_new("idem-collect-2026-00002")?;
        let idem_rev = IdempotencyKey::try_new("idem-reverse-2026-00002")?;

        let original = build_collection_journal_entry(
            tenant,
            idem_orig,
            &quote,
            "1112-BANCOS-TESORERIA",
            "1124-DEUDORES-FISCALES",
            "4112-INGRESOS",
            now,
        )?;

        let reversal = build_reversal_journal_entry(
            &original,
            idem_rev,
            "Contracargo bancario SPEI devuelto",
            now,
        )?;

        assert_eq!(original.lines.len(), reversal.lines.len());
        assert_eq!(
            original.total_balanced_amount()?,
            reversal.total_balanced_amount()?
        );

        // Sumar el efecto neto (Cargo = +monto, Abono = -monto) por cuenta y momento entre original + reversa
        let mut net_by_account: BTreeMap<(String, Option<CriCode>), MoneyCents> = BTreeMap::new();

        for line in original.lines.iter().chain(reversal.lines.iter()) {
            let key = (line.account_code.clone(), line.cri_code.clone());
            let current = net_by_account
                .get(&key)
                .copied()
                .unwrap_or(MoneyCents::ZERO);
            let next = match line.side {
                EntrySide::Debit => current.checked_add(line.amount)?,
                EntrySide::Credit => current.checked_sub(line.amount)?,
            };
            net_by_account.insert(key, next);
        }

        for net_balance in net_by_account.values() {
            assert_eq!(*net_balance, MoneyCents::ZERO);
        }

        Ok(())
    }
}

