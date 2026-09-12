// Stable error categories shared across Lime components.

use serde::{Deserialize, Serialize};

/// Stable error categories shared by IPC clients and the management UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    ProtocolVersionMismatch,
    ConfigValidationFailed,
    ServiceUnavailable,
    RimeInitializationFailed,
    ModelLoadFailed,
    ModelNotFound,
    ModelUnsupported,
    IpcTransportFailed,
    RequestCancelled,
    Internal,
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl ErrorCode {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "LIME-0001",
            Self::ConfigValidationFailed => "LIME-0002",
            Self::ProtocolVersionMismatch => "LIME-0003",
            Self::ServiceUnavailable => "LIME-0004",
            Self::RimeInitializationFailed => "LIME-0005",
            Self::ModelLoadFailed => "LIME-0006",
            Self::ModelNotFound => "LIME-0007",
            Self::ModelUnsupported => "LIME-0011",
            Self::IpcTransportFailed => "LIME-0008",
            Self::RequestCancelled => "LIME-0009",
            Self::Internal => "LIME-0010",
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::ProtocolVersionMismatch => "protocol_version_mismatch",
            Self::ConfigValidationFailed => "config_validation_failed",
            Self::ServiceUnavailable => "service_unavailable",
            Self::RimeInitializationFailed => "rime_initialization_failed",
            Self::ModelLoadFailed => "model_load_failed",
            Self::ModelNotFound => "model_not_found",
            Self::ModelUnsupported => "model_unsupported",
            Self::IpcTransportFailed => "ipc_transport_failed",
            Self::RequestCancelled => "request_cancelled",
            Self::Internal => "internal",
        }
    }

    pub const fn retryable(self) -> bool {
        matches!(
            self,
            Self::ServiceUnavailable | Self::IpcTransportFailed | Self::RequestCancelled
        )
    }
}
