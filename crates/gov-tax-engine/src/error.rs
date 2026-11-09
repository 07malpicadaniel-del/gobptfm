use gov_domain::DomainError;
use thiserror::Error;

/// Errores del motor determinista de reglas fiscales (`gov-tax-engine`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TaxEngineError {
    #[error("fiscal rule not found for tenant='{tenant_id}', department='{department}', fiscal_year={fiscal_year}")]
    RuleNotFound {
        tenant_id: String,
        department: String,
        fiscal_year: i32,
    },

    #[error("Rhai fiscal script compilation failed: {0}")]
    ScriptCompilationError(String),

    #[error("Rhai fiscal script evaluation failed: {0}")]
    ScriptEvaluationError(String),

    #[error("invalid fiscal script output structure: {0}")]
    InvalidScriptOutput(String),

    #[error("domain validation error during tax evaluation: {0}")]
    Domain(#[from] DomainError),
}

impl From<TaxEngineError> for DomainError {
    fn from(err: TaxEngineError) -> Self {
        match err {
            TaxEngineError::Domain(domain_err) => domain_err,
            other => Self::TaxRuleEvaluationFailed(other.to_string()),
        }
    }
}

