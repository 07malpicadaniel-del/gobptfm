/// Devuelve el script Rhai determinista de referencia para el cálculo de Impuesto Predial Municipal
/// conforme a la Ley de Ingresos Municipal:
///
/// - **Impuesto Predial Base (`CRI 1.2.01.01`)**: `25 bps` (`0.25%`) sobre `base_value_cents`,
///   con cuota mínima anual de `3 UMAs` (`uma_threshold_cents(uma_cents, 3)`).
/// - **Impuesto Adicional para Fomento Educativo (`CRI 1.1.01.02`)**: `1500 bps` (`15%`) sobre el predial base.
/// - **Descuentos Excluyentes en Enero y Febrero (`CRI 1.2.01.99`)**:
///   - `50%` (`5000 bps`) para `InapamSenior`, `Pensioner`, `Widow` o `Disability` cuyo valor catastral
///     no exceda `10,000 UMAs` (`uma_threshold_cents(uma_cents, 10000)`).
///   - De lo contrario (o si excede el tope de `10,000 UMAs`), `20%` (`2000 bps`) por pronto pago en enero (`payment_month == 1`)
///     o `15%` (`1500 bps`) en febrero (`payment_month == 2`).
/// - **Recargos Moratorios (`CRI 1.1.07.01`)**:
///   - A partir de marzo (`payment_month >= 3`), `150 bps` (`1.5%`) por cada mes vencido `(payment_month - 2)` sobre el predial base.
#[must_use]
pub const fn standard_property_tax_script() -> &'static str {
    r#"
let calculated_base = apply_bps(base_value_cents, 25);
let min_quota_cents = uma_threshold_cents(uma_cents, 3);
let predial_base = if calculated_base < min_quota_cents {
    min_quota_cents
} else {
    calculated_base
};

let adicional_fomento = apply_bps(predial_base, 1500);

let items = [
    line_item(
        "PREDIAL_BASE",
        "Impuesto Predial Anual",
        "1.2.01.01",
        predial_base,
        false
    ),
    line_item(
        "ADICIONAL_FOMENTO",
        "15% Adicional para Fomento Educativo y Asistencia Social",
        "1.1.01.02",
        adicional_fomento,
        false
    )
];

let is_vulnerable_benefit = benefit == "InapamSenior"
    || benefit == "Pensioner"
    || benefit == "Widow"
    || benefit == "Disability";

let inapam_cap_cents = uma_threshold_cents(uma_cents, 10000);

if (payment_month == 1 || payment_month == 2) && is_vulnerable_benefit && base_value_cents <= inapam_cap_cents {
    let discount_50 = apply_bps(predial_base, 5000);
    items.push(line_item(
        "DESCUENTO_BENEFICIO_SOCIAL_50",
        "Subsidio 50% Adulto Mayor / Pensionado / Viudez / Discapacidad",
        "1.2.01.99",
        discount_50,
        true
    ));
} else if payment_month == 1 {
    let discount_20 = apply_bps(predial_base, 2000);
    items.push(line_item(
        "DESCUENTO_PRONTO_PAGO_20",
        "Descuento 20% Pronto Pago Enero",
        "1.2.01.99",
        discount_20,
        true
    ));
} else if payment_month == 2 {
    let discount_15 = apply_bps(predial_base, 1500);
    items.push(line_item(
        "DESCUENTO_PRONTO_PAGO_15",
        "Descuento 15% Pronto Pago Febrero",
        "1.2.01.99",
        discount_15,
        true
    ));
} else if payment_month >= 3 {
    let months_late = checked_sub(payment_month, 2);
    let surcharge_bps = checked_mul(months_late, 150);
    let surcharge_amount = apply_bps(predial_base, surcharge_bps);
    items.push(line_item(
        "RECARGOS_MORA",
        "Recargos Moratorios sobre Impuesto Predial (1.5% mensual)",
        "1.1.07.01",
        surcharge_amount,
        false
    ));
}

items
"#
}

