//! Protobuf wire contract definitions (`gov.bridge.v1`) for the Cloud <-> Municipal Edge bidirectional bridge.
//! Uses `prost::Message` derive macros with explicit field tags for deterministic binary encoding.

use prost::Message;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Error de decodificación binaria Protobuf en el puente Nube-Municipio.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BridgeDecodeError {
    #[error("failed to decode protobuf frame: {0}")]
    InvalidProtobufFrame(String),
}

/// Estado de confirmación de inyección de un pago en el sistema municipal legacy.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, ::prost::Enumeration,
)]
#[repr(i32)]
pub enum InjectionAckStatus {
    Unspecified = 0,
    Committed = 1,
    QueuedLocally = 2,
    RejectedConflict = 3,
}

/// Verificación previa (`preflight check`) antes de autorizar el cobro digital.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ::prost::Message)]
pub struct PreflightDebtCheckRequest {
    #[prost(string, tag = "1")]
    pub request_id: String,
    #[prost(string, tag = "2")]
    pub tenant_id: String,
    #[prost(string, tag = "3")]
    pub department: String,
    #[prost(string, tag = "4")]
    pub external_tax_id: String,
    #[prost(int64, tag = "5")]
    pub expected_amount_cents: i64,
}

impl PreflightDebtCheckRequest {
    #[must_use]
    pub fn encode_to_vec(&self) -> Vec<u8> {
        Message::encode_to_vec(self)
    }

    /// Decodifica un [`PreflightDebtCheckRequest`] desde un slice binario Protobuf.
    ///
    /// # Errors
    /// Devuelve [`BridgeDecodeError::InvalidProtobufFrame`] si el frame es inválido.
    pub fn decode_from_slice(bytes: &[u8]) -> Result<Self, BridgeDecodeError> {
        Message::decode(bytes).map_err(|e| BridgeDecodeError::InvalidProtobufFrame(e.to_string()))
    }
}

/// Respuesta del sistema legacy municipal al `preflight check`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ::prost::Message)]
pub struct PreflightDebtCheckResponse {
    #[prost(string, tag = "1")]
    pub request_id: String,
    #[prost(bool, tag = "2")]
    pub is_valid: bool,
    #[prost(int64, tag = "3")]
    pub current_legacy_debt_cents: i64,
    #[prost(bool, tag = "4")]
    pub is_already_paid_in_window: bool,
    #[prost(string, tag = "5")]
    pub rejection_reason: String,
}

impl PreflightDebtCheckResponse {
    #[must_use]
    pub fn encode_to_vec(&self) -> Vec<u8> {
        Message::encode_to_vec(self)
    }

    /// Decodifica un [`PreflightDebtCheckResponse`] desde un slice binario Protobuf.
    ///
    /// # Errors
    /// Devuelve [`BridgeDecodeError::InvalidProtobufFrame`] si el frame es inválido.
    pub fn decode_from_slice(bytes: &[u8]) -> Result<Self, BridgeDecodeError> {
        Message::decode(bytes).map_err(|e| BridgeDecodeError::InvalidProtobufFrame(e.to_string()))
    }
}

/// Comando idempotente para inyectar un pago autorizado en la base de datos legacy municipal.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ::prost::Message)]
pub struct InjectPaymentCommand {
    #[prost(string, tag = "1")]
    pub outbox_id: String,
    #[prost(string, tag = "2")]
    pub payment_id: String,
    #[prost(string, tag = "3")]
    pub tenant_id: String,
    #[prost(string, tag = "4")]
    pub department: String,
    #[prost(string, tag = "5")]
    pub external_tax_id: String,
    #[prost(string, tag = "6")]
    pub idempotency_key: String,
    #[prost(int64, tag = "7")]
    pub amount_cents: i64,
    #[prost(int32, tag = "8")]
    pub fiscal_year: i32,
    #[prost(int64, tag = "9")]
    pub paid_at_unix: i64,
}

impl InjectPaymentCommand {
    #[must_use]
    pub fn encode_to_vec(&self) -> Vec<u8> {
        Message::encode_to_vec(self)
    }

    /// Decodifica un [`InjectPaymentCommand`] desde un slice binario Protobuf.
    ///
    /// # Errors
    /// Devuelve [`BridgeDecodeError::InvalidProtobufFrame`] si el frame es inválido.
    pub fn decode_from_slice(bytes: &[u8]) -> Result<Self, BridgeDecodeError> {
        Message::decode(bytes).map_err(|e| BridgeDecodeError::InvalidProtobufFrame(e.to_string()))
    }
}

/// Confirmación emitida por `gov-edge-agent` tras procesar un [`InjectPaymentCommand`].
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ::prost::Message)]
pub struct InjectPaymentAck {
    #[prost(string, tag = "1")]
    pub outbox_id: String,
    #[prost(string, tag = "2")]
    pub payment_id: String,
    #[prost(enumeration = "InjectionAckStatus", tag = "3")]
    pub status: i32,
    #[prost(string, tag = "4")]
    pub legacy_receipt_folio: String,
    #[prost(string, tag = "5")]
    pub error_message: String,
}

impl InjectPaymentAck {
    /// Devuelve el enum tipado [`InjectionAckStatus`] asociado al campo `status`.
    #[must_use]
    pub fn ack_status(&self) -> InjectionAckStatus {
        InjectionAckStatus::try_from(self.status).unwrap_or(InjectionAckStatus::Unspecified)
    }

    #[must_use]
    pub fn encode_to_vec(&self) -> Vec<u8> {
        Message::encode_to_vec(self)
    }

    /// Decodifica un [`InjectPaymentAck`] desde un slice binario Protobuf.
    ///
    /// # Errors
    /// Devuelve [`BridgeDecodeError::InvalidProtobufFrame`] si el frame es inválido.
    pub fn decode_from_slice(bytes: &[u8]) -> Result<Self, BridgeDecodeError> {
        Message::decode(bytes).map_err(|e| BridgeDecodeError::InvalidProtobufFrame(e.to_string()))
    }
}

/// Delta de sincronización de cuenta catastral/padrón desde el municipio hacia la nube.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ::prost::Message)]
pub struct LegacyAccountDelta {
    #[prost(string, tag = "1")]
    pub tenant_id: String,
    #[prost(string, tag = "2")]
    pub department: String,
    #[prost(string, tag = "3")]
    pub external_tax_id: String,
    #[prost(string, tag = "4")]
    pub owner_verification_hash: String,
    #[prost(int64, tag = "5")]
    pub cadastral_or_base_value_cents: i64,
    #[prost(string, tag = "6")]
    pub active_benefit: String,
    #[prost(bool, tag = "7")]
    pub is_paid_current_year: bool,
    #[prost(int64, tag = "8")]
    pub updated_at_unix: i64,
    #[prost(string, tag = "9")]
    pub metadata_json: String,
}

impl LegacyAccountDelta {
    #[must_use]
    pub fn encode_to_vec(&self) -> Vec<u8> {
        Message::encode_to_vec(self)
    }

    /// Decodifica un [`LegacyAccountDelta`] desde un slice binario Protobuf.
    ///
    /// # Errors
    /// Devuelve [`BridgeDecodeError::InvalidProtobufFrame`] si el frame es inválido.
    pub fn decode_from_slice(bytes: &[u8]) -> Result<Self, BridgeDecodeError> {
        Message::decode(bytes).map_err(|e| BridgeDecodeError::InvalidProtobufFrame(e.to_string()))
    }
}

/// Telemetría de salud y estado del spool local del agente municipal.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ::prost::Message)]
pub struct AgentHeartbeat {
    #[prost(string, tag = "1")]
    pub tenant_id: String,
    #[prost(string, tag = "2")]
    pub agent_version: String,
    #[prost(uint64, tag = "3")]
    pub pending_spool_count: u64,
    #[prost(bool, tag = "4")]
    pub legacy_db_reachable: bool,
    #[prost(int64, tag = "5")]
    pub timestamp_unix: i64,
}

impl AgentHeartbeat {
    #[must_use]
    pub fn encode_to_vec(&self) -> Vec<u8> {
        Message::encode_to_vec(self)
    }

    /// Decodifica un [`AgentHeartbeat`] desde un slice binario Protobuf.
    ///
    /// # Errors
    /// Devuelve [`BridgeDecodeError::InvalidProtobufFrame`] si el frame es inválido.
    pub fn decode_from_slice(bytes: &[u8]) -> Result<Self, BridgeDecodeError> {
        Message::decode(bytes).map_err(|e| BridgeDecodeError::InvalidProtobufFrame(e.to_string()))
    }
}

/// Payload polimórfico (`oneof`) para el contenedor [`BridgeEnvelope`].
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ::prost::Oneof)]
pub enum BridgePayload {
    #[prost(message, tag = "1")]
    PreflightRequest(PreflightDebtCheckRequest),
    #[prost(message, tag = "2")]
    PreflightResponse(PreflightDebtCheckResponse),
    #[prost(message, tag = "3")]
    InjectPaymentCommand(InjectPaymentCommand),
    #[prost(message, tag = "4")]
    InjectPaymentAck(InjectPaymentAck),
    #[prost(message, tag = "5")]
    AccountDelta(LegacyAccountDelta),
    #[prost(message, tag = "6")]
    Heartbeat(AgentHeartbeat),
}

/// Contenedor de nivel superior para mensajes sobre el túnel bidireccional Nube <-> Municipio.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ::prost::Message)]
pub struct BridgeEnvelope {
    #[prost(oneof = "BridgePayload", tags = "1, 2, 3, 4, 5, 6")]
    pub payload: Option<BridgePayload>,
}

impl BridgeEnvelope {
    #[must_use]
    pub fn encode_to_vec(&self) -> Vec<u8> {
        Message::encode_to_vec(self)
    }

    /// Decodifica un [`BridgeEnvelope`] desde un slice binario Protobuf.
    ///
    /// # Errors
    /// Devuelve [`BridgeDecodeError::InvalidProtobufFrame`] si el frame es inválido.
    pub fn decode_from_slice(bytes: &[u8]) -> Result<Self, BridgeDecodeError> {
        Message::decode(bytes).map_err(|e| BridgeDecodeError::InvalidProtobufFrame(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_protobuf_binary_roundtrip_all_bridge_messages() -> Result<(), BridgeDecodeError> {
        let preflight_req = PreflightDebtCheckRequest {
            request_id: "req-001".to_owned(),
            tenant_id: "mun_veracruz".to_owned(),
            department: "PropertyTax".to_owned(),
            external_tax_id: "30-193-001-01-0100".to_owned(),
            expected_amount_cents: 285_000,
        };
        let encoded_req = preflight_req.encode_to_vec();
        assert_eq!(
            PreflightDebtCheckRequest::decode_from_slice(&encoded_req)?,
            preflight_req
        );

        let preflight_resp = PreflightDebtCheckResponse {
            request_id: "req-001".to_owned(),
            is_valid: true,
            current_legacy_debt_cents: 285_000,
            is_already_paid_in_window: false,
            rejection_reason: String::new(),
        };
        let encoded_resp = preflight_resp.encode_to_vec();
        assert_eq!(
            PreflightDebtCheckResponse::decode_from_slice(&encoded_resp)?,
            preflight_resp
        );

        let inject_cmd = InjectPaymentCommand {
            outbox_id: "01931111-2222-7333-8444-555555555555".to_owned(),
            payment_id: "01931111-2222-7333-8444-666666666666".to_owned(),
            tenant_id: "mun_veracruz".to_owned(),
            department: "PropertyTax".to_owned(),
            external_tax_id: "30-193-001-01-0100".to_owned(),
            idempotency_key: "idem-payment-2026-0001".to_owned(),
            amount_cents: 285_000,
            fiscal_year: 2026,
            paid_at_unix: 1_762_725_000,
        };
        let encoded_cmd = inject_cmd.encode_to_vec();
        assert_eq!(
            InjectPaymentCommand::decode_from_slice(&encoded_cmd)?,
            inject_cmd
        );

        let inject_ack = InjectPaymentAck {
            outbox_id: inject_cmd.outbox_id.clone(),
            payment_id: inject_cmd.payment_id.clone(),
            status: i32::from(InjectionAckStatus::Committed),
            legacy_receipt_folio: "LEGACY-FOLIO-mun_veracruz-2026-1".to_owned(),
            error_message: String::new(),
        };
        assert_eq!(inject_ack.ack_status(), InjectionAckStatus::Committed);
        let encoded_ack = inject_ack.encode_to_vec();
        assert_eq!(
            InjectPaymentAck::decode_from_slice(&encoded_ack)?,
            inject_ack
        );

        let delta = LegacyAccountDelta {
            tenant_id: "mun_veracruz".to_owned(),
            department: "PropertyTax".to_owned(),
            external_tax_id: "30-193-001-01-0100".to_owned(),
            owner_verification_hash: "abc123def456".to_owned(),
            cadastral_or_base_value_cents: 120_000_000,
            active_benefit: "InapamSenior".to_owned(),
            is_paid_current_year: false,
            updated_at_unix: 1_762_725_100,
            metadata_json: r#"{"zone":"centro"}"#.to_owned(),
        };
        let encoded_delta = delta.encode_to_vec();
        assert_eq!(LegacyAccountDelta::decode_from_slice(&encoded_delta)?, delta);

        let heartbeat = AgentHeartbeat {
            tenant_id: "mun_veracruz".to_owned(),
            agent_version: "0.1.0".to_owned(),
            pending_spool_count: 3,
            legacy_db_reachable: true,
            timestamp_unix: 1_762_725_200,
        };
        let encoded_hb = heartbeat.encode_to_vec();
        assert_eq!(AgentHeartbeat::decode_from_slice(&encoded_hb)?, heartbeat);

        let envelope = BridgeEnvelope {
            payload: Some(BridgePayload::InjectPaymentCommand(inject_cmd)),
        };
        let encoded_env = envelope.encode_to_vec();
        assert_eq!(BridgeEnvelope::decode_from_slice(&encoded_env)?, envelope);

        Ok(())
    }
}
