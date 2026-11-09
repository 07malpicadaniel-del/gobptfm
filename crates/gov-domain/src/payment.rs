use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::DomainError;

/// Máquina de estados determinista del ciclo de vida de un pago fiscal.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PaymentStatus {
    /// Cotización generada, pendiente de pago o autorización bancaria.
    Quoted,
    /// Cobro autorizado y confirmado mediante webhook bancario / SPEI / pasarela.
    Authorized,
    /// Pago inyectado y confirmado en el sistema legacy municipal vía `gov-edge-agent`.
    SyncedToMunicipality,
    /// Conciliado contra dispersión bancaria en tesorería.
    Reconciled,
    /// Intento de cobro rechazado o expirado sin cargo.
    Failed,
    /// Contracargo o cancelación registrada mediante contra-asiento compensatorio (`Reversal`).
    Reversed,
}

impl PaymentStatus {
    /// Evalúa y ejecuta una transición de estado estricta.
    ///
    /// Transiciones legales permitidas:
    /// - `Quoted -> Authorized | Failed`
    /// - `Authorized -> SyncedToMunicipality | Reversed`
    /// - `SyncedToMunicipality -> Reconciled | Reversed`
    ///
    /// # Errors
    /// Devuelve [`DomainError::InvalidStateTransition`] ante cualquier salto de estado no permitido.
    pub fn transition_to(self, next: Self) -> Result<Self, DomainError> {
        let is_valid = matches!(
            (self, next),
            (Self::Quoted, Self::Authorized | Self::Failed)
                | (
                    Self::Authorized,
                    Self::SyncedToMunicipality | Self::Reversed
                )
                | (
                    Self::SyncedToMunicipality,
                    Self::Reconciled | Self::Reversed
                )
        );

        if is_valid {
            Ok(next)
        } else {
            Err(DomainError::InvalidStateTransition {
                from: self.to_string(),
                to: next.to_string(),
            })
        }
    }
}

impl fmt::Display for PaymentStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Quoted => "Quoted",
            Self::Authorized => "Authorized",
            Self::SyncedToMunicipality => "SyncedToMunicipality",
            Self::Reconciled => "Reconciled",
            Self::Failed => "Failed",
            Self::Reversed => "Reversed",
        };
        f.write_str(label)
    }
}

/// Estado de sincronización asíncrona en la tabla `sync_outbox` hacia el agente municipal.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SyncOutboxStatus {
    /// Pendiente de envío por el canal gRPC hacia el municipio.
    PendingSync,
    /// Enviado al agente municipal; esperando ACK con folio local.
    InFlight,
    /// Confirmado y aplicado en la base de datos legacy del municipio.
    CommittedRemote,
    /// Conflicto detectado (ej. pago duplicado en caja física); requiere revisión manual/tesorería.
    ConflictManualReview,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_payment_status_transitions() {
        assert_eq!(
            PaymentStatus::Quoted.transition_to(PaymentStatus::Authorized),
            Ok(PaymentStatus::Authorized)
        );
        assert_eq!(
            PaymentStatus::Quoted.transition_to(PaymentStatus::Failed),
            Ok(PaymentStatus::Failed)
        );
        assert_eq!(
            PaymentStatus::Authorized.transition_to(PaymentStatus::SyncedToMunicipality),
            Ok(PaymentStatus::SyncedToMunicipality)
        );
        assert_eq!(
            PaymentStatus::Authorized.transition_to(PaymentStatus::Reversed),
            Ok(PaymentStatus::Reversed)
        );
        assert_eq!(
            PaymentStatus::SyncedToMunicipality.transition_to(PaymentStatus::Reconciled),
            Ok(PaymentStatus::Reconciled)
        );
        assert_eq!(
            PaymentStatus::SyncedToMunicipality.transition_to(PaymentStatus::Reversed),
            Ok(PaymentStatus::Reversed)
        );
    }

    #[test]
    fn test_illegal_payment_status_transitions_are_blocked() {
        assert_eq!(
            PaymentStatus::Quoted.transition_to(PaymentStatus::Reconciled),
            Err(DomainError::InvalidStateTransition {
                from: "Quoted".to_owned(),
                to: "Reconciled".to_owned(),
            })
        );
        assert_eq!(
            PaymentStatus::Quoted.transition_to(PaymentStatus::SyncedToMunicipality),
            Err(DomainError::InvalidStateTransition {
                from: "Quoted".to_owned(),
                to: "SyncedToMunicipality".to_owned(),
            })
        );
        assert_eq!(
            PaymentStatus::Failed.transition_to(PaymentStatus::Authorized),
            Err(DomainError::InvalidStateTransition {
                from: "Failed".to_owned(),
                to: "Authorized".to_owned(),
            })
        );
        assert_eq!(
            PaymentStatus::Reconciled.transition_to(PaymentStatus::Quoted),
            Err(DomainError::InvalidStateTransition {
                from: "Reconciled".to_owned(),
                to: "Quoted".to_owned(),
            })
        );
    }
}
