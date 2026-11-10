use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use gov_domain::{
    DepartmentKind, ExternalTaxId, MoneyCents, QuoteLineItem, TenantId,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::ReceiptError;

const HEX_LOWER: &[u8; 16] = b"0123456789abcdef";

/// Codifica un slice de bytes en hexadecimal minúscula sin asignaciones inseguras.
#[must_use]
pub fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for &b in bytes {
        let hi = usize::from(b >> 4);
        let lo = usize::from(b & 0x0F);
        out.push(char::from(HEX_LOWER[hi]));
        out.push(char::from(HEX_LOWER[lo]));
    }
    out
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Decodifica una cadena hexadecimal par en un vector de bytes.
#[must_use]
pub fn hex_to_bytes(hex_str: &str) -> Option<Vec<u8>> {
    let raw = hex_str.as_bytes();
    if raw.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(raw.len() / 2);
    for pair in raw.chunks_exact(2) {
        let hi = hex_nibble(pair[0])?;
        let lo = hex_nibble(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

/// Datos estructurados del comprobante oficial de pago municipal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptPayload {
    pub receipt_id: Uuid,
    pub payment_id: Uuid,
    pub quote_id: Uuid,
    pub tenant_id: TenantId,
    pub department: DepartmentKind,
    pub external_tax_id: ExternalTaxId,
    pub fiscal_year: i32,
    pub items: Vec<QuoteLineItem>,
    pub subtotal: MoneyCents,
    pub total_discount: MoneyCents,
    pub total_paid: MoneyCents,
    pub legacy_folio: String,
    pub issued_at_unix: i64,
}

impl ReceiptPayload {
    /// Construye la Cadena Original determinista estilo SAT/Tesorería:
    /// `||1.0|{tenant_id}|{payment_id}|{external_tax_id}|{fiscal_year}|{total_paid_cents}|{issued_at_unix}|{legacy_folio}||`
    #[must_use]
    pub fn canonical_original_string(&self) -> String {
        format!(
            "||1.0|{}|{}|{}|{}|{}|{}|{}||",
            self.tenant_id.as_str(),
            self.payment_id,
            self.external_tax_id.as_str(),
            self.fiscal_year,
            self.total_paid.as_cents(),
            self.issued_at_unix,
            self.legacy_folio
        )
    }

    /// Calcula la huella criptográfica SHA-256 en hexadecimal minúscula sobre la Cadena Original.
    #[must_use]
    pub fn sha256_hex(&self) -> String {
        let chain = self.canonical_original_string();
        let mut hasher = Sha256::new();
        hasher.update(chain.as_bytes());
        let digest = hasher.finalize();
        bytes_to_hex(&digest)
    }
}

/// Recibo oficial municipal sellado criptográficamente con Ed25519.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedOfficialReceipt {
    pub payload: ReceiptPayload,
    pub canonical_chain: String,
    pub sha256_digest_hex: String,
    pub signature_ed25519_hex: String,
    pub public_key_hex: String,
    pub qr_verification_url: String,
}

impl SignedOfficialReceipt {
    /// Verifica criptográficamente la integridad de la Cadena Original y la autenticidad
    /// del sello digital Ed25519 usando `verify_strict`.
    ///
    /// # Errors
    /// - [`ReceiptError::TamperedCanonicalChain`] si el payload fue modificado respecto a `canonical_chain` o `sha256_digest_hex`.
    /// - [`ReceiptError::InvalidPublicKeyBytes`] si la llave pública es inválida.
    /// - [`ReceiptError::InvalidSignatureFormat`] si la firma no tiene 64 bytes válidos.
    /// - [`ReceiptError::SignatureVerificationFailed`] si la firma no corresponde a la cadena original y llave pública.
    pub fn verify(&self) -> Result<(), ReceiptError> {
        let recomputed_chain = self.payload.canonical_original_string();
        if recomputed_chain != self.canonical_chain {
            return Err(ReceiptError::TamperedCanonicalChain);
        }

        let recomputed_sha256 = self.payload.sha256_hex();
        if recomputed_sha256 != self.sha256_digest_hex {
            return Err(ReceiptError::TamperedCanonicalChain);
        }

        let pk_vec =
            hex_to_bytes(&self.public_key_hex).ok_or(ReceiptError::InvalidPublicKeyBytes)?;
        let pk_bytes: [u8; 32] = pk_vec
            .try_into()
            .map_err(|_| ReceiptError::InvalidPublicKeyBytes)?;
        let verifying_key = VerifyingKey::from_bytes(&pk_bytes)
            .map_err(|_| ReceiptError::InvalidPublicKeyBytes)?;

        let sig_vec = hex_to_bytes(&self.signature_ed25519_hex)
            .ok_or(ReceiptError::InvalidSignatureFormat)?;
        let sig_bytes: [u8; 64] = sig_vec
            .try_into()
            .map_err(|_| ReceiptError::InvalidSignatureFormat)?;
        let signature = Signature::from_bytes(&sig_bytes);

        verifying_key
            .verify_strict(self.canonical_chain.as_bytes(), &signature)
            .map_err(|_| ReceiptError::SignatureVerificationFailed)
    }
}

/// Firmador criptográfico institucional Ed25519 de la Tesorería Municipal.
#[derive(Debug, Clone)]
pub struct ReceiptSigner {
    signing_key: SigningKey,
}

impl ReceiptSigner {
    /// Construye un [`ReceiptSigner`] a partir de una semilla privada de 32 bytes.
    #[must_use]
    pub fn from_seed_bytes(seed: &[u8; 32]) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(seed),
        }
    }

    /// Devuelve la llave pública de verificación Ed25519 codificada en hexadecimal.
    #[must_use]
    pub fn verifying_key_hex(&self) -> String {
        let vk = self.signing_key.verifying_key();
        bytes_to_hex(vk.as_bytes())
    }

    /// Genera la Cadena Original, calcula el digest SHA-256, firma con Ed25519 y construye
    /// el [`SignedOfficialReceipt`] con su URL de verificación QR.
    #[must_use]
    pub fn sign_receipt(&self, payload: ReceiptPayload) -> SignedOfficialReceipt {
        let canonical_chain = payload.canonical_original_string();
        let sha256_digest_hex = payload.sha256_hex();

        let signature: Signature = self.signing_key.sign(canonical_chain.as_bytes());
        let signature_ed25519_hex = bytes_to_hex(&signature.to_bytes());
        let public_key_hex = self.verifying_key_hex();

        let sig_prefix = signature_ed25519_hex
            .get(..16)
            .unwrap_or(&signature_ed25519_hex);
        let qr_verification_url = format!(
            "https://gob.mx/verify/{}?sig={sig_prefix}",
            payload.receipt_id
        );

        SignedOfficialReceipt {
            payload,
            canonical_chain,
            sha256_digest_hex,
            signature_ed25519_hex,
            public_key_hex,
            qr_verification_url,
        }
    }
}
