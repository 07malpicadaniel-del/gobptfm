use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::DomainError;

/// Validated municipality / tenant identifier used for `Schema-per-Tenant` isolation in PostgreSQL.
/// Strictly constrained to `3..=32` characters of `[a-z0-9_]` to prevent SQL injection in schema names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TenantId(String);

impl TenantId {
    /// Validates and constructs a [`TenantId`].
    ///
    /// # Errors
    /// Returns [`DomainError::InvalidTenantId`] if length is outside `3..=32` or contains any char outside `[a-z0-9_]`.
    pub fn try_new(raw: impl Into<String>) -> Result<Self, DomainError> {
        let value = raw.into();
        let len = value.len();
        if !(3..=32).contains(&len) {
            return Err(DomainError::InvalidTenantId(value));
        }
        let valid_chars = value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if !valid_chars {
            return Err(DomainError::InvalidTenantId(value));
        }
        Ok(Self(value))
    }

    /// Returns the underlying string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for TenantId {
    type Error = DomainError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl TryFrom<&str> for TenantId {
    type Error = DomainError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<TenantId> for String {
    fn from(tenant_id: TenantId) -> Self {
        tenant_id.0
    }
}

impl fmt::Display for TenantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Time-ordered UUIDv7 identifier for a [` crate::tax_object::TaxObject `].
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaxObjectId(Uuid);

impl TaxObjectId {
    /// Generates a new time-ordered UUIDv7 [`TaxObjectId`].
    #[must_use]
    pub fn new_v7() -> Self {
        Self(Uuid::now_v7())
    }

    /// Wraps an existing [`Uuid`].
    #[must_use]
    pub const fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    /// Returns the underlying [`Uuid`].
    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for TaxObjectId {
    fn default() -> Self {
        Self::new_v7()
    }
}

impl fmt::Display for TaxObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Normalized external tax identifier (Clave Catastral, Water Account, License Number, or Vehicle Plate).
/// Normalizes by stripping whitespace and converting to uppercase; enforces `1..=64` printable characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ExternalTaxId(String);

impl ExternalTaxId {
    /// Normalizes and validates an external tax identifier.
    ///
    /// # Errors
    /// Returns [`DomainError::InvalidExternalId`] if the normalized string is empty, exceeds 64 characters,
    /// or contains control characters.
    pub fn try_new(raw: impl AsRef<str>) -> Result<Self, DomainError> {
        let raw_ref = raw.as_ref();
        let normalized: String = raw_ref
            .chars()
            .filter(|c| !c.is_whitespace())
            .flat_map(char::to_uppercase)
            .collect();

        let char_count = normalized.chars().count();
        if !(1..=64).contains(&char_count) || normalized.chars().any(char::is_control) {
            return Err(DomainError::InvalidExternalId(raw_ref.to_owned()));
        }

        Ok(Self(normalized))
    }

    /// Returns the normalized identifier slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ExternalTaxId {
    type Error = DomainError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl TryFrom<&str> for ExternalTaxId {
    type Error = DomainError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<ExternalTaxId> for String {
    fn from(id: ExternalTaxId) -> Self {
        id.0
    }
}

impl fmt::Display for ExternalTaxId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Validated idempotency key for state-mutating financial endpoints and webhooks.
/// Enforces `16..=128` printable ASCII characters (`0x20..=0x7E`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// Validates and constructs an [`IdempotencyKey`].
    ///
    /// # Errors
    /// Returns [`DomainError::InvalidIdempotencyKey`] if length is outside `16..=128` or contains non-printable ASCII.
    pub fn try_new(raw: impl Into<String>) -> Result<Self, DomainError> {
        let value = raw.into();
        let len = value.len();
        if !(16..=128).contains(&len) {
            return Err(DomainError::InvalidIdempotencyKey(value));
        }
        let valid_ascii = value.bytes().all(|b| (0x20..=0x7E).contains(&b));
        if !valid_ascii {
            return Err(DomainError::InvalidIdempotencyKey(value));
        }
        Ok(Self(value))
    }

    /// Returns the underlying string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for IdempotencyKey {
    type Error = DomainError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl TryFrom<&str> for IdempotencyKey {
    type Error = DomainError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<IdempotencyKey> for String {
    fn from(key: IdempotencyKey) -> Self {
        key.0
    }
}

impl fmt::Display for IdempotencyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// CONAC Clasificador por Rubro de Ingresos (CRI) code (e.g., `"1.2.01.01"`).
/// Validates dot-separated numeric segments.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CriCode(String);

impl CriCode {
    /// Validates and constructs a [`CriCode`].
    ///
    /// # Errors
    /// Returns [`DomainError::InvalidCriCode`] if the code does not consist of at least two
    /// non-empty dot-separated numeric segments.
    pub fn try_new(raw: impl Into<String>) -> Result<Self, DomainError> {
        let value = raw.into();
        let segments: Vec<&str> = value.split('.').collect();
        if segments.len() < 2 {
            return Err(DomainError::InvalidCriCode(value));
        }
        let all_numeric = segments
            .iter()
            .all(|seg| !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit()));
        if !all_numeric {
            return Err(DomainError::InvalidCriCode(value));
        }
        Ok(Self(value))
    }

    /// Returns the underlying string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for CriCode {
    type Error = DomainError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl TryFrom<&str> for CriCode {
    type Error = DomainError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<CriCode> for String {
    fn from(code: CriCode) -> Self {
        code.0
    }
}

impl fmt::Display for CriCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tenant_id_blocks_sql_injection_and_invalid_lengths() {
        assert!(TenantId::try_new("mun_veracruz_01").is_ok());
        assert_eq!(
            TenantId::try_new("mun_veracruz; DROP SCHEMA"),
            Err(DomainError::InvalidTenantId(
                "mun_veracruz; DROP SCHEMA".to_owned()
            ))
        );
        assert_eq!(
            TenantId::try_new("ab"),
            Err(DomainError::InvalidTenantId("ab".to_owned()))
        );
        assert_eq!(
            TenantId::try_new("MUN_UPPERCASE"),
            Err(DomainError::InvalidTenantId("MUN_UPPERCASE".to_owned()))
        );
    }

    #[test]
    fn test_external_tax_id_normalizes_whitespace_and_case() {
        let ext = ExternalTaxId::try_new("  30-014-001-02-0045-a  ");
        assert!(ext.is_ok());
        if let Ok(normalized) = ext {
            assert_eq!(normalized.as_str(), "30-014-001-02-0045-A");
        }
        assert!(ExternalTaxId::try_new("   ").is_err());
    }

    #[test]
    fn test_idempotency_key_and_cri_code_validation() {
        assert!(IdempotencyKey::try_new("idem-key-2026-0001-abcdef").is_ok());
        assert!(IdempotencyKey::try_new("too-short").is_err());

        assert!(CriCode::try_new("1.2.01.01").is_ok());
        assert!(CriCode::try_new("120101").is_err());
        assert!(CriCode::try_new("1.2..01").is_err());
        assert!(CriCode::try_new("1.2.A1").is_err());
    }
}
