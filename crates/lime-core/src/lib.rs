//! Lime Phase 1 core service primitives.

pub mod config;
pub mod engine;
pub mod error;
pub mod llama;
pub mod logging;
pub mod ranking;
pub mod service;

pub use config::{validate, validate_with, ConfigStore, ConfigValidationError, Limits};
pub use engine::{CandidateEngine, RimeEngine, RimeKeyResult};
pub use error::CoreError;
pub use lime_protocol::{
    CandidateDiagnostic, Config, ConfigSnapshot, InputHistoryEntry, InputHistoryPage,
    LlmPerformance, ModelPreset,
};
pub use llama::BackendPreference;
pub use logging::PrivacyLogger;
pub use ranking::{
    rerank_candidates, rerank_candidates_with_diagnostics, GenerationTracker, LlamaRuntime,
    RerankResult,
};
pub use service::CoreService;
