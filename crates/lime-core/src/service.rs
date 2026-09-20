use crate::{
    config::ConfigStore,
    engine::{CandidateBatch, CandidateEngine, RimeEngine},
    logging::PrivacyLogger,
    ranking::{
        try_rerank_selected_candidates_with_preedit_and_limit, GenerationTracker, LlamaRuntime,
        RerankOptions,
    },
};
mod benchmark;
mod history;
mod models;
mod persistence;
use lime_protocol::{
    Candidate, CandidateDiagnostic, ConfigSnapshot, DictionaryPage, ErrorCode, InputHistoryEntry,
    InputRequest, InputResponse, LlmPerformance, ModelInfo, ModelMemoryInfo, ModelPreset, Request,
    Response, ServiceState, ServiceStatus, DICTIONARY_PAGE_SIZE,
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

fn read_user_weasel_theme_file(data_dir: Option<&Path>, name: &str) -> String {
    let Some(data_dir) = data_dir else {
        return String::new();
    };
    fs::read_to_string(data_dir.join("rime").join(name)).unwrap_or_default()
}

fn backend_preference_for(value: &str) -> BackendPreference {
    match value {
        "cpu" => BackendPreference::Cpu,
        // Config validation only accepts `cuda` or `cpu`; treating any
        // unexpected persisted value as CUDA preserves the safe default while
        // still allowing the runtime's documented CPU fallback.
        _ => BackendPreference::Cuda,
    }
}

fn visible_candidate_count(candidate_count: usize, requested_limit: u32) -> usize {
    if requested_limit == 0 {
        candidate_count
    } else {
        candidate_count.min(requested_limit as usize)
    }
}

fn merge_cached_candidate_order(
    cached: &CandidateCacheEntry,
    candidates: &[Candidate],
    candidate_remainders: &[Option<String>],
) -> (Vec<Candidate>, Vec<Option<String>>) {
    let mut used = vec![false; candidates.len()];
    let mut ordered = Vec::with_capacity(candidates.len());
    let mut ordered_remainders = Vec::with_capacity(candidates.len());
    for (cached_index, cached_candidate) in cached.candidates.iter().enumerate() {
        let Some(index) = candidates
            .iter()
            .enumerate()
            .find(|(index, candidate)| !used[*index] && *candidate == cached_candidate)
            .map(|(index, _)| index)
        else {
            continue;
        };
        used[index] = true;
        ordered.push(candidates[index].clone());
        let remainder = match candidate_remainders.get(index) {
            Some(value @ Some(_)) => value.clone(),
            Some(None) | None => cached
                .candidate_remainders
                .get(cached_index)
                .cloned()
                .unwrap_or(None),
        };
        ordered_remainders.push(remainder);
    }
    for (index, candidate) in candidates.iter().enumerate() {
        if !used[index] {
            ordered.push(candidate.clone());
            ordered_remainders.push(candidate_remainders.get(index).cloned().unwrap_or(None));
        }
    }
    (ordered, ordered_remainders)
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
    /// Full final ordering for active candidate windows. Extensions use this to keep the
    /// already-visible pages stable while appending newly loaded Rime rows.
    candidate_cache: Arc<Mutex<BTreeMap<u64, CandidateCacheEntry>>>,
    history_clock: Arc<AtomicU64>,
    history_revision: Arc<(Mutex<u64>, Condvar)>,
    model_presets: Arc<Mutex<BTreeMap<String, ModelPreset>>>,
    /// Path of the last model that loaded successfully.  This is separate from
    /// `ModelPreset::loaded`, which is a runtime-only status bit exposed to clients.
    active_model_path: Arc<Mutex<Option<String>>>,
    model_loading: Arc<std::sync::atomic::AtomicBool>,
    benchmark: Arc<Mutex<benchmark::BenchmarkControl>>,
    benchmark_request_id: Arc<AtomicU64>,
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

#[derive(Clone, Debug)]
struct CandidateCacheEntry {
    preedit: String,
    preceding_text: String,
    candidates: Vec<Candidate>,
    candidate_remainders: Vec<Option<String>>,
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
            candidate_cache: Arc::new(Mutex::new(BTreeMap::new())),
            history_clock: Arc::new(AtomicU64::new(now_unix_ms())),
            history_revision: Arc::new((Mutex::new(0), Condvar::new())),
            model_presets: Arc::new(Mutex::new(model_presets)),
            active_model_path: Arc::new(Mutex::new(active_model_path.clone())),
            model_loading: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            benchmark: Arc::new(Mutex::new(benchmark::BenchmarkControl::new())),
            benchmark_request_id: Arc::new(AtomicU64::new(1)),
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
            Request::GetWeaselTheme => Response::WeaselTheme {
                base: read_user_weasel_theme_file(self.data_dir.as_deref(), "weasel.yaml"),
                custom: read_user_weasel_theme_file(self.data_dir.as_deref(), "weasel.custom.yaml"),
            },
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
                self.candidate_cache
                    .lock()
                    .expect("candidate cache mutex poisoned")
                    .clear();
                self.bump_history_revision();
                Response::Accepted
            }
            Request::GetBenchmarkDataset => Response::BenchmarkDataset(self.benchmark_dataset()),
            Request::StartBenchmark(request) => self.start_benchmark(request),
            Request::StopBenchmark => self.stop_benchmark(),
            Request::GetBenchmarkStatus => Response::BenchmarkState(self.benchmark_status()),
        }
    }

    fn input(&self, request: InputRequest) -> Result<InputResponse, ErrorCode> {
        self.input_with_options(request, true, true)
    }

    pub(crate) fn input_for_benchmark(
        &self,
        request: InputRequest,
    ) -> Result<InputResponse, ErrorCode> {
        self.input_with_options(request, false, false)
    }

    fn input_with_options(
        &self,
        request: InputRequest,
        record_history: bool,
        track_generation: bool,
    ) -> Result<InputResponse, ErrorCode> {
        let input_started = Instant::now();
        let snapshot = self.config_snapshot();
        if request.config_revision != snapshot.revision {
            if record_history && request.candidate_extension_of.is_none() {
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
        let generation = track_generation.then(|| self.generation.next());
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
        // Rime must provide enough rows for the configured reranker, while the response is
        // clipped to the client's requested prefix below.
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
                if record_history && request.candidate_extension_of.is_none() {
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
            self.append_candidate_history(original_request_id, &request, &rime_candidates);
            if !generation_is_current(&self.generation, generation) {
                return Err(ErrorCode::RequestCancelled);
            }
            let cached = self
                .candidate_cache
                .lock()
                .expect("candidate cache mutex poisoned")
                .get(&original_request_id)
                .filter(|entry| {
                    entry.preedit == request.preedit
                        && entry.preceding_text == request.preceding_text
                })
                .cloned();
            let (ordered_candidates, ordered_remainders) = cached
                .as_ref()
                .map(|entry| {
                    merge_cached_candidate_order(
                        entry,
                        &rime_candidates,
                        &rime_candidate_remainders,
                    )
                })
                .unwrap_or_else(|| (rime_candidates.clone(), rime_candidate_remainders.clone()));
            if cached.is_some() {
                if let Some(entry) = self
                    .candidate_cache
                    .lock()
                    .expect("candidate cache mutex poisoned")
                    .get_mut(&original_request_id)
                {
                    entry.candidates = ordered_candidates.clone();
                    entry.candidate_remainders = ordered_remainders.clone();
                }
            }
            let visible_count =
                visible_candidate_count(ordered_candidates.len(), request.candidate_limit);
            return Ok(InputResponse {
                request_id: request.request_id,
                candidates: ordered_candidates[..visible_count].to_vec(),
                candidate_remainders: ordered_remainders[..visible_count].to_vec(),
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
            RerankOptions {
                rerank_count: config.llm_rerank_count as usize,
                effective_count: config.llm_effective_count as usize,
                inference_count_limit: config.llm_inference_count_limit as usize,
                ignore_emoji: config.llm_ignore_emoji,
            },
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
        let final_candidates = ranking.result.candidates;
        let final_candidate_remainders = ranking
            .candidate_indices
            .iter()
            .map(|index| {
                rime_candidate_remainders
                    .get(*index)
                    .cloned()
                    .unwrap_or(None)
            })
            .collect::<Vec<_>>();
        let all_diagnostics = ranking.result.diagnostics;
        if track_generation {
            self.candidate_cache
                .lock()
                .expect("candidate cache mutex poisoned")
                .insert(
                    request.request_id,
                    CandidateCacheEntry {
                        preedit: request.preedit.clone(),
                        preceding_text: request.preceding_text.clone(),
                        candidates: final_candidates.clone(),
                        candidate_remainders: final_candidate_remainders.clone(),
                    },
                );
        }
        let end_to_end_duration_ms = Some(elapsed_ms(input_started.elapsed()));
        if record_history {
            self.record_input_history(InputHistoryRecord {
                request: &request,
                rime_candidates,
                final_candidates: final_candidates.clone(),
                diagnostics: all_diagnostics.clone(),
                service_state,
                model_name,
                rime_duration_ms,
                llm_performance: llm_performance.clone(),
                end_to_end_duration_ms,
            });
        }
        if !generation_is_current(&self.generation, generation) {
            return Err(ErrorCode::RequestCancelled);
        }
        let visible_count =
            visible_candidate_count(final_candidates.len(), request.candidate_limit);
        Ok(InputResponse {
            request_id: request.request_id,
            candidates: final_candidates[..visible_count].to_vec(),
            candidate_remainders: final_candidate_remainders[..visible_count].to_vec(),
            context_used: request.context_available && !request.preceding_text.is_empty(),
            service_state,
            diagnostics: all_diagnostics,
            end_to_end_duration_ms,
            rime_duration_ms,
            llm_performance,
        })
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

fn generation_is_current(tracker: &GenerationTracker, generation: Option<u64>) -> bool {
    match generation {
        Some(value) => tracker.is_current(value),
        None => true,
    }
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
mod tests;
