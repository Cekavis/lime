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

/// A model selected for a benchmark row.
///
/// `RimeOnly` is a first-class choice so clients do not need to reserve a model
/// name for the no-LLM path.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BenchmarkModelSelection {
    RimeOnly,
    Preset { name: String },
}

impl From<&str> for BenchmarkModelSelection {
    fn from(value: &str) -> Self {
        Self::preset(value)
    }
}

impl From<String> for BenchmarkModelSelection {
    fn from(value: String) -> Self {
        Self::preset(value)
    }
}

impl BenchmarkModelSelection {
    pub fn preset(name: impl Into<String>) -> Self {
        Self::Preset { name: name.into() }
    }

    pub fn display_name(&self) -> &str {
        match self {
            Self::RimeOnly => "仅 Rime",
            Self::Preset { name } => name,
        }
    }
}

/// One model/configuration/mode row. Each selected corpus is evaluated as a
/// separate result cell so completed cells can be reused independently.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkRunRequest {
    pub modes: Vec<InputMode>,
    pub models: Vec<BenchmarkModelSelection>,
    pub configurations: Vec<BenchmarkConfiguration>,
    pub corpora: Vec<String>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkCellStatus {
    Pending,
    Running,
    Cancelled,
    Completed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkResultCell {
    /// Stable persistence key for this model/configuration/mode/corpus tuple.
    pub key: String,
    pub corpus_id: String,
    pub status: BenchmarkCellStatus,
    pub total: u32,
    pub completed: u32,
    pub correct: u32,
    pub no_prediction: u32,
    pub errors: u32,
    pub accuracy: Option<f64>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkResult {
    pub id: String,
    pub model: BenchmarkModelSelection,
    pub model_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_sha256: Option<String>,
    pub configuration: BenchmarkConfiguration,
    pub mode: InputMode,
    pub config: Config,
    pub status: BenchmarkRunStatus,
    pub cells: Vec<BenchmarkResultCell>,
    /// Kept for compatibility with old clients. New clients read cell summaries
    /// and fetch full errors through `GetBenchmarkErrors`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<Report>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkProgress {
    pub key: String,
    pub model_name: String,
    pub corpus_id: String,
    pub completed: u32,
    pub total: u32,
    pub rate_per_second: Option<f64>,
    pub eta_seconds: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkQueueItem {
    pub key: String,
    pub model_name: String,
    pub corpus_id: String,
    pub status: BenchmarkCellStatus,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkErrorPage {
    pub key: String,
    pub items: Vec<Observation>,
    pub page: u32,
    pub page_size: u32,
    pub total: u32,
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
    pub current: Option<BenchmarkProgress>,
    pub queue: Vec<BenchmarkQueueItem>,
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
            current: None,
            queue: Vec::new(),
            error: None,
        }
    }
}
