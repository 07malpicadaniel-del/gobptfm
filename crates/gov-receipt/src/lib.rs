//! Official digital receipt issuance: Ed25519 cryptographic signing, original chain (cadena original), QR, and native PDF rendering.

pub mod crypto;
pub mod error;
pub mod pdf;

pub use crypto::{
    bytes_to_hex, hex_to_bytes, ReceiptPayload, ReceiptSigner, SignedOfficialReceipt,
};
pub use error::ReceiptError;
pub use pdf::{format_cents_exact, render_official_receipt_pdf};

#[cfg(test)]
mod tests {
    use gov_domain::{
        CriCode, DepartmentKind, DomainError, ExternalTaxId, MoneyCents, QuoteLineItem, TenantId,
    };
    use uuid::Uuid;

    use super::*;

    fn sample_signed_receipt() -> Result<SignedOfficialReceipt, DomainError> {
        let signer = ReceiptSigner::from_seed_bytes(&[0x42_u8; 32]);
        let tenant_id = TenantId::try_new("mun_veracruz")?;
        let ext_id = ExternalTaxId::try_new("30-193-001-01-0100")?;
        let cri = CriCode::try_new("1.2.01.01")?;

        let items = vec![
            QuoteLineItem {
                concept_code: "PREDIAL_BASE".to_owned(),
                description: "Impuesto Predial Anual".to_owned(),
                cri_code: cri.clone(),
                amount: MoneyCents::from_cents(200_000),
                is_discount: false,
            },
            QuoteLineItem {
                concept_code: "SUBSIDIO_INAPAM".to_owned(),
                description: "Subsidio 50% INAPAM".to_owned(),
                cri_code: cri,
                amount: MoneyCents::from_cents(100_000),
                is_discount: true,
            },
        ];

        let payload = ReceiptPayload {
            receipt_id: Uuid::now_v7(),
            payment_id: Uuid::now_v7(),
            quote_id: Uuid::now_v7(),
            tenant_id,
            department: DepartmentKind::PropertyTax,
            external_tax_id: ext_id,
            fiscal_year: 2026,
            items,
            subtotal: MoneyCents::from_cents(200_000),
            total_discount: MoneyCents::from_cents(100_000),
            total_paid: MoneyCents::from_cents(100_000),
            legacy_folio: "LEGACY-FOLIO-mun_veracruz-2026-1".to_owned(),
            issued_at_unix: 1_762_725_000,
        };

        Ok(signer.sign_receipt(payload))
    }

    #[test]
    fn test_ed25519_signing_and_strict_verification_succeeds() -> Result<(), ReceiptError> {
        let receipt = sample_signed_receipt()?;
        assert!(receipt.canonical_chain.starts_with("||1.0|mun_veracruz|"));
        assert!(receipt.qr_verification_url.starts_with("https://gob.mx/verify/"));
        assert_eq!(receipt.verify(), Ok(()));
        Ok(())
    }

    #[test]
    fn test_tampering_single_cent_or_folio_char_triggers_tampered_canonical_chain(
    ) -> Result<(), ReceiptError> {
        let mut tampered_amount = sample_signed_receipt()?;
        tampered_amount.payload.total_paid = MoneyCents::from_cents(100_001);
        assert_eq!(
            tampered_amount.verify(),
            Err(ReceiptError::TamperedCanonicalChain)
        );

        let mut tampered_folio = sample_signed_receipt()?;
        tampered_folio.payload.legacy_folio.push('X');
        assert_eq!(
            tampered_folio.verify(),
            Err(ReceiptError::TamperedCanonicalChain)
        );
        Ok(())
    }

    #[test]
    fn test_render_official_receipt_pdf_produces_valid_pdf_1_4() -> Result<(), ReceiptError> {
        let receipt = sample_signed_receipt()?;
        let pdf_bytes = render_official_receipt_pdf(&receipt)?;
        let pdf_text = String::from_utf8_lossy(&pdf_bytes);

        assert!(pdf_bytes.starts_with(b"%PDF-1.4\n"));
        assert!(pdf_text.contains("TESORERIA MUNICIPAL - RECIBO OFICIAL DE PAGO"));
        assert!(pdf_text.contains("1.2.01.01"));
        assert!(pdf_text.contains(&receipt.signature_ed25519_hex));
        assert!(pdf_text.ends_with("%%EOF\n"));
        Ok(())
    }
}
