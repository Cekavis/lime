//! Management-facing batch benchmark messages.

use crate::Config;
use lime_benchmark::{DatasetInfo, InputMode, Report};
use serde::{Deserialize, Serialize};

pub use lime_benchmark::{Case, Observation, Summary};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkConfiguration {
    pub llm_rerank_count: u32,
    pub preceding_text_char_limit: u32,
}

/// Every model/configuration/mode combination covers all built-in corpora.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkRunRequest {
    pub modes: Vec<InputMode>,
    pub models: Vec<String>,
    pub configurations: Vec<BenchmarkConfiguration>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkRunStatus {
    Idle,
    Running,
    Stopping,
    Cancelled,
    Completed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkResult {
    pub id: String,
    pub model_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_sha256: Option<String>,
    pub configuration: BenchmarkConfiguration,
    pub mode: InputMode,
    pub config: Config,
    pub status: BenchmarkRunStatus,
    /// Summaries cover every target; observations contain bounded error examples.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<Report>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkRunState {
    pub status: BenchmarkRunStatus,
    pub dataset_id: String,
    pub dataset_name: String,
    pub dataset_version: u32,
    pub config_revision: u64,
    /// Fingerprint of the fixed Rime candidate pool shared by the entire batch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rime_snapshot_sha256: Option<String>,
    pub total: u32,
    pub completed: u32,
    pub results: Vec<BenchmarkResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl BenchmarkRunState {
    pub fn idle(dataset: &DatasetInfo) -> Self {
        Self {
            status: BenchmarkRunStatus::Idle,
            dataset_id: dataset.id.clone(),
            dataset_name: dataset.name.clone(),
            dataset_version: dataset.version,
            config_revision: 0,
            rime_snapshot_sha256: None,
            total: 0,
            completed: 0,
            results: Vec::new(),
            error: None,
        }
    }
}
