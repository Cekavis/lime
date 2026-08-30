//! Stable, minimal data contracts for the Lime local IPC channel.
//!
//! This crate intentionally contains no transport implementation. Platform adapters and
//! the core service can serialize these values over their platform-private channel in a
//! later phase.

use serde::{Deserialize, Serialize};

/// Wire protocol version used by all components in the same Lime release.
pub const PROTOCOL_VERSION: u16 = 1;

/// A client-to-service handshake.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandshakeRequest {
    pub protocol_version: u16,
}

impl Default for HandshakeRequest {
    fn default() -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
        }
    }
}

/// Service response to a handshake attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandshakeResponse {
    pub protocol_version: u16,
    pub accepted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorCode>,
}

impl HandshakeResponse {
    pub fn accepted() -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            accepted: true,
            error: None,
        }
    }

    pub fn rejected(error: ErrorCode) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            accepted: false,
            error: Some(error),
        }
    }
}

/// Minimal input request sent by a platform adapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputRequest {
    pub request_id: u64,
    pub preedit: String,
    pub preceding_text: String,
    pub context_available: bool,
    pub config_revision: u64,
}

/// Candidate data exposed to the platform adapter/UI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub display_text: String,
    pub commit_text: String,
}

/// Availability state reported for an input response.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    Ready,
    RimeOnly,
    Reloading,
    Unavailable,
}

/// Response to an input request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputResponse {
    pub request_id: u64,
    pub candidates: Vec<Candidate>,
    pub context_used: bool,
    pub service_state: ServiceState,
}

/// Settings owned by the Rust service and exchanged through management IPC.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub preceding_text_char_limit: u32,
    pub context_preview_char_limit: u32,
    pub page_size: u32,
    pub llm_rerank_count: u32,
    pub llm_effective_count: u32,
    pub llm_context_token_limit: u32,
    pub llm_enabled: bool,
    pub auto_start_service: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            preceding_text_char_limit: 128,
            context_preview_char_limit: 32,
            page_size: 9,
            llm_rerank_count: 32,
            llm_effective_count: 3,
            llm_context_token_limit: 32,
            llm_enabled: true,
            auto_start_service: false,
        }
    }
}

/// A revisioned configuration snapshot. Revisions let the service discard stale input
/// requests without pretending to provide cross-version compatibility.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigSnapshot {
    pub revision: u64,
    pub config: Config,
}

/// Stable error categories shared by IPC clients and management UI.
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
    /// Machine-readable catalog identifier (see `schemas/errors.catalog.json`).
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "LIME-0001",
            Self::ConfigValidationFailed => "LIME-0002",
            Self::ProtocolVersionMismatch => "LIME-0003",
            Self::ServiceUnavailable => "LIME-0004",
            Self::RimeInitializationFailed => "LIME-0005",
            Self::ModelLoadFailed => "LIME-0006",
            Self::ModelNotFound => "LIME-0007",
            Self::IpcTransportFailed => "LIME-0008",
            Self::RequestCancelled => "LIME-0009",
            Self::Internal => "LIME-0010",
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::ConfigValidationFailed => "config_validation_failed",
            Self::ProtocolVersionMismatch => "protocol_version_mismatch",
            Self::ServiceUnavailable => "service_unavailable",
            Self::RimeInitializationFailed => "rime_initialization_failed",
            Self::ModelLoadFailed => "model_load_failed",
            Self::ModelNotFound => "model_not_found",
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

/// Requests supported by the Phase 0 contract. The actual transport/server is deferred.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum Request {
    Handshake(HandshakeRequest),
    Input(InputRequest),
    GetConfig,
    SetConfig(Config),
}

/// Responses supported by the Phase 0 contract. Management/model/dictionary operations
/// will add variants as their service implementations land.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum Response {
    Handshake(HandshakeResponse),
    Input(InputResponse),
    Config(ConfigSnapshot),
    Error { code: ErrorCode },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_match_design_document() {
        assert_eq!(
            Config::default(),
            Config {
                preceding_text_char_limit: 128,
                context_preview_char_limit: 32,
                page_size: 9,
                llm_rerank_count: 32,
                llm_effective_count: 3,
                llm_context_token_limit: 32,
                llm_enabled: true,
                auto_start_service: false,
            }
        );
    }

    #[test]
    fn input_response_serializes_only_public_fields() {
        let response = InputResponse {
            request_id: 7,
            candidates: vec![Candidate {
                display_text: "你好".into(),
                commit_text: "你好".into(),
            }],
            context_used: true,
            service_state: ServiceState::RimeOnly,
        };
        let json = serde_json::to_string(&response).expect("serialize response");
        assert!(json.contains("rime_only"));
        assert!(!json.contains("score"));
    }

    #[test]
    fn error_codes_match_catalog_identifiers() {
        assert_eq!(ErrorCode::ConfigValidationFailed.code(), "LIME-0002");
        assert_eq!(
            ErrorCode::ConfigValidationFailed.name(),
            "config_validation_failed"
        );
        assert!(!ErrorCode::ConfigValidationFailed.retryable());
        assert!(ErrorCode::RequestCancelled.retryable());
    }
}
