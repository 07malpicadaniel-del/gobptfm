use std::fmt::Write as _;

use gov_domain::MoneyCents;

use crate::crypto::SignedOfficialReceipt;
use crate::error::ReceiptError;

/// Formatea un [`MoneyCents`] en pesos y centavos exactos (`$X.YY`) usando exclusivamente aritmética entera.
#[must_use]
pub fn format_cents_exact(amount: MoneyCents) -> String {
    let raw = amount.as_cents();
    let sign = if raw < 0 { "-" } else { "" };
    let pesos = (raw / 100).abs();
    let centavos = (raw % 100).abs();
    format!("{sign}${pesos}.{centavos:02} MXN")
}

/// Escapa paréntesis y barras invertidas para literales de texto PDF `(...)`.
fn escape_pdf_text(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        if !ch.is_ascii() {
            escaped.push('?');
            continue;
        }
        match ch {
            '(' => escaped.push_str("\\("),
            ')' => escaped.push_str("\\)"),
            '\\' => escaped.push_str("\\\\"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Renderiza en memoria un documento PDF 1.4 nativo (`%PDF-1.4 ... %%EOF`) sin navegador headless,
/// verificando previamente la validez criptográfica del [`SignedOfficialReceipt`].
///
/// # Errors
/// Devuelve [`ReceiptError`] si la firma o cadena original del recibo es inválida o si falla la construcción del buffer.
pub fn render_official_receipt_pdf(receipt: &SignedOfficialReceipt) -> Result<Vec<u8>, ReceiptError> {
    // Regla estricta: jamás emitir un PDF si la firma Ed25519 o la cadena original fueron alteradas.
    receipt.verify()?;

    let p = &receipt.payload;
    let mut lines: Vec<String> = Vec::with_capacity(p.items.len() + 18);

    lines.push("TESORERIA MUNICIPAL - RECIBO OFICIAL DE PAGO".to_owned());
    lines.push("================================================================".to_owned());
    lines.push(format!("Municipio / Tenant : {}", p.tenant_id));
    lines.push(format!("Folio Recibo UUID  : {}", p.receipt_id));
    lines.push(format!("Folio Pago UUID    : {}", p.payment_id));
    lines.push(format!("Folio Legacy Mun.  : {}", p.legacy_folio));
    lines.push(format!("Cuenta / Catastro  : {}", p.external_tax_id));
    lines.push(format!("Ejercicio Fiscal   : {}", p.fiscal_year));
    lines.push("----------------------------------------------------------------".to_owned());
    lines.push("CONCEPTOS Y CLASIFICADOR POR RUBRO DE INGRESOS (CONAC CRI):".to_owned());

    for item in &p.items {
        let sign_label = if item.is_discount { "DESC" } else { "CARGO" };
        lines.push(format!(
            "  [CRI {}] [{}] {} : {}",
            item.cri_code,
            sign_label,
            item.description,
            format_cents_exact(item.amount)
        ));
    }

    lines.push("----------------------------------------------------------------".to_owned());
    lines.push(format!("SUBTOTAL           : {}", format_cents_exact(p.subtotal)));
    lines.push(format!(
        "DESCUENTO TOTAL    : {}",
        format_cents_exact(p.total_discount)
    ));
    lines.push(format!(
        "TOTAL PAGADO       : {}",
        format_cents_exact(p.total_paid)
    ));
    lines.push("================================================================".to_owned());
    lines.push(format!("Cadena Original    : {}", receipt.canonical_chain));
    lines.push(format!("Huella SHA-256     : {}", receipt.sha256_digest_hex));
    lines.push(format!(
        "Sello Ed25519      : {}",
        receipt.signature_ed25519_hex
    ));
    lines.push(format!("Llave Publica Mun. : {}", receipt.public_key_hex));
    lines.push(format!(
        "Verificacion QR    : {}",
        receipt.qr_verification_url
    ));

    let mut content_stream = String::from("BT\n/F1 10 Tf\n40 750 Td\n14 TL\n");
    for (idx, line) in lines.iter().enumerate() {
        let escaped = escape_pdf_text(line);
        if idx == 0 {
            let _ = writeln!(content_stream, "({escaped}) Tj");
        } else {
            let _ = writeln!(content_stream, "T* ({escaped}) Tj");
        }
    }
    content_stream.push_str("ET\n");

    let mut pdf: Vec<u8> = Vec::with_capacity(2048 + content_stream.len());
    pdf.extend_from_slice(b"%PDF-1.4\n");

    let mut offsets: Vec<usize> = Vec::with_capacity(5);

    // Object 1: Catalog
    offsets.push(pdf.len());
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    // Object 2: Pages
    offsets.push(pdf.len());
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    // Object 3: Page
    offsets.push(pdf.len());
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n",
    );

    // Object 4: Content Stream
    offsets.push(pdf.len());
    let obj4_header = format!("4 0 obj\n<< /Length {} >>\nstream\n", content_stream.len());
    pdf.extend_from_slice(obj4_header.as_bytes());
    pdf.extend_from_slice(content_stream.as_bytes());
    pdf.extend_from_slice(b"endstream\nendobj\n");

    // Object 5: Font (Courier monospace for aligned financial tables)
    offsets.push(pdf.len());
    pdf.extend_from_slice(
        b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>\nendobj\n",
    );

    // Xref Table
    let xref_offset = pdf.len();
    let total_objects = offsets.len() + 1;
    let mut xref_section = format!("xref\n0 {total_objects}\n0000000000 65535 f \n");
    for offset in &offsets {
        let _ = writeln!(xref_section, "{offset:010} 00000 n ");
    }
    pdf.extend_from_slice(xref_section.as_bytes());

    // Trailer
    let trailer = format!(
        "trailer\n<< /Size {total_objects} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n"
    );
    pdf.extend_from_slice(trailer.as_bytes());

    Ok(pdf)
}
