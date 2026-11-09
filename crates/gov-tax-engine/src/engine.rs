use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};

use async_trait::async_trait;
use chrono::{Datelike, Duration, NaiveDate, Utc};
use gov_domain::{
    CriCode, DepartmentKind, DomainError, MoneyCents, QuoteLineItem, TaxObject, TaxQuote,
    TaxRuleEnginePort, TaxpayerBenefit, TenantId,
};
use rhai::{Array, Engine, Map, Scope, AST};
use tracing::instrument;

use crate::error::TaxEngineError;
use crate::sandbox::build_sandboxed_engine;

/// Llave compuesta de búsqueda para reglas fiscales compiladas por municipio, departamento y ejercicio.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RuleKey {
    pub tenant_id: TenantId,
    pub department: DepartmentKind,
    pub fiscal_year: i32,
}

/// Regla fiscal compilada en forma de AST inmutable lista para ejecución concurrente.
#[derive(Debug, Clone)]
pub struct CompiledRule {
    pub ast: AST,
    pub quote_ttl_minutes: i64,
}

const fn benefit_as_str(benefit: TaxpayerBenefit) -> &'static str {
    match benefit {
        TaxpayerBenefit::None => "None",
        TaxpayerBenefit::InapamSenior => "InapamSenior",
        TaxpayerBenefit::Pensioner => "Pensioner",
        TaxpayerBenefit::Widow => "Widow",
        TaxpayerBenefit::Disability => "Disability",
    }
}

const fn department_as_str(dept: DepartmentKind) -> &'static str {
    match dept {
        DepartmentKind::PropertyTax => "PropertyTax",
        DepartmentKind::WaterUtility => "WaterUtility",
        DepartmentKind::BusinessLicense => "BusinessLicense",
        DepartmentKind::TrafficFine => "TrafficFine",
    }
}

fn extract_string_field(map: &Map, key: &str) -> Result<String, TaxEngineError> {
    let val = map.get(key).ok_or_else(|| {
        TaxEngineError::InvalidScriptOutput(format!("missing required string field '{key}'"))
    })?;
    val.clone().into_string().map_err(|type_name| {
        TaxEngineError::InvalidScriptOutput(format!(
            "field '{key}' must be a string, got '{type_name}'"
        ))
    })
}

fn extract_i64_field(map: &Map, key: &str) -> Result<i64, TaxEngineError> {
    let val = map.get(key).ok_or_else(|| {
        TaxEngineError::InvalidScriptOutput(format!("missing required integer field '{key}'"))
    })?;
    val.as_int().map_err(|type_name| {
        TaxEngineError::InvalidScriptOutput(format!(
            "field '{key}' must be an i64 integer, got '{type_name}'"
        ))
    })
}

fn extract_bool_field(map: &Map, key: &str) -> Result<bool, TaxEngineError> {
    let val = map.get(key).ok_or_else(|| {
        TaxEngineError::InvalidScriptOutput(format!("missing required boolean field '{key}'"))
    })?;
    val.as_bool().map_err(|type_name| {
        TaxEngineError::InvalidScriptOutput(format!(
            "field '{key}' must be a bool, got '{type_name}'"
        ))
    })
}

/// Motor determinista de reglas fiscales basado en [`rhai::Engine`] en modo sandbox (`no_float`).
#[derive(Debug)]
pub struct RhaiTaxRuleEngine {
    engine: Engine,
    rules: RwLock<HashMap<RuleKey, Arc<CompiledRule>>>,
}

impl Default for RhaiTaxRuleEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl RhaiTaxRuleEngine {
    /// Crea una nueva instancia de [`RhaiTaxRuleEngine`] con el sandbox preconfigurado.
    #[must_use]
    pub fn new() -> Self {
        Self {
            engine: build_sandboxed_engine(),
            rules: RwLock::new(HashMap::new()),
        }
    }

    /// Compila y registra una regla fiscal Rhai para un `(tenant_id, department, fiscal_year)`.
    ///
    /// # Errors
    /// Devuelve [`TaxEngineError::ScriptCompilationError`] si el script contiene errores de sintaxis,
    /// o [`TaxEngineError::Domain`] si `quote_ttl_minutes <= 0`.
    pub fn register_rule(
        &self,
        tenant_id: TenantId,
        department: DepartmentKind,
        fiscal_year: i32,
        script_source: &str,
        quote_ttl_minutes: i64,
    ) -> Result<(), TaxEngineError> {
        if quote_ttl_minutes <= 0 {
            return Err(TaxEngineError::Domain(
                DomainError::NegativeAmountNotAllowed(quote_ttl_minutes),
            ));
        }

        let ast = self
            .engine
            .compile(script_source)
            .map_err(|err| TaxEngineError::ScriptCompilationError(err.to_string()))?;

        let key = RuleKey {
            tenant_id,
            department,
            fiscal_year,
        };
        let compiled = Arc::new(CompiledRule {
            ast,
            quote_ttl_minutes,
        });

        let mut guard = self.rules.write().unwrap_or_else(PoisonError::into_inner);
        guard.insert(key, compiled);
        Ok(())
    }

    /// Evalúa de forma determinista el [`AST`] registrado para el objeto imponible y devuelve
    /// una cotización [`TaxQuote`] cuadrada al centavo, conservando el tipo de error detallado [`TaxEngineError`].
    ///
    /// # Errors
    /// Devuelve [`TaxEngineError`] si la regla no existe, el script excede los límites del sandbox,
    /// o la salida no cumple los invariantes financieros.
    #[instrument(skip(self, tax_object), fields(tenant = %tax_object.tenant_id, dept = ?tax_object.department, fiscal_year))]
    pub fn evaluate_tax_quote(
        &self,
        tax_object: &TaxObject,
        fiscal_year: i32,
        uma_value: MoneyCents,
        payment_date: NaiveDate,
    ) -> Result<TaxQuote, TaxEngineError> {
        if uma_value.is_negative() {
            return Err(TaxEngineError::Domain(
                DomainError::NegativeAmountNotAllowed(uma_value.as_cents()),
            ));
        }

        let key = RuleKey {
            tenant_id: tax_object.tenant_id.clone(),
            department: tax_object.department,
            fiscal_year,
        };

        let compiled = {
            let guard = self.rules.read().unwrap_or_else(PoisonError::into_inner);
            guard.get(&key).cloned()
        }
        .ok_or_else(|| TaxEngineError::RuleNotFound {
            tenant_id: tax_object.tenant_id.to_string(),
            department: department_as_str(tax_object.department).to_owned(),
            fiscal_year,
        })?;

        let metadata_dyn = rhai::serde::to_dynamic(&tax_object.metadata)
            .map_err(|err| TaxEngineError::ScriptEvaluationError(err.to_string()))?;

        let mut scope = Scope::new();
        scope.push(
            "base_value_cents",
            tax_object.cadastral_or_base_value.as_cents(),
        );
        scope.push("uma_cents", uma_value.as_cents());
        scope.push("fiscal_year", i64::from(fiscal_year));
        scope.push("payment_year", i64::from(payment_date.year()));
        scope.push("payment_month", i64::from(payment_date.month()));
        scope.push("payment_day", i64::from(payment_date.day()));
        scope.push(
            "benefit",
            benefit_as_str(tax_object.active_benefit).to_owned(),
        );
        scope.push_dynamic("metadata", metadata_dyn);

        let raw_items: Array = self
            .engine
            .eval_ast_with_scope(&mut scope, &compiled.ast)
            .map_err(|err| TaxEngineError::ScriptEvaluationError(err.to_string()))?;

        if raw_items.is_empty() {
            return Err(TaxEngineError::InvalidScriptOutput(
                "fiscal rule returned zero line items".to_owned(),
            ));
        }

        let mut items = Vec::with_capacity(raw_items.len());
        let mut subtotal = MoneyCents::ZERO;
        let mut total_discount = MoneyCents::ZERO;

        for (idx, elem) in raw_items.into_iter().enumerate() {
            let map = elem.try_cast::<Map>().ok_or_else(|| {
                TaxEngineError::InvalidScriptOutput(format!(
                    "item at index {idx} is not a Rhai Map"
                ))
            })?;

            let concept_code = extract_string_field(&map, "concept_code")?;
            let description = extract_string_field(&map, "description")?;
            let cri_raw = extract_string_field(&map, "cri_code")?;
            let amount_raw = extract_i64_field(&map, "amount_cents")?;
            let is_discount = extract_bool_field(&map, "is_discount")?;

            let cri_code = CriCode::try_new(cri_raw)?;
            let amount = MoneyCents::from_non_negative(amount_raw)?;

            if is_discount {
                total_discount = total_discount.checked_add(amount)?;
            } else {
                subtotal = subtotal.checked_add(amount)?;
            }

            items.push(QuoteLineItem {
                concept_code,
                description,
                cri_code,
                amount,
                is_discount,
            });
        }

        let net_to_pay = subtotal.checked_sub(total_discount)?;
        let _verified_non_negative = MoneyCents::from_non_negative(net_to_pay.as_cents())?;

        let expires_at = Utc::now() + Duration::minutes(compiled.quote_ttl_minutes);
        let quote = TaxQuote::try_build(
            tax_object.tenant_id.clone(),
            tax_object.id,
            fiscal_year,
            items,
            expires_at,
        )?;

        Ok(quote)
    }
}

#[async_trait]
impl TaxRuleEnginePort for RhaiTaxRuleEngine {
    async fn evaluate_quote(
        &self,
        tax_object: &TaxObject,
        fiscal_year: i32,
        uma_value: MoneyCents,
        payment_date: NaiveDate,
    ) -> Result<TaxQuote, DomainError> {
        Ok(self.evaluate_tax_quote(tax_object, fiscal_year, uma_value, payment_date)?)
    }
}

