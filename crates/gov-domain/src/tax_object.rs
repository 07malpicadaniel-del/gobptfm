use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::DomainError;
use crate::identifiers::{CriCode, ExternalTaxId, TaxObjectId, TenantId};
use crate::money::MoneyCents;

/// Departamentos recaudadores municipales soportados por la plataforma.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DepartmentKind {
    /// Impuesto Predial Urbano / Rústico / Ejidal.
    PropertyTax,
    /// Derechos de Agua Potable, Drenaje y Saneamiento.
    WaterUtility,
    /// Licencias de Funcionamiento Comercial e Industrial.
    BusinessLicense,
    /// Multas e Infracciones de Tránsito y Vialidad.
    TrafficFine,
}

/// Beneficio o subsidio fiscal acreditado al padrón del contribuyente.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaxpayerBenefit {
    #[default]
    None,
    /// Adulto mayor con credencial INAPAM vigente.
    InapamSenior,
    /// Jubilado o pensionado.
    Pensioner,
    /// Viudez acreditada.
    Widow,
    /// Persona con discapacidad permanente.
    Disability,
}

/// Objeto imponible multi-departamento con protección de datos personales (LGPDPPSO).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaxObject {
    pub id: TaxObjectId,
    pub tenant_id: TenantId,
    pub department: DepartmentKind,
    pub external_id: ExternalTaxId,
    /// Hash SHA-256 de verificación del titular (ej. últimos dígitos de recibo anterior o RFC/CURP).
    pub owner_verification_hash: String,
    pub cadastral_or_base_value: MoneyCents,
    pub active_benefit: TaxpayerBenefit,
    pub metadata: serde_json::Value,
}

impl TaxObject {
    /// Verifica el hash de privacidad del titular antes de revelar desgloses sensibles.
    ///
    /// # Errors
    /// Devuelve [`DomainError::PrivacyVerificationFailed`] si el hash no coincide.
    pub fn verify_owner_hash(&self, candidate_hash: &str) -> Result<(), DomainError> {
        if self.owner_verification_hash.eq_ignore_ascii_case(candidate_hash.trim()) {
            Ok(())
        } else {
            Err(DomainError::PrivacyVerificationFailed)
        }
    }
}

/// Concepto individual dentro de un estado de cuenta o cotización fiscal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteLineItem {
    pub concept_code: String,
    pub description: String,
    pub cri_code: CriCode,
    pub amount: MoneyCents,
    pub is_discount: bool,
}

/// Cotización fiscal determinista lista para cobro en ventanilla digital o referencia bancaria.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaxQuote {
    pub quote_id: Uuid,
    pub tenant_id: TenantId,
    pub tax_object_id: TaxObjectId,
    pub fiscal_year: i32,
    pub items: Vec<QuoteLineItem>,
    pub subtotal: MoneyCents,
    pub total_discount: MoneyCents,
    pub total_to_pay: MoneyCents,
    pub expires_at: DateTime<Utc>,
}

impl TaxQuote {
    /// Construye y valida una cotización fiscal (`TaxQuote`) a partir de sus conceptos,
    /// calculando `subtotal`, `total_discount` y `total_to_pay`, y garantizando que:
    /// - Ningún concepto tenga monto negativo (`< 0`).
    /// - `subtotal - total_discount == total_to_pay`.
    /// - `total_to_pay >= MoneyCents::ZERO`.
    ///
    /// # Errors
    /// Devuelve [`DomainError::NegativeAmountNotAllowed`] o [`DomainError::ArithmeticOverflow`]
    /// si algún concepto es negativo o si el descuento excede el subtotal dejando un neto negativo.
    pub fn try_build(
        tenant_id: TenantId,
        tax_object_id: TaxObjectId,
        fiscal_year: i32,
        items: Vec<QuoteLineItem>,
        expires_at: DateTime<Utc>,
    ) -> Result<Self, DomainError> {
        let mut subtotal = MoneyCents::ZERO;
        let mut total_discount = MoneyCents::ZERO;

        for item in &items {
            if item.amount.is_negative() {
                return Err(DomainError::NegativeAmountNotAllowed(
                    item.amount.as_cents(),
                ));
            }
            if item.is_discount {
                total_discount = total_discount.checked_add(item.amount)?;
            } else {
                subtotal = subtotal.checked_add(item.amount)?;
            }
        }

        let net = subtotal.checked_sub(total_discount)?;
        let total_to_pay = MoneyCents::from_non_negative(net.as_cents())?;

        Ok(Self {
            quote_id: Uuid::now_v7(),
            tenant_id,
            tax_object_id,
            fiscal_year,
            items,
            subtotal,
            total_discount,
            total_to_pay,
            expires_at,
        })
    }

    /// Verifica que la cotización siga vigente en la marca de tiempo `now`.
    ///
    /// # Errors
    /// Devuelve [`DomainError::QuoteExpired`] si `now > self.expires_at`.
    pub fn ensure_not_expired(&self, now: DateTime<Utc>) -> Result<(), DomainError> {
        if now > self.expires_at {
            return Err(DomainError::QuoteExpired);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tax_quote_try_build_computes_exact_totals() -> Result<(), DomainError> {
        let tenant = TenantId::try_new("mun_boca_del_rio")?;
        let tax_obj_id = TaxObjectId::new_v7();
        let cri_predial = CriCode::try_new("1.2.01.01")?;
        let cri_adicional = CriCode::try_new("1.7.01.01")?;

        let base_amount = MoneyCents::from_cents(200_000); // $2,000.00
        let additional_amount = base_amount.apply_basis_points(1_500)?; // 15% = $300.00
        let inapam_discount = base_amount.apply_basis_points(5_000)?; // 50% = $1,000.00

        let items = vec![
            QuoteLineItem {
                concept_code: "PREDIAL_URBANO".to_owned(),
                description: "Impuesto Predial Urbano 2026".to_owned(),
                cri_code: cri_predial.clone(),
                amount: base_amount,
                is_discount: false,
            },
            QuoteLineItem {
                concept_code: "ADICIONAL_FOMENTO".to_owned(),
                description: "15% Adicional Fomento Educativo".to_owned(),
                cri_code: cri_adicional,
                amount: additional_amount,
                is_discount: false,
            },
            QuoteLineItem {
                concept_code: "SUBSIDIO_INAPAM".to_owned(),
                description: "Descuento 50% Adulto Mayor INAPAM".to_owned(),
                cri_code: cri_predial,
                amount: inapam_discount,
                is_discount: true,
            },
        ];

        let quote = TaxQuote::try_build(tenant, tax_obj_id, 2026, items, Utc::now())?;
        assert_eq!(quote.subtotal, MoneyCents::from_cents(230_000));
        assert_eq!(quote.total_discount, MoneyCents::from_cents(100_000));
        assert_eq!(quote.total_to_pay, MoneyCents::from_cents(130_000));
        assert_eq!(
            quote.subtotal.checked_sub(quote.total_discount)?,
            quote.total_to_pay
        );
        Ok(())
    }

    #[test]
    fn test_tax_quote_rejects_discount_exceeding_subtotal() -> Result<(), DomainError> {
        let tenant = TenantId::try_new("mun_boca_del_rio")?;
        let tax_obj_id = TaxObjectId::new_v7();
        let cri = CriCode::try_new("1.2.01.01")?;

        let items = vec![
            QuoteLineItem {
                concept_code: "PREDIAL".to_owned(),
                description: "Predial".to_owned(),
                cri_code: cri.clone(),
                amount: MoneyCents::from_cents(10_000),
                is_discount: false,
            },
            QuoteLineItem {
                concept_code: "DESC_EXCESIVO".to_owned(),
                description: "Descuento mayor al subtotal".to_owned(),
                cri_code: cri,
                amount: MoneyCents::from_cents(15_000),
                is_discount: true,
            },
        ];

        let res = TaxQuote::try_build(tenant, tax_obj_id, 2026, items, Utc::now());
        assert_eq!(res, Err(DomainError::NegativeAmountNotAllowed(-5_000)));
        Ok(())
    }
}
