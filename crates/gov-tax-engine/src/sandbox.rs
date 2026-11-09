use gov_domain::{CriCode, DomainError, MoneyCents};
use rhai::{Dynamic, Engine, EvalAltResult, Map, Position};

fn domain_err_to_rhai(err: DomainError) -> Box<EvalAltResult> {
    EvalAltResult::ErrorRuntime(Dynamic::from(err.to_string()), Position::NONE).into()
}

fn custom_err_to_rhai(msg: impl Into<String>) -> Box<EvalAltResult> {
    EvalAltResult::ErrorRuntime(Dynamic::from(msg.into()), Position::NONE).into()
}

/// Aplica puntos base (`10_000 bps = 100.00%`) a un monto en centavos usando redondeo bancario exacto (`MidpointNearestEven`).
///
/// # Errors
/// Retorna `EvalAltResult` si `amount_cents < 0`, `bps < 0`, `bps > u32::MAX` o si hay desbordamiento aritmético.
pub fn host_apply_bps(amount_cents: i64, bps: i64) -> Result<i64, Box<EvalAltResult>> {
    let bps_u32 = u32::try_from(bps).map_err(|_| {
        custom_err_to_rhai(format!("invalid basis points value: {bps} (must fit in u32)"))
    })?;
    let money = MoneyCents::from_non_negative(amount_cents).map_err(domain_err_to_rhai)?;
    let calculated = money
        .apply_basis_points(bps_u32)
        .map_err(domain_err_to_rhai)?;
    Ok(calculated.as_cents())
}

/// Suma entera segura de centavos con detección de overflow.
///
/// # Errors
/// Retorna `EvalAltResult` en caso de overflow de `i64`.
pub fn host_checked_add(a: i64, b: i64) -> Result<i64, Box<EvalAltResult>> {
    let sum = MoneyCents::from_cents(a)
        .checked_add(MoneyCents::from_cents(b))
        .map_err(domain_err_to_rhai)?;
    Ok(sum.as_cents())
}

/// Resta entera segura de centavos con detección de overflow.
///
/// # Errors
/// Retorna `EvalAltResult` en caso de overflow de `i64`.
pub fn host_checked_sub(a: i64, b: i64) -> Result<i64, Box<EvalAltResult>> {
    let diff = MoneyCents::from_cents(a)
        .checked_sub(MoneyCents::from_cents(b))
        .map_err(domain_err_to_rhai)?;
    Ok(diff.as_cents())
}

/// Multiplicación entera segura con detección de overflow.
///
/// # Errors
/// Retorna `EvalAltResult` en caso de overflow de `i64`.
pub fn host_checked_mul(a: i64, b: i64) -> Result<i64, Box<EvalAltResult>> {
    let product = MoneyCents::from_cents(a)
        .checked_mul_i64(b)
        .map_err(domain_err_to_rhai)?;
    Ok(product.as_cents())
}

/// Calcula un tope fiscal en UMAs (`uma_daily_cents * uma_count`) de forma segura.
///
/// # Errors
/// Retorna `EvalAltResult` si algún operando es negativo o si la multiplicación desborda `i64`.
pub fn host_uma_threshold_cents(
    uma_daily_cents: i64,
    uma_count: i64,
) -> Result<i64, Box<EvalAltResult>> {
    if uma_count < 0 {
        return Err(domain_err_to_rhai(DomainError::NegativeAmountNotAllowed(
            uma_count,
        )));
    }
    let uma = MoneyCents::from_non_negative(uma_daily_cents).map_err(domain_err_to_rhai)?;
    let threshold = uma.checked_mul_i64(uma_count).map_err(domain_err_to_rhai)?;
    Ok(threshold.as_cents())
}

/// Construye un concepto de cotización (`rhai::Map`) validando inmediatamente el [`CriCode`] y el monto no negativo.
///
/// # Errors
/// Retorna `EvalAltResult` si el `cri_code` es inválido, si `amount_cents < 0` o si los textos están vacíos.
pub fn host_line_item(
    concept_code: &str,
    description: &str,
    cri_code: &str,
    amount_cents: i64,
    is_discount: bool,
) -> Result<Map, Box<EvalAltResult>> {
    if concept_code.trim().is_empty() {
        return Err(custom_err_to_rhai("concept_code cannot be empty"));
    }
    if description.trim().is_empty() {
        return Err(custom_err_to_rhai("description cannot be empty"));
    }

    let valid_cri = CriCode::try_new(cri_code).map_err(domain_err_to_rhai)?;
    let valid_amount = MoneyCents::from_non_negative(amount_cents).map_err(domain_err_to_rhai)?;

    let mut map = Map::new();
    map.insert(
        "concept_code".into(),
        Dynamic::from(concept_code.trim().to_owned()),
    );
    map.insert(
        "description".into(),
        Dynamic::from(description.trim().to_owned()),
    );
    map.insert("cri_code".into(), Dynamic::from(valid_cri.to_string()));
    map.insert("amount_cents".into(), Dynamic::from(valid_amount.as_cents()));
    map.insert("is_discount".into(), Dynamic::from(is_discount));

    Ok(map)
}

/// Construye y configura una instancia blindada (sandboxed) de [`rhai::Engine`] con límites estrictos
/// anti-DoS y las funciones nativas financieras registradas.
#[must_use]
pub fn build_sandboxed_engine() -> Engine {
    let mut engine = Engine::new();

    engine.set_max_operations(50_000);
    engine.set_max_call_levels(16);
    engine.set_max_expr_depths(32, 16);
    engine.set_max_string_size(4096);
    engine.set_max_array_size(128);
    engine.set_max_map_size(128);

    engine.register_fn("apply_bps", host_apply_bps);
    engine.register_fn("checked_add", host_checked_add);
    engine.register_fn("checked_sub", host_checked_sub);
    engine.register_fn("checked_mul", host_checked_mul);
    engine.register_fn("uma_threshold_cents", host_uma_threshold_cents);
    engine.register_fn("line_item", host_line_item);

    engine
}

