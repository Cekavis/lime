use crate::{
    config::ConfigStore,
    engine::{CandidateBatch, CandidateEngine, RimeEngine},
    logging::PrivacyLogger,
    ranking::{
        try_rerank_selected_candidates_with_preedit_and_limit, GenerationTracker, LlamaRuntime,
    },
};
mod history;
mod persistence;
use lime_protocol::{
    CandidateDiagnostic, ConfigSnapshot, DictionaryPage, ErrorCode, InputHistoryEntry,
    InputHistoryPage, InputRequest, InputResponse, LlmPerformance, ModelInfo, ModelMemoryInfo,
    ModelPreset, Request, Response, ServiceState, ServiceStatus,
    DICTIONARY_PAGE_SIZE,
};
use llama_cpp_v3::BackendPreference;
pub(crate) use persistence::{load_config, load_model_state};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn backend_preference_for(value: &str) -> BackendPreference {
    match value {
        "cpu" => BackendPreference::Cpu,
        // Config validation only accepts `cuda` or `cpu`; treating any
        // unexpected persisted value as CUDA preserves the safe default while
        // still allowing the runtime's documented CPU fallback.
        _ => BackendPreference::Cuda,
    }
}

#[derive(Clone)]
pub struct CoreService {
    config: Arc<Mutex<ConfigStore>>,
    engine: Arc<Mutex<RimeEngine>>,
    model: Arc<Mutex<Option<LlamaRuntime>>>,
    model_load: Arc<Mutex<()>>,
    generation: Arc<GenerationTracker>,
    data_dir: Option<PathBuf>,
    logger: Arc<PrivacyLogger>,
    history: Arc<Mutex<Vec<InputHistoryEntry>>>,
    history_clock: Arc<AtomicU64>,
    history_revision: Arc<(Mutex<u64>, Condvar)>,
    model_presets: Arc<Mutex<BTreeMap<String, ModelPreset>>>,
    /// Path of the last model that loaded successfully.  This is separate from
    /// `ModelPreset::loaded`, which is a runtime-only status bit exposed to clients.
    active_model_path: Arc<Mutex<Option<String>>>,
    model_loading: Arc<std::sync::atomic::AtomicBool>,
}

struct InputHistoryRecord<'a> {
    request: &'a InputRequest,
    rime_candidates: Vec<lime_protocol::Candidate>,
    final_candidates: Vec<lime_protocol::Candidate>,
    diagnostics: Vec<CandidateDiagnostic>,
    service_state: ServiceState,
    model_name: Option<String>,
    rime_duration_ms: Option<u64>,
    llm_performance: Option<LlmPerformance>,
    end_to_end_duration_ms: Option<u64>,
}

impl Default for CoreService {
    fn default() -> Self {
        Self::new(None)
    }
}

impl CoreService {
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        Self::new_with_rime_dir(data_dir, None)
    }

    pub fn new_with_rime_dir(data_dir: Option<PathBuf>, rime_dir: Option<PathBuf>) -> Self {
        let schema_override = std::env::var("LIME_RIME_SCHEMA")
            .ok()
            .filter(|value| !value.trim().is_empty());
        Self::new_with_rime_dir_and_schema_override(data_dir, rime_dir, schema_override)
    }

    fn new_with_rime_dir_and_schema_override(
        data_dir: Option<PathBuf>,
        rime_dir: Option<PathBuf>,
        schema_override: Option<String>,
    ) -> Self {
        if let Some(dir) = &data_dir {
            let _ = fs::create_dir_all(dir);
        }
        let logger = Arc::new(PrivacyLogger::new(data_dir.clone(), false));
        let stored_config = data_dir
            .as_deref()
            .and_then(load_config)
            .and_then(|value| ConfigStore::from_config(value).ok())
            .unwrap_or_default();
        // The environment override is an effective startup setting. Reflect it in the
        // revisioned config snapshot as well as in the native session, otherwise GetStatus/GetConfig
        // report a schema different from the one actually used by librime. Invalid overrides are
        // ignored and leave the persisted, validated configuration active.
        let config = if let Some(schema) = schema_override {
            let mut effective = stored_config.snapshot().config;
            effective.rime_schema = schema;
            match ConfigStore::from_config(effective) {
                Ok(config) => config,
                Err(_) => {
                    logger.event(
                        "config_validation_failed",
                        Some(ErrorCode::ConfigValidationFailed),
                    );
                    stored_config
                }
            }
        } else {
            stored_config
        };
        let configured_schema = config.snapshot().config.rime_schema.clone();
        let engine = match rime_dir {
            Some(path) => {
                let result = if let Some(data_dir) = &data_dir {
                    RimeEngine::with_resource_dir_and_user_dir_and_schema(
                        &path,
                        data_dir.join("rime-user"),
                        &configured_schema,
                    )
                } else {
                    RimeEngine::with_resource_dir_and_schema(&path, &configured_schema)
                };
                match result {
                    Ok(engine) => engine,
                    Err(error) => {
                        logger.event("rime_initialization_failed", Some(error.code));
                        eprintln!(
                            "librime initialization failed for {}: {}",
                            path.display(),
                            error
                        );
                        RimeEngine::new()
                    }
                }
            }
            None => {
                logger.event(
                    "rime_initialization_failed",
                    Some(ErrorCode::RimeInitializationFailed),
                );
                RimeEngine::new()
            }
        };
        let loaded_model_presets = data_dir
            .as_deref()
            .and_then(load_model_state)
            .unwrap_or_default();
        let active_model_path = loaded_model_presets.active_model_path.clone();
        let model_presets = loaded_model_presets
            .presets
            .into_iter()
            .map(|mut preset| {
                // `loaded` is a runtime status bit and must never survive a service restart.
                preset.loaded = false;
                (preset.name.clone(), preset)
            })
            .collect::<BTreeMap<_, _>>();
        let service = Self {
            config: Arc::new(Mutex::new(config)),
            engine: Arc::new(Mutex::new(engine)),
            model: Arc::new(Mutex::new(None)),
            model_load: Arc::new(Mutex::new(())),
            generation: Arc::new(GenerationTracker::default()),
            data_dir: data_dir.clone(),
            logger,
            history: Arc::new(Mutex::new(Vec::new())),
            history_clock: Arc::new(AtomicU64::new(now_unix_ms())),
            history_revision: Arc::new((Mutex::new(0), Condvar::new())),
            model_presets: Arc::new(Mutex::new(model_presets)),
            active_model_path: Arc::new(Mutex::new(active_model_path.clone())),
            model_loading: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        // Restoring the last model is best-effort.  A missing model, unavailable
        // native runtime, or an incompatible GGUF must leave the service alive in
        // Rime-only mode rather than aborting construction.
        if let Some(path) = active_model_path {
            let loading = Arc::clone(&service.model_loading);
            loading.store(true, Ordering::Release);
            let restore_service = service.clone();
            std::thread::spawn(move || {
                restore_service.restore_model_at_startup(Path::new(&path));
                loading.store(false, Ordering::Release);
            });
        }
        service
    }

    pub fn config_snapshot(&self) -> ConfigSnapshot {
        self.config
            .lock()
            .expect("config mutex poisoned")
            .snapshot()
    }

    pub fn handle(&self, request: Request) -> Response {
        match request {
            Request::Handshake(handshake) => {
                if handshake.protocol_version == lime_protocol::PROTOCOL_VERSION {
                    Response::Handshake(lime_protocol::HandshakeResponse::accepted())
                } else {
                    Response::Handshake(lime_protocol::HandshakeResponse::rejected(
                        ErrorCode::ProtocolVersionMismatch,
                    ))
                }
            }
            Request::Input(input) => self
                .input(input)
                .map(Response::Input)
                .unwrap_or_else(|code| Response::Error { code }),
            Request::GetConfig => Response::Config(self.config_snapshot()),
            Request::SetConfig(config) => {
                let result = {
                    let mut store = self.config.lock().expect("config mutex poisoned");
                    let before = store.snapshot();
                    match store.replace(config) {
                        Ok(snapshot) => {
                            let schema_changed =
                                before.config.rime_schema != snapshot.config.rime_schema;
                            let schema_result = if schema_changed {
                                let mut engine = self.engine.lock().expect("engine mutex poisoned");
                                if engine.is_available() {
                                    engine
                                        .select_schema(&snapshot.config.rime_schema)
                                        .map_err(|error| error.code)
                                } else {
                                    // Defer applying the schema until the native runtime is
                                    // available; configuration remains the source of truth.
                                    Ok(())
                                }
                            } else {
                                Ok(())
                            };
                            if let Err(code) = schema_result {
                                store.restore(before);
                                Err(code)
                            } else if self.persist_config(&snapshot.config).is_ok() {
                                Ok(snapshot)
                            } else {
                                if schema_changed {
                                    let mut engine =
                                        self.engine.lock().expect("engine mutex poisoned");
                                    if engine.is_available() {
                                        let _ = engine.select_schema(&before.config.rime_schema);
                                    }
                                }
                                store.restore(before);
                                Err(ErrorCode::Internal)
                            }
                        }
                        Err(_) => Err(ErrorCode::ConfigValidationFailed),
                    }
                };
                match result {
                    Ok(snapshot) => Response::Config(snapshot),
                    Err(ErrorCode::ConfigValidationFailed) => {
                        self.logger.event(
                            "config_validation_failed",
                            Some(ErrorCode::ConfigValidationFailed),
                        );
                        Response::Error {
                            code: ErrorCode::ConfigValidationFailed,
                        }
                    }
                    Err(ErrorCode::RimeInitializationFailed) => {
                        self.logger.event(
                            "rime_schema_change_failed",
                            Some(ErrorCode::RimeInitializationFailed),
                        );
                        Response::Error {
                            code: ErrorCode::RimeInitializationFailed,
                        }
                    }
                    Err(code) => {
                        self.logger.event("config_persist_failed", Some(code));
                        Response::Error { code }
                    }
                }
            }
            Request::GetStatus => Response::Status(self.status()),
            Request::LoadModel { path } => self.load_model(Path::new(&path)),
            Request::UnloadModel => self.unload_model(),
            Request::ListModelPresets => Response::ModelPresets(self.model_presets()),
            Request::SaveModelPreset { name, path } => {
                self.save_model_preset(&name, Path::new(&path))
            }
            Request::RenameModelPreset { name, new_name } => {
                self.rename_model_preset(&name, &new_name)
            }
            Request::DeleteModelPreset { name } => self.delete_model_preset(&name),
            Request::SelectModelPreset { name } => self.select_model_preset(&name),
            Request::Learn { pinyin, text } => self.learn(&pinyin, &text),
            Request::ExportDictionary => match self
                .engine
                .lock()
                .expect("engine mutex poisoned")
                .export_dictionary()
            {
                Ok(entries) => Response::Dictionary(entries),
                Err(error) => Response::Error { code: error.code },
            },
            Request::GetDictionaryPage { page, page_size } => {
                match self.dictionary_page(page, page_size) {
                    Ok(page) => Response::DictionaryPage(page),
                    Err(error) => Response::Error { code: error.code },
                }
            }
            Request::ImportDictionary { entries } => match self
                .engine
                .lock()
                .expect("engine mutex poisoned")
                .import_dictionary(&entries)
            {
                Ok(()) => Response::Accepted,
                Err(error) => Response::Error { code: error.code },
            },
            Request::ClearDictionary => match self
                .engine
                .lock()
                .expect("engine mutex poisoned")
                .clear_dictionary()
            {
                Ok(()) => Response::Accepted,
                Err(error) => Response::Error { code: error.code },
            },
            Request::GetInputHistoryPage { page, page_size } => {
                Response::InputHistoryPage(self.history_page(page, page_size))
            }
            Request::WaitForInputHistory { revision } => {
                Response::InputHistoryRevision(self.wait_for_history_revision(revision))
            }
            Request::ClearInputHistory => {
                self.history.lock().expect("history mutex poisoned").clear();
                self.bump_history_revision();
                Response::Accepted
            }
        }
    }

    fn input(&self, request: InputRequest) -> Result<InputResponse, ErrorCode> {
        let input_started = Instant::now();
        let snapshot = self.config_snapshot();
        if request.config_revision != snapshot.revision {
            if request.candidate_extension_of.is_none() {
                self.record_input_history(InputHistoryRecord {
                    request: &request,
                    rime_candidates: Vec::new(),
                    final_candidates: Vec::new(),
                    diagnostics: Vec::new(),
                    service_state: self.current_service_state(),
                    model_name: None,
                    rime_duration_ms: None,
                    llm_performance: None,
                    end_to_end_duration_ms: Some(elapsed_ms(input_started.elapsed())),
                });
            }
            return Err(ErrorCode::RequestCancelled);
        }
        let generation = self.generation.next();
        let config = snapshot.config;
        let model = self.model.lock().map_err(|_| ErrorCode::Internal)?;
        // A loaded model is active; model absence selects the Rime-only path.
        let runtime = model.as_ref();
        let service_state = if model.is_some() {
            ServiceState::Ready
        } else {
            ServiceState::RimeOnly
        };
        let model_name = model_display_name(runtime);
        let rerank_count = runtime.map_or(0, |_| config.llm_rerank_count as usize);
        let candidate_limit = if request.candidate_limit == 0 {
            0
        } else {
            (request.candidate_limit as usize)
                .max(config.page_size as usize)
                .max(rerank_count)
        };
        let (rime_result, rime_duration_ms) = match self.engine.lock() {
            Err(_) => (Err(ErrorCode::Internal), None),
            Ok(mut engine) => {
                let rime_started = Instant::now();
                let result = engine
                    .candidates_for_rerank(&request.preedit, rerank_count, candidate_limit)
                    .map_err(|error| error.code);
                (result, Some(elapsed_ms(rime_started.elapsed())))
            }
        };
        let rime_batch = match rime_result {
            Ok(batch) => batch,
            Err(code) => {
                if request.candidate_extension_of.is_none() {
                    self.record_input_history(InputHistoryRecord {
                        request: &request,
                        rime_candidates: Vec::new(),
                        final_candidates: Vec::new(),
                        diagnostics: Vec::new(),
                        service_state,
                        model_name: model_name.clone(),
                        rime_duration_ms,
                        llm_performance: None,
                        end_to_end_duration_ms: Some(elapsed_ms(input_started.elapsed())),
                    });
                }
                return Err(code);
            }
        };
        let CandidateBatch {
            candidates: rime_candidates,
            complete_candidate_indices,
            candidate_remainders: rime_candidate_remainders,
        } = rime_batch;
        if let Some(original_request_id) = request.candidate_extension_of {
            let candidates = rime_candidates;
            let candidate_remainders = rime_candidate_remainders;
            self.append_candidate_history(original_request_id, &request, &candidates);
            if !self.generation.is_current(generation) {
                return Err(ErrorCode::RequestCancelled);
            }
            return Ok(InputResponse {
                request_id: request.request_id,
                candidates,
                candidate_remainders,
                context_used: request.context_available && !request.preceding_text.is_empty(),
                service_state,
                diagnostics: Vec::new(),
                end_to_end_duration_ms: None,
                rime_duration_ms: None,
                llm_performance: None,
            });
        }
        let preceding_text = truncate_chars(
            &request.preceding_text,
            config.preceding_text_char_limit as usize,
        );
        let ranking = match try_rerank_selected_candidates_with_preedit_and_limit(
            &rime_candidates,
            &complete_candidate_indices,
            &request.preedit,
            &preceding_text,
            runtime,
            config.llm_rerank_count as usize,
            config.llm_effective_count as usize,
            config.llm_inference_count_limit as usize,
        ) {
            Ok(ranking) => ranking,
            Err(error) => {
                self.logger
                    .event("llama_rerank_failed", Some(ErrorCode::ModelLoadFailed));
                eprintln!("llama.cpp rerank failed: {error}");
                return Err(ErrorCode::ModelLoadFailed);
            }
        };
        let llm_performance = ranking.llm_performance.clone();
        let candidates = ranking.result.candidates;
        let candidate_remainders = ranking
            .candidate_indices
            .iter()
            .map(|index| {
                rime_candidate_remainders
                    .get(*index)
                    .cloned()
                    .unwrap_or(None)
            })
            .collect::<Vec<_>>();
        let diagnostics = ranking.result.diagnostics;
        let end_to_end_duration_ms = Some(elapsed_ms(input_started.elapsed()));
        self.record_input_history(InputHistoryRecord {
            request: &request,
            rime_candidates,
            final_candidates: candidates.clone(),
            diagnostics: diagnostics.clone(),
            service_state,
            model_name,
            rime_duration_ms,
            llm_performance: llm_performance.clone(),
            end_to_end_duration_ms,
        });
        if !self.generation.is_current(generation) {
            return Err(ErrorCode::RequestCancelled);
        }
        Ok(InputResponse {
            request_id: request.request_id,
            candidates,
            candidate_remainders,
            context_used: request.context_available && !request.preceding_text.is_empty(),
            service_state,
            diagnostics,
            end_to_end_duration_ms,
            rime_duration_ms,
            llm_performance,
        })
    }

    fn load_model(&self, path: &Path) -> Response {
        // Loading a native runtime mutates process-global llama.cpp state and
        // the Windows DLL search path. Serialize the whole load/replace
        // operation while leaving inference protected by the model mutex.
        let _load_guard = self.model_load.lock().expect("model load mutex poisoned");
        match self.load_runtime(path) {
            Ok(model) => {
                let loaded_path = model.path.clone();
                *self.model.lock().expect("model mutex poisoned") = Some(model);
                self.mark_loaded_preset(Some(&loaded_path));
                self.set_active_model_path(Some(&loaded_path));
                self.persist_model_state_best_effort("model_state_persist_failed");
                Response::Accepted
            }
            Err(code) => {
                self.logger.event("model_load_failed", Some(code));
                Response::Error { code }
            }
        }
    }

    fn unload_model(&self) -> Response {
        let _load_guard = self.model_load.lock().expect("model load mutex poisoned");
        *self.model.lock().expect("model mutex poisoned") = None;
        self.mark_loaded_preset(None);
        self.set_active_model_path(None);
        self.persist_model_state_best_effort("model_state_persist_failed");
        Response::Accepted
    }

    fn load_runtime(&self, path: &Path) -> Result<LlamaRuntime, ErrorCode> {
        let (context_tokens, sequence_count, backend_preference) = self
            .config
            .lock()
            .map(|config| {
                let snapshot = config.snapshot();
                (
                    snapshot.config.llm_context_token_limit as usize,
                    snapshot.config.llm_rerank_count as usize,
                    backend_preference_for(&snapshot.config.llm_backend),
                )
            })
            .unwrap_or((
                crate::llama::DEFAULT_CONTEXT_TOKENS,
                crate::llama::DEFAULT_SEQUENCE_COUNT,
                BackendPreference::Cuda,
            ));
        LlamaRuntime::load_with_backend_preference_and_sequence_count(
            path.to_path_buf(),
            context_tokens,
            backend_preference,
            sequence_count,
        )
        .map_err(|error| {
            if error.contains("unsupported model for Lime ranking:") {
                return ErrorCode::ModelUnsupported;
            }
            if path.exists() {
                ErrorCode::ModelLoadFailed
            } else {
                ErrorCode::ModelNotFound
            }
        })
    }

    fn restore_model_at_startup(&self, path: &Path) {
        // Startup restoration is deliberately best-effort.  Keep this operation
        // serialized with explicit model requests, but never let a failed load
        // prevent the service (and its Rime path) from coming up.
        let _load_guard = self.model_load.lock().expect("model load mutex poisoned");
        if !path.is_file() {
            self.logger
                .event("model_autoload_failed", Some(ErrorCode::ModelNotFound));
            return;
        }
        match self.load_runtime(path) {
            Ok(model) => {
                let loaded_path = model.path.clone();
                *self.model.lock().expect("model mutex poisoned") = Some(model);
                self.mark_loaded_preset(Some(&loaded_path));
                self.set_active_model_path(Some(&loaded_path));
            }
            Err(code) => {
                self.logger.event("model_autoload_failed", Some(code));
                eprintln!(
                    "Lime model auto-load failed for {}: {}",
                    path.display(),
                    code
                );
            }
        }
    }

    fn set_active_model_path(&self, path: Option<&Path>) {
        let value = path.map(normalize_path_string);
        *self
            .active_model_path
            .lock()
            .expect("active model mutex poisoned") = value;
    }

    fn persist_model_state_best_effort(&self, event: &'static str) {
        if let Err(error) = self.persist_model_state() {
            self.logger.event(event, Some(ErrorCode::Internal));
            eprintln!("Lime model state persistence failed: {error}");
        }
    }

    fn persist_model_state(&self) -> Result<(), std::io::Error> {
        let presets = self
            .model_presets
            .lock()
            .map_err(|_| std::io::Error::other("model presets mutex poisoned"))?;
        self.persist_model_presets_locked(&presets)
    }

    fn model_presets(&self) -> Vec<ModelPreset> {
        let loaded_path = self
            .model
            .lock()
            .expect("model mutex poisoned")
            .as_ref()
            .map(|model| normalize_path_string(&model.path));
        let mut presets = self
            .model_presets
            .lock()
            .expect("model presets mutex poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for preset in &mut presets {
            preset.loaded = loaded_path
                .as_deref()
                .is_some_and(|path| path == preset.path);
        }
        presets
    }

    fn mark_loaded_preset(&self, path: Option<&Path>) {
        let normalized = path.map(normalize_path_string);
        let mut presets = self
            .model_presets
            .lock()
            .expect("model presets mutex poisoned");
        for preset in presets.values_mut() {
            preset.loaded = normalized
                .as_deref()
                .is_some_and(|value| value == preset.path);
        }
    }

    fn save_model_preset(&self, name: &str, path: &Path) -> Response {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 128 || name.contains('\0') {
            return Response::Error {
                code: ErrorCode::InvalidRequest,
            };
        }
        let metadata = match LlamaRuntime::inspect_gguf(path.to_path_buf()) {
            Ok(metadata) => metadata,
            Err(_) => {
                let code = if path.exists() {
                    ErrorCode::ModelLoadFailed
                } else {
                    ErrorCode::ModelNotFound
                };
                self.logger.event("model_preset_save_failed", Some(code));
                return Response::Error { code };
            }
        };
        let normalized_path = normalize_path_string(path);
        let loaded = self
            .model
            .lock()
            .expect("model mutex poisoned")
            .as_ref()
            .map(|current| normalize_path_string(&current.path))
            .is_some_and(|current| current == normalized_path);
        let preset = ModelPreset {
            name: name.to_owned(),
            path: normalized_path,
            size_bytes: Some(metadata.size_bytes),
            sha256: Some(metadata.sha256),
            loaded,
        };
        let mut presets = self
            .model_presets
            .lock()
            .expect("model presets mutex poisoned");
        let previous = presets.insert(name.to_owned(), preset.clone());
        if self.persist_model_presets_locked(&presets).is_err() {
            if let Some(previous) = previous {
                presets.insert(name.to_owned(), previous);
            } else {
                presets.remove(name);
            }
            return Response::Error {
                code: ErrorCode::Internal,
            };
        }
        Response::ModelPreset(preset)
    }

    fn delete_model_preset(&self, name: &str) -> Response {
        let name = name.trim();
        if name.is_empty() {
            return Response::Error {
                code: ErrorCode::InvalidRequest,
            };
        }
        let mut presets = self
            .model_presets
            .lock()
            .expect("model presets mutex poisoned");
        let Some(removed) = presets.remove(name) else {
            return Response::Error {
                code: ErrorCode::ModelNotFound,
            };
        };
        if self.persist_model_presets_locked(&presets).is_err() {
            presets.insert(name.to_owned(), removed);
            return Response::Error {
                code: ErrorCode::Internal,
            };
        }
        Response::Accepted
    }

    fn rename_model_preset(&self, name: &str, new_name: &str) -> Response {
        let name = name.trim();
        let new_name = new_name.trim();
        if name.is_empty()
            || new_name.is_empty()
            || name.chars().count() > 128
            || new_name.chars().count() > 128
            || name.contains('\0')
            || new_name.contains('\0')
        {
            return Response::Error {
                code: ErrorCode::InvalidRequest,
            };
        }
        let mut presets = self
            .model_presets
            .lock()
            .expect("model presets mutex poisoned");
        let Some(preset) = presets.get(name).cloned() else {
            return Response::Error {
                code: ErrorCode::ModelNotFound,
            };
        };
        if name == new_name {
            return Response::ModelPreset(preset);
        }
        if presets.contains_key(new_name) {
            return Response::Error {
                code: ErrorCode::InvalidRequest,
            };
        }

        let mut renamed = preset.clone();
        renamed.name = new_name.to_owned();
        presets.remove(name);
        presets.insert(new_name.to_owned(), renamed.clone());
        if self.persist_model_presets_locked(&presets).is_err() {
            presets.remove(new_name);
            presets.insert(name.to_owned(), preset);
            return Response::Error {
                code: ErrorCode::Internal,
            };
        }
        Response::ModelPreset(renamed)
    }

    fn select_model_preset(&self, name: &str) -> Response {
        let _load_guard = self.model_load.lock().expect("model load mutex poisoned");
        let name = name.trim();
        if name.is_empty() {
            return Response::Error {
                code: ErrorCode::InvalidRequest,
            };
        }
        let preset = {
            let presets = self
                .model_presets
                .lock()
                .expect("model presets mutex poisoned");
            presets.get(name).cloned()
        };
        let Some(preset) = preset else {
            return Response::Error {
                code: ErrorCode::ModelNotFound,
            };
        };
        match self.load_runtime(Path::new(&preset.path)) {
            Ok(model) => {
                let loaded_path = model.path.clone();
                *self.model.lock().expect("model mutex poisoned") = Some(model);
                self.mark_loaded_preset(Some(&loaded_path));
                self.set_active_model_path(Some(&loaded_path));
                self.persist_model_state_best_effort("model_state_persist_failed");
                Response::ModelPreset(
                    self.model_presets()
                        .into_iter()
                        .find(|item| item.name == name)
                        .unwrap_or(preset),
                )
            }
            Err(code) => {
                self.logger.event("model_preset_select_failed", Some(code));
                Response::Error { code }
            }
        }
    }

    fn learn(&self, pinyin: &str, text: &str) -> Response {
        match self
            .engine
            .lock()
            .expect("engine mutex poisoned")
            .learn(pinyin, text)
        {
            Ok(()) => Response::Accepted,
            Err(error) => Response::Error { code: error.code },
        }
    }
    fn status(&self) -> ServiceStatus {
        let (rime_available, active_schema) = {
            let engine = self.engine.lock().expect("engine mutex poisoned");
            (
                engine.is_available(),
                engine.active_schema().map(str::to_owned),
            )
        };
        let model = self.model.lock().expect("model mutex poisoned");
        let mut config = self.config_snapshot();
        // Keep status truthful even if a schema was selected through a native session
        // operation rather than through the persisted config path.
        if let Some(schema) = active_schema {
            config.config.rime_schema = schema;
        }
        ServiceStatus {
            state: service_state(
                rime_available,
                model.is_some(),
                self.model_loading.load(Ordering::Acquire),
            ),
            config,
            model: model
                .as_ref()
                .map(|item| ModelInfo {
                    path: Some(item.path.display().to_string()),
                    size_bytes: Some(item.size_bytes),
                    sha256: Some(item.sha256.clone()),
                    loaded: true,
                    scoring_path: Some(item.scoring_path()),
                    initialization_memory: item.initialization_memory.as_ref().map(|memory| {
                        let total_bytes = memory_breakdown_total(&memory.breakdown);
                        let mut breakdown = memory.breakdown.clone();
                        if let Some(total) = total_bytes {
                            breakdown.insert("total".to_owned(), total);
                        }
                        ModelMemoryInfo {
                            model_bytes: memory_breakdown_sum(&memory.breakdown, "model"),
                            context_bytes: memory_breakdown_sum_for_kinds(
                                &memory.breakdown,
                                &["kv", "output", "rs", "lora", "state"],
                            ),
                            compute_bytes: memory_breakdown_sum(&memory.breakdown, "compute"),
                            total_bytes,
                            backend: Some(item.backend_name.to_owned()),
                            breakdown,
                        }
                    }),
                })
                .unwrap_or(ModelInfo {
                    path: None,
                    size_bytes: None,
                    sha256: None,
                    loaded: false,
                    scoring_path: None,
                    initialization_memory: None,
                }),
        }
    }

    fn current_service_state(&self) -> ServiceState {
        let rime_available = self
            .engine
            .lock()
            .expect("engine mutex poisoned")
            .is_available();
        let model_loaded = self.model.lock().expect("model mutex poisoned").is_some();
        service_state(
            rime_available,
            model_loaded,
            self.model_loading.load(Ordering::Acquire),
        )
    }

    fn history_page(&self, page: u32, page_size: u32) -> InputHistoryPage {
        history::page(
            &self.history.lock().expect("history mutex poisoned"),
            page,
            page_size,
        )
    }

    fn dictionary_page(
        &self,
        page: u32,
        page_size: u32,
    ) -> Result<DictionaryPage, crate::error::CoreError> {
        let entries = self
            .engine
            .lock()
            .expect("engine mutex poisoned")
            .export_dictionary()?;
        let total = entries.len() as u64;
        let page = page.max(1);
        let page_size = if page_size == 0 {
            DICTIONARY_PAGE_SIZE
        } else {
            page_size.clamp(1, DICTIONARY_PAGE_SIZE)
        };
        let start = (u64::from(page - 1) * u64::from(page_size)) as usize;
        let items = entries
            .into_iter()
            .skip(start)
            .take(page_size as usize)
            .collect();
        Ok(DictionaryPage {
            items,
            total,
            page,
            page_size,
        })
    }

    /// Return the current history revision, waiting until it changes from the
    /// caller's last observed value.  The revision is separate from the
    /// entries so a client can wait without transferring input content.
    fn wait_for_history_revision(&self, revision: u64) -> u64 {
        let (lock, changed) = &*self.history_revision;
        let current = lock.lock().expect("history revision mutex poisoned");
        let (current, _) = changed
            .wait_timeout_while(current, Duration::from_secs(30), |value| *value == revision)
            .expect("history revision mutex poisoned");
        *current
    }

    fn bump_history_revision(&self) {
        let (lock, changed) = &*self.history_revision;
        let mut revision = lock.lock().expect("history revision mutex poisoned");
        *revision = revision.saturating_add(1);
        changed.notify_all();
    }

    fn record_input_history(&self, record: InputHistoryRecord<'_>) {
        let InputHistoryRecord {
            request,
            rime_candidates,
            final_candidates,
            diagnostics,
            service_state,
            model_name,
            rime_duration_ms,
            llm_performance,
            end_to_end_duration_ms,
        } = record;
        let timestamp_ms = self.next_timestamp_ms();
        self.history
            .lock()
            .expect("history mutex poisoned")
            .push(InputHistoryEntry {
                request_id: request.request_id,
                timestamp_ms,
                end_to_end_duration_ms,
                preceding_text: request.preceding_text.clone(),
                preedit: request.preedit.clone(),
                rime_candidates,
                final_candidates,
                service_state,
                model_name,
                rime_duration_ms,
                diagnostics,
                llm_performance,
            });
        self.bump_history_revision();
    }

    fn append_candidate_history(
        &self,
        original_request_id: u64,
        request: &InputRequest,
        candidates: &[lime_protocol::Candidate],
    ) {
        let changed = {
            let mut history = self.history.lock().expect("history mutex poisoned");
            let Some(entry) = history.iter_mut().rev().find(|entry| {
                entry.request_id == original_request_id
                    && entry.preedit == request.preedit
                    && entry.preceding_text == request.preceding_text
            }) else {
                return;
            };
            let rime_start = entry.rime_candidates.len();
            if candidates.len() <= rime_start {
                false
            } else {
                entry
                    .rime_candidates
                    .extend(candidates[rime_start..].iter().cloned());

                let final_start = entry.final_candidates.len().min(candidates.len());
                entry
                    .final_candidates
                    .extend(candidates[final_start..].iter().cloned());

                let diagnostic_start = entry.diagnostics.len().min(candidates.len());
                entry
                    .diagnostics
                    .extend(candidates[diagnostic_start..].iter().enumerate().map(
                        |(offset, candidate)| CandidateDiagnostic {
                            rank: (diagnostic_start + offset + 1) as u32,
                            rime_candidate: Some(candidate.clone()),
                            llm_candidate: None,
                            logprob: 0.0,
                            logprobs: Vec::new(),
                            mismatch: false,
                            display_candidate: Some(candidate.clone()),
                        },
                    ));
                true
            }
        };
        if changed {
            self.bump_history_revision();
        }
    }

    fn next_timestamp_ms(&self) -> u64 {
        let now = now_unix_ms();
        let mut previous = self.history_clock.load(Ordering::Acquire);
        loop {
            let next = now.max(previous.saturating_add(1));
            match self.history_clock.compare_exchange(
                previous,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return next,
                Err(observed) => previous = observed,
            }
        }
    }

    fn persist_model_presets_locked(
        &self,
        presets: &BTreeMap<String, ModelPreset>,
    ) -> Result<(), std::io::Error> {
        let active_model_path = self
            .active_model_path
            .lock()
            .map_err(|_| std::io::Error::other("active model mutex poisoned"))?
            .clone();
        persistence::persist_model_state(
            self.data_dir.as_deref(),
            presets,
            active_model_path.as_deref(),
        )
    }

    fn persist_config(&self, config: &lime_protocol::Config) -> Result<(), std::io::Error> {
        persistence::persist_config(self.data_dir.as_deref(), config)
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(elapsed_ms)
        .unwrap_or(0)
}

fn elapsed_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn model_display_name(runtime: Option<&LlamaRuntime>) -> Option<String> {
    let path = &runtime?.path;
    path.file_name()
        .filter(|name| !name.is_empty())
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .or_else(|| Some(normalize_path_string(path)))
}

fn normalize_path_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn memory_breakdown_sum(breakdown: &BTreeMap<String, u64>, kind: &str) -> Option<u64> {
    memory_breakdown_sum_for_kinds(breakdown, &[kind])
}

fn memory_breakdown_sum_for_kinds(
    breakdown: &BTreeMap<String, u64>,
    kinds: &[&str],
) -> Option<u64> {
    let mut found = false;
    let total = breakdown
        .iter()
        .filter(|(key, _)| {
            key.rsplit('.')
                .next()
                .is_some_and(|suffix| kinds.contains(&suffix))
        })
        .fold(0_u64, |total, (_, value)| {
            found = true;
            total.saturating_add(*value)
        });
    found.then_some(total)
}

fn memory_breakdown_total(breakdown: &BTreeMap<String, u64>) -> Option<u64> {
    (!breakdown.is_empty()).then(|| breakdown.values().copied().fold(0_u64, u64::saturating_add))
}

fn service_state(rime_available: bool, model_loaded: bool, model_loading: bool) -> ServiceState {
    if !rime_available {
        ServiceState::Unavailable
    } else if model_loading {
        ServiceState::Reloading
    } else if model_loaded {
        ServiceState::Ready
    } else {
        ServiceState::RimeOnly
    }
}

fn truncate_chars(value: &str, limit: usize) -> String {
    let count = value.chars().count();
    if count <= limit {
        return value.to_owned();
    }
    value.chars().skip(count - limit).collect()
}

#[cfg(test)]
mod tests {
    use super::persistence::{VersionedModelPresets, MODEL_PRESETS_FILE_VERSION};
    use super::*;
    use lime_protocol::Config;

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
            serde_json::from_slice(&fs::read(directory.join("model-presets.json")).unwrap())
                .unwrap();
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
            serde_json::from_slice(&fs::read(directory.join("model-presets.json")).unwrap())
                .unwrap();
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
}
