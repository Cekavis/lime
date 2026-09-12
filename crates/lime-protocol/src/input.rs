// Shared input and history wire types.
use crate::{ErrorCode, PROTOCOL_VERSION};
use serde::{Deserialize, Serialize};

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
    /// Identifies the original input request when this is only a lazy Rime
    /// candidate extension. Extension requests do not invoke LLM ranking or
    /// create a new history entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_extension_of: Option<u64>,
    /// Number of leading candidates requested by the client.
    ///
    /// Zero requests the full list for protocol clients that explicitly need it. The management
    /// test page requests the current page, while Windows TSF sends the end of the page it needs
    /// so the service can read more candidates lazily when the user pages past the initial rerank
    /// window.
    #[serde(default)]
    pub candidate_limit: u32,
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
    /// Remaining raw pinyin after selecting each candidate, aligned with `candidates`.
    ///
    /// A missing value means that the engine could not expose reliable selection metadata. An
    /// empty string means that the candidate consumes the complete composition.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidate_remainders: Vec<Option<String>>,
    pub context_used: bool,
    pub service_state: ServiceState,
    /// One row per candidate used by the management/test diagnostics table.
    pub diagnostics: Vec<CandidateDiagnostic>,
    /// Wall-clock time spent handling this request, for management diagnostics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_to_end_duration_ms: Option<u64>,
    /// Wall-clock time spent obtaining the Rime candidate batch, for management diagnostics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rime_duration_ms: Option<u64>,
    /// Optional LLM timing and workload counters for the management/test page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_performance: Option<LlmPerformance>,
}

/// A diagnostic record for one input request received by the core service.
///
/// History is explicitly requested by the management UI and is kept in memory for the
/// lifetime of the service. It is not written to the default structured log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputHistoryEntry {
    /// Request identifier assigned by the originating input client.
    pub request_id: u64,
    /// Monotonic wall-clock timestamp in Unix milliseconds.  The service guarantees newer
    /// entries have a larger value, including when several requests arrive in one millisecond.
    pub timestamp_ms: u64,
    /// Wall-clock time spent handling this input request before the history entry is recorded,
    /// including Rime and any LLM work. Older history entries leave this absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_to_end_duration_ms: Option<u64>,
    pub preceding_text: String,
    pub preedit: String,
    pub rime_candidates: Vec<Candidate>,
    pub final_candidates: Vec<Candidate>,
    pub service_state: ServiceState,
    /// Displayable identifier of the model active for this request, when one was loaded.
    /// This is normally the GGUF file name rather than the full local path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    /// Wall-clock time spent obtaining the Rime candidate batch, in milliseconds.
    /// Older payloads and requests rejected before entering Rime leave this absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rime_duration_ms: Option<u64>,
    /// Detailed rows shared by the test page and history detail page.
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
    pub rime_candidate: Option<Candidate>,
    pub llm_candidate: Option<Candidate>,
    /// Aggregate log probability for `llm_candidate`, computed by the active llama.cpp model.
    /// It is zero for Rime-only rows where no model candidate exists.
    pub logprob: f64,
    /// Per-token log probabilities from the active llama.cpp vocabulary. Their sum is the
    /// aggregate `logprob` (within floating point round-off).
    pub logprobs: Vec<f64>,
    /// Whether tokenizing `preceding_text + candidate` changes the tokenized prefix of
    /// `preceding_text`. This remains a diagnostic flag; mismatch candidates use the same scoring
    /// path as other candidates.
    pub mismatch: bool,
    pub display_candidate: Option<Candidate>,
}

/// Performance counters collected only when a request actually enters the local LLM scorer.
///
/// The values are management-facing diagnostics. They are not used by the TSF candidate path,
/// and an absent value means that no LLM inference was performed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LlmPerformance {
    /// Wall-clock time spent in the LLM scorer, in milliseconds.
    pub total_ms: u64,
    /// Time spent tokenizing the context and candidate strings, in milliseconds.
    pub tokenize_ms: u64,
    /// Time spent in native llama.cpp inference decode calls, in milliseconds.
    pub decode_ms: u64,
    /// Number of candidates passed to the scorer after all filters.
    pub candidate_count: u32,
    /// Number of candidates for which a score was returned.
    pub scored_count: u32,
    /// Number of target tokens whose log probabilities were computed.
    pub target_token_count: u32,
    /// Number of batch decode operations.
    pub batch_count: u32,
    /// Number of scored candidates whose tokenization crosses the preceding-text boundary.
    pub mismatch_count: u32,
    /// Number of tokens in the untruncated preceding-text prompt.
    pub context_token_count: u32,
    /// Number of token rows submitted to llama.cpp decode, summed across outer batches.
    pub decode_input_token_count: u32,
    /// Number of compact log-probability result rows read from llama.cpp, summed across outer batches.
    pub logits_output_count: u32,
    /// Maximum continuation inference batches allowed for this request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference_count_limit: Option<u32>,
    /// Candidates left unscored because the continuation inference limit was reached.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted_candidate_count: u32,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
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
