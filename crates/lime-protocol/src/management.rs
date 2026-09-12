// Shared configuration, model, and dictionary wire types.
use crate::{ServiceState, DEFAULT_LLM_BACKEND};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Settings owned by the Rust service and exchanged through management IPC.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Schema id from the official Rime/Ice package.
    pub rime_schema: String,
    pub preceding_text_char_limit: u32,
    pub context_preview_char_limit: u32,
    pub page_size: u32,
    pub llm_rerank_count: u32,
    pub llm_effective_count: u32,
    pub llm_context_token_limit: u32,
    /// Maximum number of candidate continuation inference batches per input request.
    #[serde(default = "default_llm_inference_count_limit")]
    pub llm_inference_count_limit: u32,
    /// Whether candidates containing Emoji code points are excluded from LLM reranking.
    #[serde(default = "default_llm_ignore_emoji")]
    pub llm_ignore_emoji: bool,
    /// Native llama.cpp backend preference.
    pub llm_backend: String,
}

fn default_rime_schema() -> String {
    "rime_ice".to_owned()
}

fn default_llm_backend() -> String {
    DEFAULT_LLM_BACKEND.to_owned()
}

fn default_llm_inference_count_limit() -> u32 {
    1
}

fn default_llm_ignore_emoji() -> bool {
    true
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
            llm_inference_count_limit: default_llm_inference_count_limit(),
            llm_ignore_emoji: default_llm_ignore_emoji(),
            llm_backend: default_llm_backend(),
        }
    }
}

/// A revisioned configuration snapshot. Revisions let the service discard stale input requests.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigSnapshot {
    pub revision: u64,
    pub config: Config,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelScoringPath {
    Attention,
    Recurrent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub path: Option<String>,
    pub size_bytes: Option<u64>,
    pub sha256: Option<String>,
    pub loaded: bool,
    /// Native candidate scoring path selected for the loaded model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scoring_path: Option<ModelScoringPath>,
    /// Memory figures reported by llama.cpp while initializing the active model.
    ///
    /// This is optional because runtimes may not expose their initialization log hooks. Clients
    /// must treat `None` (or individual `None` fields) as "unavailable", not as zero bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initialization_memory: Option<ModelMemoryInfo>,
}

/// Native memory figures collected from llama.cpp initialization diagnostics, when the runtime
/// exposes its public log callback hooks.
///
/// The values are deliberately optional: llama.cpp versions and backends differ in which
/// buffers they report, and Lime must not infer GPU usage from the GGUF file size or context
/// settings.  `total_bytes` is not necessarily the sum of the component fields when the native
/// runtime reports shared buffers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelMemoryInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compute_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// Per-buffer figures parsed from llama.cpp's initialization diagnostics.
    /// Keys are stable buffer kinds, optionally qualified by a native device
    /// name such as `cuda0.model` when the runtime reports one.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub breakdown: BTreeMap<String, u64>,
}

/// A persisted local GGUF model preset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPreset {
    pub name: String,
    pub path: String,
    pub size_bytes: Option<u64>,
    pub sha256: Option<String>,
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
