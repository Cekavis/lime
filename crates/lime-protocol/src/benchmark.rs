//! Management-facing benchmark messages.

use crate::Config;
use lime_benchmark::{Dataset, InputMode, Report};
use serde::{Deserialize, Serialize};

pub use lime_benchmark::{Case, Observation, Summary};

/// A request to run one or more modes over selected built-in dataset categories.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchmarkRunRequest {
    #[serde(default)]
    pub modes: Vec<InputMode>,
    #[serde(default)]
    pub categories: Vec<String>,
}

impl Default for BenchmarkRunRequest {
    fn default() -> Self {
        Self {
            modes: vec![InputMode::Full, InputMode::Initials],
            categories: Vec::new(),
        }
    }
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
pub struct BenchmarkRunState {
    pub status: BenchmarkRunStatus,
    pub dataset_id: String,
    pub dataset_name: String,
    pub dataset_version: u32,
    pub config_revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<Config>,
    pub total: u32,
    pub completed: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<Report>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl BenchmarkRunState {
    pub fn idle(dataset: &Dataset) -> Self {
        Self {
            status: BenchmarkRunStatus::Idle,
            dataset_id: dataset.id.clone(),
            dataset_name: dataset.name.clone(),
            dataset_version: dataset.version,
            config_revision: 0,
            model_name: None,
            model_sha256: None,
            config: None,
            total: 0,
            completed: 0,
            report: None,
            error: None,
        }
    }
}
