//! Stable, minimal data contracts for the Lime local IPC channel.
//!
//! This crate intentionally contains no transport implementation. Platform adapters and
//! the core service can serialize these values over their platform-private channel in a
//! later phase.

use serde::{Deserialize, Serialize};

/// Wire protocol version used by all components in the same Lime release.
pub const PROTOCOL_VERSION: u16 = 1;

/// The management UI history endpoint always returns at most this many entries per page.
///
/// Keeping the limit in the shared protocol prevents a client from accidentally requesting
/// an unbounded response over the local IPC channel.  The legacy `GetInputHistory` request is
/// retained for older clients, but new clients should use `GetInputHistoryPage`.
pub const INPUT_HISTORY_PAGE_SIZE: u32 = 100;

/// The management UI dictionary endpoint always returns at most this many entries per page.
pub const DICTIONARY_PAGE_SIZE: u32 = 100;

/// The default native llama.cpp backend.  CUDA is preferred on Windows builds; the service may
/// fall back to the CPU runtime when the CUDA runtime or a compatible GPU is unavailable.
pub const DEFAULT_LLM_BACKEND: &str = "cuda";

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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputResponse {
    pub request_id: u64,
    pub candidates: Vec<Candidate>,
    pub context_used: bool,
    pub service_state: ServiceState,
    /// One row per candidate used by the management/test diagnostics table.  This is additive
    /// so platform clients that only consume `candidates` remain source compatible.
    #[serde(default)]
    pub diagnostics: Vec<CandidateDiagnostic>,
}

/// A diagnostic record for one input request received by the core service.
///
/// History is explicitly requested by the management UI and is kept in memory for the
/// lifetime of the service. It is not written to the default structured log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputHistoryEntry {
    /// Kept for wire compatibility with the original history API.  The UI must not use this as
    /// the sort key; `timestamp_ms` is monotonic within a service lifetime and is the canonical
    /// ordering field.
    #[serde(default)]
    pub request_id: u64,
    /// Monotonic wall-clock timestamp in Unix milliseconds.  The service guarantees newer
    /// entries have a larger value, including when several requests arrive in one millisecond.
    #[serde(default)]
    pub timestamp_ms: u64,
    pub preceding_text: String,
    pub preedit: String,
    pub rime_candidates: Vec<Candidate>,
    pub final_candidates: Vec<Candidate>,
    pub service_state: ServiceState,
    /// Detailed rows shared by the test page and history detail page.
    #[serde(default)]
    pub diagnostics: Vec<CandidateDiagnostic>,
    /// Optional LLM timing and workload counters for this request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_performance: Option<LlmPerformance>,
}

/// A row in the candidate diagnostics table.
///
/// `rime_candidate` and `llm_candidate` are intentionally optional because a row can exist in
/// one ordering but not the other (for example when a model only reranks a bounded prefix).
/// `display_candidate` is the candidate at the corresponding final display position.  The
/// `rank` field is one-based and is supplied by the service so clients do not have to infer
/// ordering after filtering empty rows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CandidateDiagnostic {
    pub rank: u32,
    #[serde(default)]
    pub rime_candidate: Option<Candidate>,
    #[serde(default)]
    pub llm_candidate: Option<Candidate>,
    /// Aggregate log probability for `llm_candidate`, computed by the active llama.cpp model.
    /// It is zero for Rime-only rows where no model candidate exists.
    #[serde(default)]
    pub logprob: f64,
    /// Per-token log probabilities from the active llama.cpp vocabulary. Their sum is the
    /// aggregate `logprob` (within floating point round-off).
    #[serde(default)]
    pub logprobs: Vec<f64>,
    /// Whether a llama.cpp tokenizer token spans the boundary between `preceding_text` and the
    /// candidate. This is useful for diagnosing why a candidate cannot be scored as an independent
    /// continuation.
    #[serde(default)]
    pub mismatch: bool,
    #[serde(default)]
    pub display_candidate: Option<Candidate>,
}

/// Performance counters collected only when a request actually enters the local LLM scorer.
///
/// The values are management-facing diagnostics. They are not used by the TSF candidate path,
/// and an absent value means that no LLM inference was performed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LlmPerformance {
    /// Wall-clock time spent in the LLM scorer, in milliseconds.
    #[serde(default)]
    pub total_ms: u64,
    /// Time spent tokenizing the context and candidate strings, in milliseconds.
    #[serde(default)]
    pub tokenize_ms: u64,
    /// Time spent in native llama.cpp decode calls, in milliseconds.
    #[serde(default)]
    pub decode_ms: u64,
    /// Time spent reading logits and deriving token log probabilities, in milliseconds.
    #[serde(default)]
    pub logits_ms: u64,
    /// Number of candidates passed to the scorer after all filters.
    #[serde(default)]
    pub candidate_count: u32,
    /// Number of candidates for which a score was returned.
    #[serde(default)]
    pub scored_count: u32,
    /// Number of target tokens whose log probabilities were computed.
    #[serde(default)]
    pub target_token_count: u32,
    /// Number of batch or fallback decode operations.
    #[serde(default)]
    pub batch_count: u32,
    /// Number of candidates that used the tokenizer-boundary fallback path.
    #[serde(default)]
    pub mismatch_count: u32,
    /// Number of tokens in the untruncated preceding-text prompt.
    #[serde(default)]
    pub context_token_count: u32,
    /// Number of token rows submitted to llama.cpp decode, summed across outer batches.
    #[serde(default)]
    pub decode_input_token_count: u32,
    /// Number of logits rows read from llama.cpp, summed across outer batches.
    #[serde(default)]
    pub logits_output_count: u32,
}

/// A bounded, newest-first history page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputHistoryPage {
    pub items: Vec<InputHistoryEntry>,
    pub total: u64,
    /// One-based page index.  Values below one are normalized to one by the service.
    pub page: u32,
    pub page_size: u32,
}

/// Settings owned by the Rust service and exchanged through management IPC.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Schema id from the official Rime/Ice package.  Missing fields in older
    /// config files continue to use the published full-pinyin schema.
    #[serde(default = "default_rime_schema")]
    pub rime_schema: String,
    pub preceding_text_char_limit: u32,
    pub context_preview_char_limit: u32,
    pub page_size: u32,
    pub llm_rerank_count: u32,
    pub llm_effective_count: u32,
    pub llm_context_token_limit: u32,
    /// Native llama.cpp backend preference.  Missing fields in pre-CUDA config files deserialize
    /// as `cuda`; the service accepts `cuda` and `cpu` only.
    #[serde(default = "default_llm_backend")]
    pub llm_backend: String,
    pub llm_enabled: bool,
    pub auto_start_service: bool,
}

fn default_rime_schema() -> String {
    "rime_ice".to_owned()
}

fn default_llm_backend() -> String {
    DEFAULT_LLM_BACKEND.to_owned()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            rime_schema: default_rime_schema(),
            preceding_text_char_limit: 128,
            context_preview_char_limit: 32,
            page_size: 9,
            llm_rerank_count: 32,
            llm_effective_count: 3,
            llm_context_token_limit: 1024,
            llm_backend: default_llm_backend(),
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub path: Option<String>,
    pub size_bytes: Option<u64>,
    pub sha256: Option<String>,
    pub loaded: bool,
}

/// A persisted local GGUF model preset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPreset {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub loaded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub state: ServiceState,
    pub config: ConfigSnapshot,
    pub model: ModelInfo,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictionaryEntry {
    pub pinyin: String,
    pub text: String,
    pub weight: i64,
}

/// A bounded, newest-as-exported-order dictionary page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictionaryPage {
    pub items: Vec<DictionaryEntry>,
    pub total: u64,
    /// One-based page index. Values below one are normalized to one by the service.
    pub page: u32,
    pub page_size: u32,
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

/// Requests supported by the local service contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum Request {
    Handshake(HandshakeRequest),
    Input(InputRequest),
    GetConfig,
    SetConfig(Config),
    GetStatus,
    LoadModel {
        path: String,
    },
    UnloadModel,
    ListModelPresets,
    SaveModelPreset {
        name: String,
        path: String,
    },
    DeleteModelPreset {
        name: String,
    },
    SelectModelPreset {
        name: String,
    },
    Learn {
        pinyin: String,
        text: String,
    },
    ExportDictionary,
    GetDictionaryPage {
        page: u32,
        page_size: u32,
    },
    ImportDictionary {
        entries: Vec<DictionaryEntry>,
    },
    ClearDictionary,
    GetInputHistory,
    GetInputHistoryPage {
        page: u32,
        page_size: u32,
    },
    /// Wait until the in-memory input history revision differs from `revision`.
    ///
    /// Management clients use this long-poll request instead of relying on a
    /// throttled browser timer, so history updates are observable while the
    /// management window is in the background. The service may return the same
    /// revision after a keepalive timeout so clients can reconnect cleanly.
    WaitForInputHistory {
        revision: u64,
    },
    ClearInputHistory,
}

/// Responses supported by the local service contract, including Phase 1 management APIs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum Response {
    Handshake(HandshakeResponse),
    Input(InputResponse),
    Config(ConfigSnapshot),
    Status(ServiceStatus),
    ModelPresets(Vec<ModelPreset>),
    ModelPreset(ModelPreset),
    Dictionary(Vec<DictionaryEntry>),
    DictionaryPage(DictionaryPage),
    InputHistory(Vec<InputHistoryEntry>),
    InputHistoryPage(InputHistoryPage),
    InputHistoryRevision(u64),
    Accepted,
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
                rime_schema: "rime_ice".into(),
                preceding_text_char_limit: 128,
                context_preview_char_limit: 32,
                page_size: 9,
                llm_rerank_count: 32,
                llm_effective_count: 3,
                llm_context_token_limit: 1024,
                llm_backend: "cuda".into(),
                llm_enabled: true,
                auto_start_service: false,
            }
        );
    }

    #[test]
    fn legacy_config_without_backend_uses_cuda_default() {
        let value: Config = serde_json::from_str(
            r#"{
                "rime_schema":"rime_ice",
                "preceding_text_char_limit":128,
                "context_preview_char_limit":32,
                "page_size":9,
                "llm_rerank_count":32,
                "llm_effective_count":3,
                "llm_context_token_limit":1024,
                "llm_enabled":true,
                "auto_start_service":false
            }"#,
        )
        .expect("legacy config should deserialize");
        assert_eq!(value.llm_backend, DEFAULT_LLM_BACKEND);
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
            diagnostics: Vec::new(),
        };
        let json = serde_json::to_string(&response).expect("serialize response");
        assert!(json.contains("rime_only"));
        assert!(!json.contains("score"));
    }

    #[test]
    fn old_input_response_without_diagnostics_remains_readable() {
        let value: InputResponse = serde_json::from_str(
            r#"{"request_id":1,"candidates":[],"context_used":false,"service_state":"rime_only"}"#,
        )
        .expect("deserialize legacy response");
        assert!(value.diagnostics.is_empty());
    }

    #[test]
    fn diagnostic_logprobs_round_trip() {
        let row = CandidateDiagnostic {
            rank: 1,
            rime_candidate: None,
            llm_candidate: Some(Candidate {
                display_text: "你好".into(),
                commit_text: "你好".into(),
            }),
            logprob: -0.01,
            logprobs: vec![-0.01, 0.0],
            mismatch: false,
            display_candidate: None,
        };
        let json = serde_json::to_string(&row).expect("serialize diagnostic");
        let decoded: CandidateDiagnostic =
            serde_json::from_str(&json).expect("deserialize diagnostic");
        assert_eq!(decoded, row);
    }

    #[test]
    fn llm_performance_round_trip_and_legacy_history_default() {
        let performance = LlmPerformance {
            total_ms: 17,
            tokenize_ms: 2,
            decode_ms: 11,
            logits_ms: 3,
            candidate_count: 4,
            scored_count: 4,
            target_token_count: 9,
            batch_count: 2,
            mismatch_count: 1,
            context_token_count: 6,
            decode_input_token_count: 18,
            logits_output_count: 12,
        };
        let json = serde_json::to_string(&performance).expect("serialize performance");
        let decoded: LlmPerformance = serde_json::from_str(&json).expect("deserialize performance");
        assert_eq!(decoded, performance);

        let legacy: InputHistoryEntry = serde_json::from_str(
            r#"{
                "request_id":1,
                "timestamp_ms":2,
                "preceding_text":"前文",
                "preedit":"nihao",
                "rime_candidates":[],
                "final_candidates":[],
                "service_state":"rime_only",
                "diagnostics":[]
            }"#,
        )
        .expect("legacy history should deserialize");
        assert!(legacy.llm_performance.is_none());
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

    #[test]
    fn history_watch_messages_round_trip() {
        let request = Request::WaitForInputHistory { revision: 12 };
        let json = serde_json::to_string(&request).expect("serialize history watch request");
        let decoded: Request =
            serde_json::from_str(&json).expect("deserialize history watch request");
        assert_eq!(decoded, request);

        let response = Response::InputHistoryRevision(13);
        let json = serde_json::to_string(&response).expect("serialize history revision response");
        let decoded: Response =
            serde_json::from_str(&json).expect("deserialize history revision response");
        assert_eq!(decoded, response);
    }

    #[test]
    fn dictionary_page_round_trip() {
        let response = Response::DictionaryPage(DictionaryPage {
            items: vec![DictionaryEntry {
                pinyin: "nihao".into(),
                text: "你好".into(),
                weight: 1,
            }],
            total: 101,
            page: 2,
            page_size: DICTIONARY_PAGE_SIZE,
        });
        let json = serde_json::to_string(&response).expect("serialize dictionary page");
        let decoded: Response = serde_json::from_str(&json).expect("deserialize dictionary page");
        assert_eq!(decoded, response);
    }
}
