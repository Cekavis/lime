//! Stable, minimal data contracts for the Lime local IPC channel.
//!
//! The protocol crate contains only serializable wire values. Transport framing lives in
//! `lime-ipc`; platform adapters and the service own their transport implementations.

/// Wire protocol version used by all components in the same Lime release.
pub const PROTOCOL_VERSION: u16 = 1;

/// The management UI history endpoint always returns at most this many entries per page.
pub const INPUT_HISTORY_PAGE_SIZE: u32 = 100;

/// The management UI dictionary endpoint always returns at most this many entries per page.
pub const DICTIONARY_PAGE_SIZE: u32 = 100;

/// The default native llama.cpp backend.
pub const DEFAULT_LLM_BACKEND: &str = "cuda";

mod error;
mod input;
mod management;
mod request;

pub use error::ErrorCode;
pub use input::{
    Candidate, CandidateDiagnostic, HandshakeRequest, HandshakeResponse, InputHistoryEntry,
    InputHistoryPage, InputRequest, InputResponse, LlmPerformance, ServiceState,
};
pub use management::{
    Config, ConfigSnapshot, DictionaryEntry, DictionaryPage, ModelInfo, ModelMemoryInfo,
    ModelPreset, ModelScoringPath, ServiceStatus,
};
pub use request::{Request, Response};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

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
                llm_inference_count_limit: 1,
                llm_backend: "cuda".into(),
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
            candidate_remainders: vec![Some(String::new())],
            context_used: true,
            service_state: ServiceState::RimeOnly,
            diagnostics: Vec::new(),
            end_to_end_duration_ms: Some(29),
            rime_duration_ms: Some(13),
            llm_performance: Some(LlmPerformance {
                total_ms: 17,
                ..LlmPerformance::default()
            }),
        };
        let json = serde_json::to_string(&response).expect("serialize response");
        assert!(json.contains("rime_only"));
        assert!(json.contains(r#""end_to_end_duration_ms":29"#));
        assert!(json.contains(r#""rime_duration_ms":13"#));
        assert!(json.contains(r#""llm_performance":{"total_ms":17"#));
        assert!(json.contains(r#""candidate_remainders":[""]"#));
        assert!(!json.contains(r#""score":"#));
    }

    #[test]
    fn model_info_round_trip_with_initialization_memory() {
        let value = ModelInfo {
            path: Some("model.gguf".into()),
            size_bytes: Some(42),
            sha256: None,
            loaded: true,
            scoring_path: Some(ModelScoringPath::Attention),
            initialization_memory: Some(ModelMemoryInfo {
                model_bytes: Some(1),
                context_bytes: Some(2),
                compute_bytes: None,
                total_bytes: Some(3),
                backend: Some("cuda".into()),
                breakdown: BTreeMap::from([(String::from("cuda0.model"), 4)]),
            }),
        };
        let json = serde_json::to_string(&value).expect("serialize model status");
        assert!(json.contains(r#""scoring_path":"attention""#));
        let decoded: ModelInfo = serde_json::from_str(&json).expect("deserialize model status");
        assert_eq!(decoded, value);
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
    fn llm_performance_round_trip() {
        let performance = LlmPerformance {
            total_ms: 17,
            tokenize_ms: 2,
            decode_ms: 11,
            candidate_count: 4,
            scored_count: 4,
            target_token_count: 9,
            batch_count: 2,
            mismatch_count: 1,
            context_token_count: 6,
            decode_input_token_count: 18,
            logits_output_count: 12,
            inference_count_limit: Some(1),
            omitted_candidate_count: 0,
        };
        let json = serde_json::to_string(&performance).expect("serialize performance");
        assert!(!json.contains("logits_ms"));
        let decoded: LlmPerformance = serde_json::from_str(&json).expect("deserialize performance");
        assert_eq!(decoded, performance);
    }

    #[test]
    fn history_model_and_rime_timing_round_trip() {
        let entry = InputHistoryEntry {
            request_id: 7,
            timestamp_ms: 8,
            end_to_end_duration_ms: Some(29),
            preceding_text: "上文".into(),
            preedit: "nihao".into(),
            rime_candidates: Vec::new(),
            final_candidates: Vec::new(),
            service_state: ServiceState::Ready,
            model_name: Some("demo.gguf".into()),
            rime_duration_ms: Some(13),
            diagnostics: Vec::new(),
            llm_performance: None,
        };
        let json = serde_json::to_string(&entry).expect("serialize history entry");
        assert!(json.contains(r#""model_name":"demo.gguf""#));
        assert!(json.contains(r#""rime_duration_ms":13"#));
        assert!(json.contains(r#""end_to_end_duration_ms":29"#));
        let decoded: InputHistoryEntry =
            serde_json::from_str(&json).expect("deserialize history entry");
        assert_eq!(decoded, entry);
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
    fn management_payloads_are_explicit_and_current() {
        let request = Request::GetDictionaryPage {
            page: 1,
            page_size: DICTIONARY_PAGE_SIZE,
        };
        let json = serde_json::to_value(&request).expect("serialize management request");
        assert_eq!(json["kind"], "get_dictionary_page");
        assert_eq!(json["payload"]["page"], 1);
        assert_eq!(json["payload"]["page_size"], DICTIONARY_PAGE_SIZE);

        let rename = Request::RenameModelPreset {
            name: "old".into(),
            new_name: "new".into(),
        };
        let json = serde_json::to_value(&rename).expect("serialize rename request");
        assert_eq!(json["kind"], "rename_model_preset");
        assert_eq!(json["payload"]["name"], "old");
        assert_eq!(json["payload"]["new_name"], "new");

        let response = Response::Error {
            code: ErrorCode::ModelNotFound,
        };
        let json = serde_json::to_value(&response).expect("serialize management response");
        assert_eq!(json["kind"], "error");
        assert_eq!(json["payload"]["code"], "model_not_found");
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
