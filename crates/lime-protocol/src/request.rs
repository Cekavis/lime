// IPC request and response envelopes.

use serde::{Deserialize, Serialize};

use crate::{
    BenchmarkRunRequest, Config, ConfigSnapshot, DictionaryEntry, DictionaryPage, ErrorCode,
    HandshakeRequest, HandshakeResponse, InputHistoryPage, InputRequest, InputResponse,
    ModelPreset, ServiceStatus,
};

/// Requests supported by the local service contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum Request {
    Handshake(HandshakeRequest),
    Input(InputRequest),
    GetConfig,
    SetConfig(Config),
    GetStatus,
    GetWeaselTheme,
    LoadModel { path: String },
    UnloadModel,
    ListModelPresets,
    SaveModelPreset { name: String, path: String },
    RenameModelPreset { name: String, new_name: String },
    DeleteModelPreset { name: String },
    SelectModelPreset { name: String },
    Learn { pinyin: String, text: String },
    ExportDictionary,
    GetDictionaryPage { page: u32, page_size: u32 },
    ImportDictionary { entries: Vec<DictionaryEntry> },
    ClearDictionary,
    GetInputHistoryPage { page: u32, page_size: u32 },
    WaitForInputHistory { revision: u64 },
    ClearInputHistory,
    GetBenchmarkDataset,
    StartBenchmark(BenchmarkRunRequest),
    StopBenchmark,
    GetBenchmarkStatus,
}

/// Responses supported by the local service contract.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum Response {
    Handshake(HandshakeResponse),
    Input(InputResponse),
    Config(ConfigSnapshot),
    Status(ServiceStatus),
    WeaselTheme { base: String, custom: String },
    ModelPresets(Vec<ModelPreset>),
    ModelPreset(ModelPreset),
    Dictionary(Vec<DictionaryEntry>),
    DictionaryPage(DictionaryPage),
    InputHistoryPage(InputHistoryPage),
    InputHistoryRevision(u64),
    BenchmarkDataset(lime_benchmark::Dataset),
    BenchmarkState(crate::BenchmarkRunState),
    Accepted,
    Error { code: ErrorCode },
}
