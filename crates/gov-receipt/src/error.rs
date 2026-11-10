use gov_domain::DomainError;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errores del motor criptográfico y generador de recibos oficiales (`gov-receipt`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
pub enum ReceiptError {
    #[error("domain invariant error: {0}")]
    Domain(#[from] DomainError),

    #[error("invalid Ed25519 signing key seed bytes (expected 32 bytes)")]
    InvalidSigningKeyBytes,

    #[error("invalid Ed25519 public key bytes or hex encoding")]
    InvalidPublicKeyBytes,

    #[error("invalid Ed25519 signature format or hex encoding (expected 64 bytes)")]
    InvalidSignatureFormat,

    #[error("Ed25519 digital signature verification failed")]
    SignatureVerificationFailed,

    #[error("canonical original string mismatch: receipt payload has been tampered with")]
    TamperedCanonicalChain,

    #[error("failed to render native PDF 1.4 receipt: {0}")]
    PdfGenerationError(String),
}
