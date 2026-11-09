//! Deterministic sandboxed fiscal rules evaluator (Rhai / AST) for municipal tax calculation.

pub mod engine;
pub mod error;
pub mod presets;
pub mod sandbox;

pub use engine::{CompiledRule, RhaiTaxRuleEngine, RuleKey};
pub use error::TaxEngineError;
pub use presets::standard_property_tax_script;
pub use sandbox::build_sandboxed_engine;

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use gov_domain::{
        DepartmentKind, DomainError, ExternalTaxId, MoneyCents, TaxObject, TaxObjectId,
        TaxRuleEnginePort, TaxpayerBenefit, TenantId,
    };
    use serde_json::json;

    use super::*;

    /// Valor UMA diaria de prueba: `$113.14 MXN` (`11_314` centavos).
    /// Tope de 10,000 UMAs = `$1,131,400.00 MXN` (`113_140_000` centavos).
    const SAMPLE_UMA_CENTS: i64 = 11_314;

    fn build_engine_with_preset() -> Result<(RhaiTaxRuleEngine, TenantId), TaxEngineError> {
        let engine = RhaiTaxRuleEngine::new();
        let tenant = TenantId::try_new("mun_veracruz")?;
        engine.register_rule(
            tenant.clone(),
            DepartmentKind::PropertyTax,
            2026,
            standard_property_tax_script(),
            30,
        )?;
        Ok((engine, tenant))
    }

    fn build_tax_object(
        tenant: TenantId,
        cadastral_cents: i64,
        benefit: TaxpayerBenefit,
    ) -> Result<TaxObject, DomainError> {
        Ok(TaxObject {
            id: TaxObjectId::new_v7(),
            tenant_id: tenant,
            department: DepartmentKind::PropertyTax,
            external_id: ExternalTaxId::try_new("30-193-001-01-0100")?,
            owner_verification_hash: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                .to_owned(),
            cadastral_or_base_value: MoneyCents::from_non_negative(cadastral_cents)?,
            active_benefit: benefit,
            metadata: json!({ "zone": "urbana", "surface_m2": 220 }),
        })
    }

    #[tokio::test]
    async fn test_case_1_pronto_pago_january_20_percent() -> Result<(), TaxEngineError> {
        let (engine, tenant) = build_engine_with_preset()?;
        // Predio de $1,200,000.00 MXN (120_000_000 centavos) sin beneficio especial
        let tax_object = build_tax_object(tenant, 120_000_000, TaxpayerBenefit::None)?;
        let uma = MoneyCents::from_non_negative(SAMPLE_UMA_CENTS)?;
        let jan_15 = NaiveDate::from_ymd_opt(2026, 1, 15).ok_or_else(|| {
            TaxEngineError::InvalidScriptOutput("invalid test date".to_owned())
        })?;

        let quote = engine
            .evaluate_quote(&tax_object, 2026, uma, jan_15)
            .await?;

        // Base Predial (0.25% = 25 bps de $1,200,000.00) = $3,000.00 (300_000 centavos)
        // Adicional 15% (1500 bps de $3,000.00) = $450.00 (45_000 centavos)
        // Subtotal = $3,450.00 (345_000 centavos)
        // Descuento Pronto Pago 20% (2000 bps de $3,000.00) = $600.00 (60_000 centavos)
        // Total a Pagar = $2,850.00 (285_000 centavos)
        assert_eq!(quote.subtotal, MoneyCents::from_cents(345_000));
        assert_eq!(quote.total_discount, MoneyCents::from_cents(60_000));
        assert_eq!(quote.total_to_pay, MoneyCents::from_cents(285_000));
        assert_eq!(quote.items.len(), 3);
        Ok(())
    }

    #[test]
    fn test_case_2_inapam_50_percent_within_uma_cap() -> Result<(), TaxEngineError> {
        let (engine, tenant) = build_engine_with_preset()?;
        // Predio de $800,000.00 MXN (80_000_000 centavos) <= 10,000 UMAs ($1,131,400.00 MXN)
        let tax_object = build_tax_object(tenant, 80_000_000, TaxpayerBenefit::InapamSenior)?;
        let uma = MoneyCents::from_non_negative(SAMPLE_UMA_CENTS)?;
        let jan_20 = NaiveDate::from_ymd_opt(2026, 1, 20).ok_or_else(|| {
            TaxEngineError::InvalidScriptOutput("invalid test date".to_owned())
        })?;

        let quote = engine.evaluate_tax_quote(&tax_object, 2026, uma, jan_20)?;

        // Base Predial (25 bps de $800,000.00) = $2,000.00 (200_000 centavos)
        // Adicional 15% = $300.00 (30_000 centavos)
        // Subtotal = $2,300.00 (230_000 centavos)
        // Descuento INAPAM 50% sobre predial base = $1,000.00 (100_000 centavos)
        // Total a Pagar = $1,300.00 (130_000 centavos)
        assert_eq!(quote.subtotal, MoneyCents::from_cents(230_000));
        assert_eq!(quote.total_discount, MoneyCents::from_cents(100_000));
        assert_eq!(quote.total_to_pay, MoneyCents::from_cents(130_000));
        assert!(quote
            .items
            .iter()
            .any(|i| i.concept_code == "DESCUENTO_BENEFICIO_SOCIAL_50" && i.is_discount));
        Ok(())
    }

    #[test]
    fn test_case_3_inapam_exceeding_10000_uma_cap_degrades_to_20_percent_pronto_pago(
    ) -> Result<(), TaxEngineError> {
        let (engine, tenant) = build_engine_with_preset()?;
        // Mansión de $2,000,000.00 MXN (200_000_000 centavos) > 10,000 UMAs ($1,131,400.00 MXN)
        let tax_object = build_tax_object(tenant, 200_000_000, TaxpayerBenefit::InapamSenior)?;
        let uma = MoneyCents::from_non_negative(SAMPLE_UMA_CENTS)?;
        let jan_10 = NaiveDate::from_ymd_opt(2026, 1, 10).ok_or_else(|| {
            TaxEngineError::InvalidScriptOutput("invalid test date".to_owned())
        })?;

        let quote = engine.evaluate_tax_quote(&tax_object, 2026, uma, jan_10)?;

        // Base Predial (25 bps de $2,000,000.00) = $5,000.00 (500_000 centavos)
        // Adicional 15% = $750.00 (75_000 centavos)
        // Subtotal = $5,750.00 (575_000 centavos)
        // Degrada a 20% Pronto Pago sobre base ($1,000.00 = 100_000 centavos) en lugar del 50%
        // Total a Pagar = $4,750.00 (475_000 centavos)
        assert_eq!(quote.subtotal, MoneyCents::from_cents(575_000));
        assert_eq!(quote.total_discount, MoneyCents::from_cents(100_000));
        assert_eq!(quote.total_to_pay, MoneyCents::from_cents(475_000));
        assert!(quote
            .items
            .iter()
            .any(|i| i.concept_code == "DESCUENTO_PRONTO_PAGO_20" && i.is_discount));
        Ok(())
    }

    #[test]
    fn test_case_4_moratory_surcharges_in_june() -> Result<(), TaxEngineError> {
        let (engine, tenant) = build_engine_with_preset()?;
        // Predio de $1,200,000.00 MXN pagado en junio (mes 6 -> 4 meses de mora = 600 bps = 6%)
        let tax_object = build_tax_object(tenant, 120_000_000, TaxpayerBenefit::None)?;
        let uma = MoneyCents::from_non_negative(SAMPLE_UMA_CENTS)?;
        let june_10 = NaiveDate::from_ymd_opt(2026, 6, 10).ok_or_else(|| {
            TaxEngineError::InvalidScriptOutput("invalid test date".to_owned())
        })?;

        let quote = engine.evaluate_tax_quote(&tax_object, 2026, uma, june_10)?;

        // Base Predial = $3,000.00 (300_000 centavos)
        // Adicional 15% = $450.00 (45_000 centavos)
        // Recargos (4 meses * 150 bps = 600 bps de $3,000.00) = $180.00 (18_000 centavos)
        // Subtotal = $3,630.00 (363_000 centavos), Descuento = $0.00
        assert_eq!(quote.subtotal, MoneyCents::from_cents(363_000));
        assert_eq!(quote.total_discount, MoneyCents::ZERO);
        assert_eq!(quote.total_to_pay, MoneyCents::from_cents(363_000));
        assert!(quote
            .items
            .iter()
            .any(|i| i.concept_code == "RECARGOS_MORA" && i.amount == MoneyCents::from_cents(18_000)));
        Ok(())
    }

    #[test]
    fn test_case_5_sandbox_anti_dos_aborts_infinite_loop() -> Result<(), TaxEngineError> {
        let engine = RhaiTaxRuleEngine::new();
        let tenant = TenantId::try_new("mun_malicioso")?;
        engine.register_rule(
            tenant.clone(),
            DepartmentKind::PropertyTax,
            2026,
            "loop {}",
            15,
        )?;

        let tax_object = build_tax_object(tenant, 100_000_000, TaxpayerBenefit::None)?;
        let uma = MoneyCents::from_non_negative(SAMPLE_UMA_CENTS)?;
        let jan_15 = NaiveDate::from_ymd_opt(2026, 1, 15).ok_or_else(|| {
            TaxEngineError::InvalidScriptOutput("invalid test date".to_owned())
        })?;

        let res = engine.evaluate_tax_quote(&tax_object, 2026, uma, jan_15);
        assert!(matches!(res, Err(TaxEngineError::ScriptEvaluationError(_))));
        Ok(())
    }
}
