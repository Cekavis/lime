use super::persistence::{VersionedModelPresets, MODEL_PRESETS_FILE_VERSION};
use super::*;
use lime_protocol::Config;

#[test]
fn requested_candidate_limit_only_controls_visible_prefix() {
    assert_eq!(visible_candidate_count(32, 9), 9);
    assert_eq!(visible_candidate_count(5, 9), 5);
    assert_eq!(visible_candidate_count(36, 36), 36);
    assert_eq!(visible_candidate_count(32, 0), 32);
}

#[test]
fn candidate_extension_reuses_cached_final_order_and_remainders() {
    let candidate = |text: &str| lime_protocol::Candidate {
        display_text: text.into(),
        commit_text: text.into(),
    };
    let raw = vec![candidate("甲"), candidate("乙"), candidate("丙")];
    let cached = CandidateCacheEntry {
        preedit: "jia".into(),
        preceding_text: String::new(),
        candidates: vec![candidate("丙"), candidate("甲")],
        candidate_remainders: vec![Some("cached".into()), Some("cached".into())],
    };
    let (ordered, remainders) =
        merge_cached_candidate_order(&cached, &raw, &[Some("a".into()), Some("b".into()), None]);
    assert_eq!(
        ordered
            .iter()
            .map(|candidate| candidate.commit_text.as_str())
            .collect::<Vec<_>>(),
        vec!["丙", "甲", "乙"]
    );
    assert_eq!(
        remainders,
        vec![Some("cached".into()), Some("a".into()), Some("b".into())]
    );
}

#[test]
fn service_without_packaged_rime_is_unavailable() {
    let service = CoreService::default();
    match service.handle(Request::GetStatus) {
        Response::Status(status) => assert_eq!(status.state, ServiceState::Unavailable),
        _ => panic!("unexpected status response"),
    }
    let response = service.handle(Request::Input(InputRequest {
        request_id: 1,
        preedit: "nihao".into(),
        preceding_text: String::new(),
        context_available: false,
        config_revision: 0,
        candidate_extension_of: None,
        candidate_limit: 0,
    }));
    assert_eq!(
        response,
        Response::Error {
            code: ErrorCode::RimeInitializationFailed
        }
    );
    let page = match service.handle(Request::GetInputHistoryPage {
        page: 1,
        page_size: 1,
    }) {
        Response::InputHistoryPage(page) => page,
        other => panic!("unexpected history response: {other:?}"),
    };
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].preedit, "nihao");
    assert!(page.items[0].rime_candidates.is_empty());
    assert!(page.items[0].end_to_end_duration_ms.is_some());
}

#[test]
fn history_records_model_name_and_rime_duration() {
    let service = CoreService::default();
    let request = InputRequest {
        request_id: 9,
        preedit: "nihao".into(),
        preceding_text: "上文".into(),
        context_available: true,
        config_revision: 0,
        candidate_extension_of: None,
        candidate_limit: 0,
    };
    service.record_input_history(InputHistoryRecord {
        request: &request,
        rime_candidates: Vec::new(),
        final_candidates: Vec::new(),
        diagnostics: Vec::new(),
        service_state: ServiceState::Ready,
        model_name: Some("demo.gguf".into()),
        rime_duration_ms: Some(17),
        llm_performance: None,
        end_to_end_duration_ms: Some(23),
    });

    let history = match service.handle(Request::GetInputHistoryPage {
        page: 1,
        page_size: 100,
    }) {
        Response::InputHistoryPage(page) => page.items,
        other => panic!("unexpected history response: {other:?}"),
    };
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].model_name.as_deref(), Some("demo.gguf"));
    assert_eq!(history[0].rime_duration_ms, Some(17));
    assert_eq!(history[0].end_to_end_duration_ms, Some(23));
}

#[test]
fn candidate_extension_appends_to_existing_history_without_new_row_or_performance() {
    let service = CoreService::default();
    let initial_request = InputRequest {
        request_id: 7,
        preedit: "nihao".into(),
        preceding_text: "上文".into(),
        context_available: true,
        config_revision: 0,
        candidate_extension_of: None,
        candidate_limit: 32,
    };
    let first_candidates = vec![
        lime_protocol::Candidate {
            display_text: "你好".into(),
            commit_text: "你好".into(),
        },
        lime_protocol::Candidate {
            display_text: "拟好".into(),
            commit_text: "拟好".into(),
        },
    ];
    service.record_input_history(InputHistoryRecord {
        request: &initial_request,
        rime_candidates: first_candidates.clone(),
        final_candidates: first_candidates.clone(),
        diagnostics: first_candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| CandidateDiagnostic {
                rank: (index + 1) as u32,
                rime_candidate: Some(candidate.clone()),
                llm_candidate: None,
                logprob: 0.0,
                logprobs: Vec::new(),
                mismatch: false,
                display_candidate: Some(candidate.clone()),
            })
            .collect(),
        service_state: ServiceState::Ready,
        model_name: Some("demo.gguf".into()),
        rime_duration_ms: Some(7),
        llm_performance: Some(LlmPerformance {
            total_ms: 11,
            ..LlmPerformance::default()
        }),
        end_to_end_duration_ms: Some(23),
    });

    let extension_request = InputRequest {
        request_id: 8,
        preedit: initial_request.preedit.clone(),
        preceding_text: initial_request.preceding_text.clone(),
        context_available: true,
        config_revision: 0,
        candidate_extension_of: Some(initial_request.request_id),
        candidate_limit: 64,
    };
    let mut all_candidates = first_candidates;
    all_candidates.push(lime_protocol::Candidate {
        display_text: "你号".into(),
        commit_text: "你号".into(),
    });
    service.append_candidate_history(
        initial_request.request_id,
        &extension_request,
        &all_candidates,
    );

    let history = match service.handle(Request::GetInputHistoryPage {
        page: 1,
        page_size: 100,
    }) {
        Response::InputHistoryPage(page) => page.items,
        other => panic!("unexpected history response: {other:?}"),
    };
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].request_id, initial_request.request_id);
    assert_eq!(history[0].rime_candidates.len(), 3);
    assert_eq!(history[0].final_candidates.len(), 3);
    assert_eq!(history[0].diagnostics.len(), 3);
    assert_eq!(history[0].rime_candidates[2].commit_text, "你号");
    assert_eq!(
        history[0]
            .llm_performance
            .as_ref()
            .map(|value| value.total_ms),
        Some(11)
    );
    assert_eq!(history[0].rime_duration_ms, Some(7));
    assert_eq!(history[0].end_to_end_duration_ms, Some(23));
    assert!(history[0].diagnostics[2].llm_candidate.is_none());
    assert!(history[0].diagnostics[2].logprobs.is_empty());
}

#[test]
fn history_is_newest_first_and_page_size_is_bounded_to_one_hundred() {
    let service = CoreService::default();
    for request_id in 1..=105 {
        let response = service.handle(Request::Input(InputRequest {
            request_id,
            preedit: format!("p{request_id}"),
            preceding_text: String::new(),
            context_available: false,
            config_revision: 0,
            candidate_extension_of: None,
            candidate_limit: 0,
        }));
        assert!(matches!(
            response,
            Response::Error {
                code: ErrorCode::RimeInitializationFailed
            }
        ));
    }
    let page = match service.handle(Request::GetInputHistoryPage {
        page: 1,
        page_size: 500,
    }) {
        Response::InputHistoryPage(page) => page,
        other => panic!("unexpected history page response: {other:?}"),
    };
    assert_eq!(page.page, 1);
    assert_eq!(page.page_size, lime_protocol::INPUT_HISTORY_PAGE_SIZE);
    assert_eq!(page.total, 105);
    assert_eq!(page.items.len(), 100);
    assert_eq!(page.items[0].request_id, 105);
    assert!(page
        .items
        .windows(2)
        .all(|rows| rows[0].timestamp_ms > rows[1].timestamp_ms));
    let second = match service.handle(Request::GetInputHistoryPage {
        page: 2,
        page_size: lime_protocol::INPUT_HISTORY_PAGE_SIZE,
    }) {
        Response::InputHistoryPage(page) => page,
        other => panic!("unexpected history page response: {other:?}"),
    };
    assert_eq!(second.items.len(), 5);
    assert_eq!(second.items[0].request_id, 5);
}

#[test]
fn history_revision_wait_wakes_for_new_and_cleared_entries() {
    let service = CoreService::default();
    let waiter = service.clone();
    let thread =
        std::thread::spawn(move || waiter.handle(Request::WaitForInputHistory { revision: 0 }));
    service.handle(Request::Input(InputRequest {
        request_id: 1,
        preedit: "nihao".into(),
        preceding_text: String::new(),
        context_available: false,
        config_revision: 0,
        candidate_extension_of: None,
        candidate_limit: 0,
    }));
    assert_eq!(
        thread.join().expect("history waiter should finish"),
        Response::InputHistoryRevision(1)
    );

    let waiter = service.clone();
    let thread =
        std::thread::spawn(move || waiter.handle(Request::WaitForInputHistory { revision: 1 }));
    assert_eq!(
        service.handle(Request::ClearInputHistory),
        Response::Accepted
    );
    assert_eq!(
        thread.join().expect("history waiter should finish"),
        Response::InputHistoryRevision(2)
    );
}

#[test]
fn model_presets_save_metadata_and_reject_unloadable_activation() {
    let directory = std::env::temp_dir().join(format!(
        "lime-core-model-preset-test-{}-{}",
        std::process::id(),
        now_unix_ms()
    ));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap();
    let model_path = directory.join("demo.gguf");
    fs::write(&model_path, b"GGUF-test-model").unwrap();
    let service = CoreService::new(Some(directory.clone()));

    let saved = match service.handle(Request::SaveModelPreset {
        name: "demo".into(),
        path: model_path.to_string_lossy().into_owned(),
    }) {
        Response::ModelPreset(preset) => preset,
        other => panic!("unexpected save response: {other:?}"),
    };
    assert_eq!(saved.name, "demo");
    assert!(!saved.loaded);
    // Saving the same name updates the preset in place rather than creating duplicates.
    let updated = match service.handle(Request::SaveModelPreset {
        name: "demo".into(),
        path: model_path.to_string_lossy().into_owned(),
    }) {
        Response::ModelPreset(preset) => preset,
        other => panic!("unexpected update response: {other:?}"),
    };
    assert_eq!(updated.name, "demo");
    assert!(matches!(
        service.handle(Request::ListModelPresets),
        Response::ModelPresets(presets) if presets.len() == 1
    ));
    let renamed = match service.handle(Request::RenameModelPreset {
        name: "demo".into(),
        new_name: "renamed".into(),
    }) {
        Response::ModelPreset(preset) => preset,
        other => panic!("unexpected rename response: {other:?}"),
    };
    assert_eq!(renamed.name, "renamed");
    assert_eq!(renamed.path, updated.path);
    assert_eq!(renamed.size_bytes, updated.size_bytes);
    assert_eq!(renamed.sha256, updated.sha256);
    assert_eq!(
        service.handle(Request::RenameModelPreset {
            name: "renamed".into(),
            new_name: "renamed".into(),
        }),
        Response::ModelPreset(renamed.clone())
    );
    assert_eq!(
        service.handle(Request::RenameModelPreset {
            name: "no-such".into(),
            new_name: "missing".into(),
        }),
        Response::Error {
            code: ErrorCode::ModelNotFound
        }
    );
    let duplicate = match service.handle(Request::SaveModelPreset {
        name: "other".into(),
        path: model_path.to_string_lossy().into_owned(),
    }) {
        Response::ModelPreset(preset) => preset,
        other => panic!("unexpected duplicate setup response: {other:?}"),
    };
    assert_eq!(
        service.handle(Request::RenameModelPreset {
            name: "renamed".into(),
            new_name: "other".into(),
        }),
        Response::Error {
            code: ErrorCode::InvalidRequest
        }
    );
    assert_eq!(duplicate.name, "other");
    // A four-byte test file is sufficient to exercise metadata persistence, but it is not a
    // loadable GGUF model.  Activation must therefore fail clearly and leave the current
    // model untouched instead of pretending that validation alone loaded the model.
    assert_eq!(
        service.handle(Request::SelectModelPreset {
            name: "renamed".into(),
        }),
        Response::Error {
            code: ErrorCode::ModelLoadFailed
        }
    );
    match service.handle(Request::GetStatus) {
        Response::Status(status) => {
            assert!(!status.model.loaded);
            assert!(status.model.path.is_none());
        }
        other => panic!("unexpected status response: {other:?}"),
    }
    assert_eq!(
        service.handle(Request::DeleteModelPreset {
            name: "renamed".into()
        }),
        Response::Accepted
    );
    assert_eq!(
        service.handle(Request::DeleteModelPreset {
            name: "other".into()
        }),
        Response::Accepted
    );
    let restarted = CoreService::new(Some(directory.clone()));
    match restarted.handle(Request::ListModelPresets) {
        Response::ModelPresets(presets) => assert!(presets.is_empty()),
        other => panic!("unexpected restarted preset response: {other:?}"),
    }
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn model_preset_state_persists_active_path_and_rejects_unknown_versions() {
    let directory = std::env::temp_dir().join(format!(
        "lime-core-model-state-test-{}-{}",
        std::process::id(),
        now_unix_ms()
    ));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap();
    let model_path = directory.join("demo.gguf");
    let preset = ModelPreset {
        name: "demo".into(),
        path: normalize_path_string(&model_path),
        size_bytes: Some(42),
        sha256: Some("deadbeef".into()),
        loaded: false,
    };

    // A new-format file carries the active model independently of the
    // runtime-only `loaded` bit.
    let new_format = VersionedModelPresets {
        version: MODEL_PRESETS_FILE_VERSION,
        presets: std::slice::from_ref(&preset),
        active_model_path: Some(preset.path.as_str()),
    };
    fs::write(
        directory.join("model-presets.json"),
        serde_json::to_vec(&new_format).unwrap(),
    )
    .unwrap();
    let loaded = load_model_state(&directory).expect("new model state should load");
    assert_eq!(
        loaded.active_model_path.as_deref(),
        Some(preset.path.as_str())
    );
    assert_eq!(loaded.presets, vec![preset.clone()]);

    fs::write(
        directory.join("model-presets.json"),
        serde_json::to_vec(&serde_json::json!({ "version": 99, "presets": [] })).unwrap(),
    )
    .unwrap();
    assert!(load_model_state(&directory).is_none());
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn startup_model_restore_failure_keeps_service_alive_in_rime_only_fallback() {
    let directory = std::env::temp_dir().join(format!(
        "lime-core-model-autoload-failure-test-{}-{}",
        std::process::id(),
        now_unix_ms()
    ));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap();
    let missing_path = directory.join("missing.gguf");
    let missing_path = normalize_path_string(&missing_path);
    fs::write(
        directory.join("model-presets.json"),
        serde_json::to_vec(&VersionedModelPresets {
            version: MODEL_PRESETS_FILE_VERSION,
            presets: &[] as &[ModelPreset],
            active_model_path: Some(missing_path.as_str()),
        })
        .unwrap(),
    )
    .unwrap();

    // No native runtime/model is present in this test environment.  The
    // failed best-effort restore must not panic or leave a phantom model.
    let service = CoreService::new(Some(directory.clone()));
    match service.handle(Request::GetStatus) {
        Response::Status(status) => {
            assert!(!status.model.loaded);
            assert!(status.model.path.is_none());
            assert_ne!(status.state, ServiceState::Ready);
        }
        other => panic!("unexpected status response: {other:?}"),
    }
    match service.handle(Request::ListModelPresets) {
        Response::ModelPresets(presets) => assert!(presets.is_empty()),
        other => panic!("unexpected preset response: {other:?}"),
    }
    // Keep the marker so a temporarily unavailable runtime/model can be
    // retried on the next service start.
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join("model-presets.json")).unwrap()).unwrap();
    assert_eq!(persisted["active_model_path"], missing_path);
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn service_reports_reloading_while_startup_model_is_loading() {
    assert_eq!(service_state(true, false, true), ServiceState::Reloading);
    assert_eq!(service_state(false, false, true), ServiceState::Unavailable);
}

#[test]
fn loaded_model_path_is_persisted_and_restored_when_native_runtime_is_configured() {
    let Some(model_path) = std::env::var_os("LIME_LLAMA_TEST_MODEL").map(PathBuf::from) else {
        return;
    };
    if std::env::var_os("LIME_LLAMA_RUNTIME_DIR").is_none()
        && std::env::var_os("LIME_LLAMA_DLL_PATH").is_none()
    {
        return;
    }
    if !model_path.is_file() {
        return;
    }
    let directory = std::env::temp_dir().join(format!(
        "lime-core-model-autoload-success-test-{}-{}",
        std::process::id(),
        now_unix_ms()
    ));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap();
    let service = CoreService::new(Some(directory.clone()));
    let path = normalize_path_string(&model_path);
    assert_eq!(
        service.handle(Request::LoadModel { path: path.clone() }),
        Response::Accepted
    );
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join("model-presets.json")).unwrap()).unwrap();
    assert_eq!(persisted["active_model_path"], path);

    let restarted = CoreService::new(Some(directory.clone()));
    match restarted.handle(Request::GetStatus) {
        Response::Status(status) => {
            assert!(status.model.loaded);
            assert_eq!(status.model.path.as_deref(), Some(path.as_str()));
        }
        other => panic!("unexpected status response: {other:?}"),
    }
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn schema_override_is_reflected_in_config_and_status() {
    let service = CoreService::new_with_rime_dir_and_schema_override(
        None,
        None,
        Some("double_pinyin_flypy".to_owned()),
    );
    assert_eq!(
        service.config_snapshot().config.rime_schema,
        "double_pinyin_flypy"
    );
    match service.handle(Request::GetStatus) {
        Response::Status(status) => {
            assert_eq!(status.config.config.rime_schema, "double_pinyin_flypy");
            assert_eq!(status.state, ServiceState::Unavailable);
        }
        _ => panic!("unexpected status response"),
    }
}

#[test]
fn learn_request_propagates_native_unavailable_error() {
    let service = CoreService::default();
    assert_eq!(
        service.handle(Request::Learn {
            pinyin: "nihao".into(),
            text: "你好".into(),
        }),
        Response::Error {
            code: ErrorCode::RimeInitializationFailed,
        }
    );
}

#[test]
fn framing_round_trips_requests() {
    let request = Request::GetStatus;
    let mut bytes = Vec::new();
    lime_ipc::write_json(&mut bytes, &request).unwrap();
    let decoded: Request = lime_ipc::read_json(&mut bytes.as_slice()).unwrap();
    assert_eq!(decoded, request);
}

#[test]
fn stale_config_revision_is_cancelled() {
    let service = CoreService::default();
    let config = Config {
        page_size: 10,
        ..Config::default()
    };
    assert!(matches!(
        service.handle(Request::SetConfig(config)),
        Response::Config(_)
    ));
    let response = service.handle(Request::Input(InputRequest {
        request_id: 2,
        preedit: "nihao".into(),
        preceding_text: String::new(),
        context_available: false,
        config_revision: 0,
        candidate_extension_of: None,
        candidate_limit: 0,
    }));
    assert_eq!(
        response,
        Response::Error {
            code: ErrorCode::RequestCancelled
        }
    );
}

#[test]
fn config_persists_versioned_format_and_rejects_unknown_versions() {
    let directory =
        std::env::temp_dir().join(format!("lime-core-config-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&directory);
    let service = CoreService::new(Some(directory.clone()));
    let config = Config {
        rime_schema: "double_pinyin_flypy".into(),
        page_size: 12,
        ..Config::default()
    };
    assert!(matches!(
        service.handle(Request::SetConfig(config)),
        Response::Config(_)
    ));
    let restarted = CoreService::new(Some(directory.clone()));
    assert_eq!(restarted.config_snapshot().config.page_size, 12);
    assert_eq!(
        restarted.config_snapshot().config.rime_schema,
        "double_pinyin_flypy"
    );

    fs::write(
        directory.join("config.json"),
        serde_json::to_vec(&serde_json::json!({
            "version": 99,
            "config": Config::default(),
        }))
        .unwrap(),
    )
    .unwrap();
    let rejected = CoreService::new(Some(directory.clone()));
    assert_eq!(rejected.config_snapshot().config, Config::default());
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn native_input_returns_candidate_remainders() {
    let Ok(rime_dir) = std::env::var("LIME_TEST_RIME_DIR") else {
        return;
    };
    let directory = std::env::temp_dir().join(format!(
        "lime-core-remainder-service-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&directory);
    let service =
        CoreService::new_with_rime_dir(Some(directory.clone()), Some(PathBuf::from(rime_dir)));
    let revision = service.config_snapshot().revision;
    let response = service.handle(Request::Input(InputRequest {
        request_id: 100,
        preedit: "nihao".into(),
        preceding_text: String::new(),
        context_available: false,
        config_revision: revision,
        candidate_extension_of: None,
        candidate_limit: 32,
    }));
    let response = match response {
        Response::Input(response) => response,
        other => panic!("unexpected input response: {other:?}"),
    };
    assert_eq!(
        response.candidate_remainders.len(),
        response.candidates.len()
    );
    let partial_index = response
        .candidates
        .iter()
        .position(|candidate| candidate.commit_text == "你")
        .expect("one-character candidate should be present");
    assert_eq!(
        response.candidate_remainders[partial_index].as_deref(),
        Some("hao")
    );
    let complete_index = response
        .candidates
        .iter()
        .position(|candidate| candidate.commit_text == "你好")
        .expect("complete candidate should be present");
    assert_eq!(
        response.candidate_remainders[complete_index].as_deref(),
        Some("")
    );

    let extension_response = service.handle(Request::Input(InputRequest {
        request_id: 101,
        preedit: "nihao".into(),
        preceding_text: String::new(),
        context_available: false,
        config_revision: revision,
        candidate_extension_of: Some(100),
        candidate_limit: 64,
    }));
    let extension_response = match extension_response {
        Response::Input(response) => response,
        other => panic!("unexpected candidate extension response: {other:?}"),
    };
    assert!(extension_response.llm_performance.is_none());
    let history = match service.handle(Request::GetInputHistoryPage {
        page: 1,
        page_size: 100,
    }) {
        Response::InputHistoryPage(page) => page,
        other => panic!("unexpected history response: {other:?}"),
    };
    assert_eq!(history.total, 1);
    assert_eq!(history.items[0].request_id, 100);
    assert_eq!(
        history.items[0].rime_candidates.len(),
        extension_response.candidates.len()
    );
    assert_eq!(
        history.items[0].final_candidates.len(),
        extension_response.candidates.len()
    );
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn native_input_candidate_limit_bounds_response_and_extension_prefix() {
    let Ok(rime_dir) = std::env::var("LIME_TEST_RIME_DIR") else {
        return;
    };
    let directory = std::env::temp_dir().join(format!(
        "lime-core-candidate-limit-service-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&directory);
    let service =
        CoreService::new_with_rime_dir(Some(directory.clone()), Some(PathBuf::from(rime_dir)));
    let revision = service.config_snapshot().revision;
    let first = match service.handle(Request::Input(InputRequest {
        request_id: 200,
        preedit: "nihao".into(),
        preceding_text: String::new(),
        context_available: false,
        config_revision: revision,
        candidate_extension_of: None,
        candidate_limit: 9,
    })) {
        Response::Input(response) => response,
        other => panic!("unexpected input response: {other:?}"),
    };
    assert!(first.candidates.len() <= 9);
    assert_eq!(first.candidates.len(), first.candidate_remainders.len());

    let extension = match service.handle(Request::Input(InputRequest {
        request_id: 201,
        preedit: "nihao".into(),
        preceding_text: String::new(),
        context_available: false,
        config_revision: revision,
        candidate_extension_of: Some(200),
        candidate_limit: 36,
    })) {
        Response::Input(response) => response,
        other => panic!("unexpected candidate extension response: {other:?}"),
    };
    assert!(extension.candidates.len() <= 36);
    assert_eq!(
        extension.candidates.len(),
        extension.candidate_remainders.len()
    );
    assert!(extension.candidates.len() >= first.candidates.len());
    assert_eq!(
        extension.candidates[..first.candidates.len()],
        first.candidates[..]
    );
    assert_eq!(
        extension.candidate_remainders[..first.candidate_remainders.len()],
        first.candidate_remainders[..]
    );
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn native_schema_config_change_reloads_the_librime_session_when_requested() {
    let Ok(rime_dir) = std::env::var("LIME_TEST_RIME_DIR") else {
        return;
    };
    if std::env::var_os("LIME_TEST_RIME_SERVICE_SCHEMA").is_none() {
        return;
    }
    let directory = std::env::temp_dir().join(format!(
        "lime-core-schema-service-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&directory);
    let service =
        CoreService::new_with_rime_dir(Some(directory.clone()), Some(PathBuf::from(rime_dir)));
    let mut config = service.config_snapshot().config;
    config.rime_schema = "double_pinyin_flypy".into();
    assert!(matches!(
        service.handle(Request::SetConfig(config)),
        Response::Config(_)
    ));
    let revision = service.config_snapshot().revision;
    let response = service.handle(Request::Input(InputRequest {
        request_id: 99,
        preedit: "nh".into(),
        preceding_text: String::new(),
        context_available: false,
        config_revision: revision,
        candidate_extension_of: None,
        candidate_limit: 0,
    }));
    match response {
        Response::Input(value) => assert!(!value.candidates.is_empty()),
        other => panic!("unexpected schema-switch response: {other:?}"),
    }
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn native_learn_request_updates_rime_user_dictionary_when_requested() {
    if std::env::var_os("LIME_TEST_RIME_LEARN_REQUEST").is_none() {
        return;
    }
    let Ok(rime_dir) = std::env::var("LIME_TEST_RIME_DIR") else {
        return;
    };
    let directory = std::env::temp_dir().join(format!(
        "lime-core-learn-request-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&directory);
    let service =
        CoreService::new_with_rime_dir(Some(directory.clone()), Some(PathBuf::from(rime_dir)));
    assert_eq!(service.handle(Request::ClearDictionary), Response::Accepted);
    assert_eq!(
        service.handle(Request::Learn {
            pinyin: "nihao".into(),
            text: "拟好".into(),
        }),
        Response::Accepted
    );
    match service.handle(Request::ExportDictionary) {
        Response::Dictionary(entries) => assert!(entries.iter().any(|entry| {
            entry.pinyin == "nihao" && entry.text == "拟好" && entry.weight >= 1
        })),
        other => panic!("unexpected dictionary response: {other:?}"),
    }
    assert_eq!(service.handle(Request::ClearDictionary), Response::Accepted);
    let _ = fs::remove_dir_all(directory);
}
