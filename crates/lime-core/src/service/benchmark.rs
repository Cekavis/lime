use super::*;
use lime_benchmark::{
    builtin_dataset_info, builtin_dataset_sha256, observe, preedit, visit_builtin_cases, Case,
    DatasetInfo, InputMode, Prediction, Report, Summary,
};
use lime_protocol::{
    BenchmarkDataset, BenchmarkResult, BenchmarkRunRequest, BenchmarkRunState, BenchmarkRunStatus,
    Config,
};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;

const MAX_BATCH_RUNS: usize = 32;
const ERROR_EXAMPLES_PER_CORPUS: usize = 8;
const CANCELLED: &str = "评测已停止";

type CandidateSnapshot = BTreeMap<String, CandidateBatch>;

pub(crate) struct BenchmarkControl {
    pub state: BenchmarkRunState,
    pub cancel: Option<Arc<AtomicBool>>,
}

impl BenchmarkControl {
    pub fn new() -> Self {
        let dataset = builtin_dataset_info().expect("bundled benchmark corpus must be valid");
        Self {
            state: BenchmarkRunState::idle(&dataset),
            cancel: None,
        }
    }
}

#[derive(Clone)]
struct PlannedRun {
    result: BenchmarkResult,
    model_path: PathBuf,
}

fn plan_runs(
    request: BenchmarkRunRequest,
    presets: &[ModelPreset],
    base: &Config,
) -> Result<Vec<PlannedRun>, String> {
    if request.models.is_empty() || request.modes.is_empty() || request.configurations.is_empty() {
        return Err("请选择模型、拼音方式和评测配置".into());
    }
    if base.rime_schema != "rime_ice" {
        return Err("评测需要 rime_ice 全拼方案".into());
    }
    let models = unique(request.models);
    let modes = unique(request.modes);
    let configurations = unique(request.configurations);
    if models
        .len()
        .saturating_mul(modes.len())
        .saturating_mul(configurations.len())
        > MAX_BATCH_RUNS
    {
        return Err("一次最多评测 32 种组合".into());
    }
    let mut runs = Vec::new();
    for name in models {
        let preset = presets
            .iter()
            .find(|preset| preset.name == name)
            .ok_or_else(|| format!("模型预设不存在：{name}"))?;
        for configuration in &configurations {
            let mut config = base.clone();
            config.llm_rerank_count = configuration.llm_rerank_count;
            config.preceding_text_char_limit = configuration.preceding_text_char_limit;
            // Display length and promoted row count cannot alter the first candidate.
            config.page_size = 1;
            config.llm_effective_count = 1;
            config.context_preview_char_limit = 0;
            crate::config::validate(&config).map_err(|error| error.to_string())?;
            for mode in &modes {
                runs.push(PlannedRun {
                    model_path: PathBuf::from(&preset.path),
                    result: BenchmarkResult {
                        id: format!("run-{}", runs.len() + 1),
                        model_name: name.clone(),
                        model_sha256: None,
                        configuration: configuration.clone(),
                        mode: *mode,
                        config: config.clone(),
                        status: BenchmarkRunStatus::Idle,
                        report: None,
                        error: None,
                    },
                });
            }
        }
    }
    Ok(runs)
}

fn unique<T: Eq + std::hash::Hash + Clone>(items: Vec<T>) -> Vec<T> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert(item.clone()))
        .collect()
}

impl CoreService {
    pub(super) fn benchmark_dataset(&self) -> BenchmarkDataset {
        builtin_dataset_info().expect("bundled benchmark corpus must be valid")
    }

    pub(super) fn benchmark_status(&self) -> BenchmarkRunState {
        self.benchmark
            .lock()
            .expect("benchmark mutex poisoned")
            .state
            .clone()
    }

    pub(super) fn start_benchmark(&self, request: BenchmarkRunRequest) -> Response {
        let snapshot = self.config_snapshot();
        let runs = match plan_runs(request, &self.model_presets(), &snapshot.config) {
            Ok(runs) => runs,
            Err(_) => {
                return Response::Error {
                    code: ErrorCode::InvalidRequest,
                }
            }
        };
        let dataset = self.benchmark_dataset();
        let mut state = BenchmarkRunState::idle(&dataset);
        state.status = BenchmarkRunStatus::Running;
        state.config_revision = snapshot.revision;
        state.total = (dataset
            .corpora
            .iter()
            .map(|corpus| corpus.cases)
            .sum::<usize>()
            * runs.len()) as u32;
        state.results = runs.iter().map(|run| run.result.clone()).collect();
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut control = self.benchmark.lock().expect("benchmark mutex poisoned");
            if matches!(
                control.state.status,
                BenchmarkRunStatus::Running | BenchmarkRunStatus::Stopping
            ) {
                return Response::Error {
                    code: ErrorCode::InvalidRequest,
                };
            }
            control.state = state.clone();
            control.cancel = Some(Arc::clone(&cancel));
        }
        let service = self.clone();
        std::thread::spawn(move || service.run_benchmark_batch(dataset, runs, cancel));
        Response::BenchmarkState(state)
    }

    pub(super) fn stop_benchmark(&self) -> Response {
        let mut control = self.benchmark.lock().expect("benchmark mutex poisoned");
        if control.state.status == BenchmarkRunStatus::Running {
            control.state.status = BenchmarkRunStatus::Stopping;
            if let Some(cancel) = &control.cancel {
                cancel.store(true, Ordering::Release);
            }
        }
        Response::BenchmarkState(control.state.clone())
    }

    /// Freeze only candidate lists, not the repeatedly duplicated text prefixes.
    /// The revision is checked under the same mutex used for Rime mutations.
    fn capture_benchmark_candidates(
        &self,
        runs: &[PlannedRun],
        cancel: &AtomicBool,
    ) -> Result<(CandidateSnapshot, String), String> {
        let max_candidates = runs
            .iter()
            .map(|run| run.result.configuration.llm_rerank_count)
            .max()
            .unwrap() as usize;
        let modes = unique(runs.iter().map(|run| run.result.mode).collect());
        let revision = {
            let engine = self.engine.lock().map_err(|_| "Rime 状态不可用")?;
            if !engine.is_available() || engine.active_schema() != Some("rime_ice") {
                return Err("Rime 不可用或未启用 rime_ice 方案".into());
            }
            engine.revision()
        };
        let mut candidates = BTreeMap::new();
        visit_builtin_cases(1, |case| {
            if cancel.load(Ordering::Acquire) {
                return Err(CANCELLED.into());
            }
            for mode in &modes {
                let input = preedit(&case, *mode);
                if candidates.contains_key(&input) {
                    continue;
                }
                let mut engine = self.engine.lock().map_err(|_| "Rime 状态不可用")?;
                if engine.revision() != revision {
                    return Err("生成评测快照时 Rime 方案或词库发生变化，请重新开始".into());
                }
                let batch = engine
                    .candidates_for_rerank(&input, max_candidates, max_candidates)
                    .map_err(|error| error.code.name().to_owned())?;
                candidates.insert(input, batch);
            }
            Ok(())
        })?;
        if self
            .engine
            .lock()
            .map_err(|_| "Rime 状态不可用")?
            .revision()
            != revision
        {
            return Err("生成评测快照时 Rime 方案或词库发生变化，请重新开始".into());
        }
        let mut digest = Sha256::new();
        for (input, batch) in &candidates {
            let bytes =
                serde_json::to_vec(&(input, &batch.candidates, &batch.complete_candidate_indices))
                    .map_err(|error| error.to_string())?;
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        }
        Ok((candidates, format!("{:x}", digest.finalize())))
    }

    fn run_benchmark_batch(
        &self,
        dataset: DatasetInfo,
        runs: Vec<PlannedRun>,
        cancel: Arc<AtomicBool>,
    ) {
        let (candidates, fingerprint) = match self.capture_benchmark_candidates(&runs, &cancel) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.finish_benchmark_batch(&cancel, Some(error));
                return;
            }
        };
        self.benchmark
            .lock()
            .expect("benchmark mutex poisoned")
            .state
            .rime_snapshot_sha256 = Some(fingerprint);
        let dataset_sha256 = match builtin_dataset_sha256() {
            Ok(value) => value,
            Err(error) => {
                self.finish_benchmark_batch(&cancel, Some(error));
                return;
            }
        };
        let mut runtime: Option<LlamaRuntime> = None;
        let mut runtime_key: Option<(PathBuf, u32, u32, String)> = None;
        for (index, run) in runs.iter().enumerate() {
            if cancel.load(Ordering::Acquire) {
                break;
            }
            let mut result = run.result.clone();
            result.status = BenchmarkRunStatus::Running;
            self.publish_benchmark_result(index, result.clone());
            let key = (
                run.model_path.clone(),
                result.config.llm_context_token_limit,
                result.config.llm_rerank_count,
                result.config.llm_backend.clone(),
            );
            if runtime_key.as_ref() != Some(&key) {
                // Release the preceding benchmark model before allocating another. The daily
                // input model remains owned by self.model and is never replaced or persisted.
                runtime_key = None;
                let loaded = {
                    let _load_guard = self.model_load.lock().expect("model load mutex poisoned");
                    // Destruction also mutates process-global llama.cpp backend state.
                    // Keep it serialized with daily model loads and unloads.
                    runtime = None;
                    LlamaRuntime::load_with_backend_preference_and_sequence_count(
                        run.model_path.clone(),
                        result.config.llm_context_token_limit as usize,
                        backend_preference_for(&result.config.llm_backend),
                        result.config.llm_rerank_count as usize,
                    )
                };
                match loaded {
                    Ok(model) => {
                        runtime = Some(model);
                        runtime_key = Some(key);
                    }
                    Err(error) => {
                        result.status = BenchmarkRunStatus::Failed;
                        result.error = Some(error);
                        self.publish_benchmark_result(index, result);
                        continue;
                    }
                }
            }
            let model = runtime.as_ref().expect("benchmark runtime loaded");
            result.model_sha256 = Some(model.sha256.clone());
            let mut accumulator = BenchmarkAccumulator::new(&dataset, &dataset_sha256, result.mode);
            let evaluation =
                visit_builtin_cases(result.config.preceding_text_char_limit as usize, |case| {
                    if cancel.load(Ordering::Acquire) {
                        return Err(CANCELLED.into());
                    }
                    let input = preedit(&case, result.mode);
                    let batch = candidates.get(&input).ok_or("评测候选快照缺少目标拼音")?;
                    let started = Instant::now();
                    let ranking = try_rerank_selected_candidates_with_preedit_and_limit(
                        &batch.candidates,
                        &batch.complete_candidate_indices,
                        &input,
                        &case.context,
                        Some(model),
                        RerankOptions {
                            rerank_count: result.config.llm_rerank_count as usize,
                            effective_count: 1,
                            inference_count_limit: result.config.llm_inference_count_limit as usize,
                            ignore_emoji: result.config.llm_ignore_emoji,
                        },
                    );
                    let prediction = match ranking {
                        Ok(ranking) => Prediction {
                            top1: ranking
                                .result
                                .diagnostics
                                .iter()
                                .find_map(|row| row.llm_candidate.as_ref())
                                .map(|candidate| candidate.commit_text.clone()),
                            error: None,
                            elapsed_ms: Some(elapsed_ms(started.elapsed())),
                        },
                        Err(error) => Prediction {
                            top1: None,
                            error: Some(error),
                            elapsed_ms: Some(elapsed_ms(started.elapsed())),
                        },
                    };
                    accumulator.observe(&case, prediction)?;
                    self.benchmark
                        .lock()
                        .expect("benchmark mutex poisoned")
                        .state
                        .completed += 1;
                    Ok(())
                });
            let stopped = cancel.load(Ordering::Acquire);
            result.status = if stopped {
                BenchmarkRunStatus::Cancelled
            } else if evaluation.is_err() {
                BenchmarkRunStatus::Failed
            } else {
                BenchmarkRunStatus::Completed
            };
            result.error = evaluation.err().filter(|_| !stopped);
            result.report =
                Some(accumulator.finish(result.status == BenchmarkRunStatus::Completed));
            self.publish_benchmark_result(index, result);
        }
        {
            let _load_guard = self.model_load.lock().expect("model load mutex poisoned");
            drop(runtime);
        }
        self.finish_benchmark_batch(&cancel, None);
    }

    fn publish_benchmark_result(&self, index: usize, result: BenchmarkResult) {
        self.benchmark
            .lock()
            .expect("benchmark mutex poisoned")
            .state
            .results[index] = result;
    }

    fn finish_benchmark_batch(&self, cancel: &AtomicBool, error: Option<String>) {
        let mut control = self.benchmark.lock().expect("benchmark mutex poisoned");
        let stopped = cancel.load(Ordering::Acquire);
        let failed = error.is_some()
            || control
                .state
                .results
                .iter()
                .any(|row| row.status == BenchmarkRunStatus::Failed);
        let status = if stopped {
            BenchmarkRunStatus::Cancelled
        } else if failed {
            BenchmarkRunStatus::Failed
        } else {
            BenchmarkRunStatus::Completed
        };
        for row in &mut control.state.results {
            if matches!(
                row.status,
                BenchmarkRunStatus::Idle | BenchmarkRunStatus::Running
            ) {
                row.status = status;
                row.error = error.clone().filter(|_| !stopped);
            }
        }
        control.state.status = status;
        control.state.error = if stopped { None } else { error };
        control.cancel = None;
    }
}

/// Exact, streaming counters. No target is sampled out of the denominator; only the
/// displayed mistakes are bounded so a batch fits comfortably inside one IPC frame.
struct BenchmarkAccumulator {
    report: Report,
    seen: HashSet<String>,
    example_counts: HashMap<String, usize>,
}

impl BenchmarkAccumulator {
    fn new(dataset: &DatasetInfo, fingerprint: &str, mode: InputMode) -> Self {
        let summary = |category, total| Summary {
            category,
            mode,
            total,
            completed: 0,
            correct: 0,
            no_prediction: 0,
            errors: 0,
            accuracy: None,
        };
        let mut summaries = dataset
            .corpora
            .iter()
            .map(|corpus| summary(Some(corpus.id.clone()), corpus.cases))
            .collect::<Vec<_>>();
        summaries.push(summary(
            None,
            dataset.corpora.iter().map(|corpus| corpus.cases).sum(),
        ));
        Self {
            report: Report {
                dataset_id: dataset.id.clone(),
                dataset_name: dataset.name.clone(),
                dataset_version: dataset.version,
                dataset_sha256: fingerprint.into(),
                modes: vec![mode],
                summaries,
                observations: Vec::new(),
                complete: false,
            },
            seen: HashSet::new(),
            example_counts: HashMap::new(),
        }
    }

    fn observe(&mut self, case: &Case, prediction: Prediction) -> Result<(), String> {
        lime_benchmark::validate_case(case)?;
        if !self.seen.insert(case.id.clone()) {
            return Err(format!("duplicate benchmark case: {}", case.id));
        }
        if !self
            .report
            .summaries
            .iter()
            .any(|summary| summary.category.as_deref() == Some(&case.category))
        {
            return Err(format!("unknown benchmark corpus: {}", case.category));
        }
        let observation = observe(case, self.report.modes[0], prediction);
        for summary in &mut self.report.summaries {
            if summary.category.is_none() || summary.category.as_deref() == Some(&case.category) {
                summary.completed += 1;
                summary.correct += usize::from(observation.correct);
                summary.errors += usize::from(observation.error.is_some());
                summary.no_prediction +=
                    usize::from(observation.error.is_none() && observation.top1.is_none());
            }
        }
        if !observation.correct {
            let count = self
                .example_counts
                .entry(case.category.clone())
                .or_default();
            if *count < ERROR_EXAMPLES_PER_CORPUS {
                self.report.observations.push(observation);
                *count += 1;
            }
        }
        Ok(())
    }

    fn finish(mut self, completed: bool) -> Report {
        self.report.complete = completed
            && self
                .report
                .summaries
                .iter()
                .all(|row| row.completed == row.total);
        if self.report.complete {
            for summary in &mut self.report.summaries {
                summary.accuracy =
                    (summary.total > 0).then(|| summary.correct as f64 / summary.total as f64);
            }
        }
        self.report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lime_benchmark::{build_report, CorpusInfo, Dataset};
    use lime_protocol::BenchmarkConfiguration;

    fn preset(name: &str) -> ModelPreset {
        ModelPreset {
            name: name.into(),
            path: format!("{name}.gguf"),
            size_bytes: None,
            sha256: None,
            loaded: false,
        }
    }

    fn configuration(count: u32, context: u32) -> BenchmarkConfiguration {
        BenchmarkConfiguration {
            llm_rerank_count: count,
            preceding_text_char_limit: context,
        }
    }

    fn request() -> BenchmarkRunRequest {
        BenchmarkRunRequest {
            models: vec!["first".into(), "second".into(), "first".into()],
            modes: vec![InputMode::Full, InputMode::Initials, InputMode::Full],
            configurations: vec![
                configuration(1, 16),
                configuration(32, 128),
                configuration(1, 16),
            ],
        }
    }

    #[test]
    fn matrix_is_deduplicated_and_does_not_change_live_configuration() {
        let base = Config::default();
        let original = base.clone();
        let plan = plan_runs(request(), &[preset("first"), preset("second")], &base).unwrap();
        assert_eq!(plan.len(), 8);
        assert_eq!(base, original);
        assert_eq!(
            plan.iter()
                .map(|run| &run.result.id)
                .collect::<HashSet<_>>()
                .len(),
            8
        );
        for run in &plan {
            assert_eq!(run.result.config.llm_effective_count, 1);
            assert_eq!(run.result.config.page_size, 1);
            assert_eq!(run.result.config.context_preview_char_limit, 0);
            assert_eq!(
                run.result.config.llm_context_token_limit,
                base.llm_context_token_limit
            );
            assert_eq!(
                run.result.config.llm_inference_count_limit,
                base.llm_inference_count_limit
            );
            assert_eq!(run.result.config.llm_backend, base.llm_backend);
            assert_eq!(run.result.config.llm_ignore_emoji, base.llm_ignore_emoji);
        }
    }

    #[test]
    fn matrix_rejects_invalid_empty_unknown_and_excessive_selections() {
        let presets = vec![preset("first"), preset("second")];
        for configurations in [
            vec![],
            vec![configuration(0, 128)],
            vec![configuration(129, 128)],
            vec![configuration(32, 0)],
            vec![configuration(32, 4097)],
        ] {
            let mut value = request();
            value.configurations = configurations;
            assert!(plan_runs(value, &presets, &Config::default()).is_err());
        }
        let mut value = request();
        value.models = vec!["missing".into()];
        assert!(plan_runs(value, &presets, &Config::default()).is_err());
        let mut value = request();
        value.modes.clear();
        assert!(plan_runs(value, &presets, &Config::default()).is_err());
        let mut value = request();
        value.models.clear();
        assert!(plan_runs(value, &presets, &Config::default()).is_err());
        let mut value = request();
        value.configurations = (1..=9).map(|count| configuration(count, 128)).collect();
        assert!(plan_runs(value, &presets, &Config::default()).is_err());
    }

    fn fixtures() -> (Dataset, DatasetInfo) {
        let cases = (0..24)
            .map(|index| Case {
                id: format!("word-{index}"),
                category: if index < 20 { "zhihu" } else { "classics" }.into(),
                context: "先有，ABC123。".into(),
                expected: "文字".into(),
                syllables: vec!["wen".into(), "zi".into()],
            })
            .collect();
        let dataset = Dataset {
            id: "fixture".into(),
            name: "Fixture".into(),
            version: 1,
            cases,
        };
        let info = DatasetInfo {
            id: dataset.id.clone(),
            name: dataset.name.clone(),
            version: dataset.version,
            corpora: vec![
                CorpusInfo {
                    id: "zhihu".into(),
                    name: "知乎".into(),
                    characters: 100,
                    cases: 20,
                },
                CorpusInfo {
                    id: "classics".into(),
                    name: "经典文章".into(),
                    characters: 100,
                    cases: 4,
                },
            ],
        };
        (dataset, info)
    }

    #[test]
    fn streaming_scores_match_full_report_and_use_weighted_total() {
        let (dataset, info) = fixtures();
        let mut streaming = BenchmarkAccumulator::new(&info, "fingerprint", InputMode::Full);
        let mut observations = Vec::new();
        for (index, case) in dataset.cases.iter().enumerate() {
            let prediction = Prediction {
                top1: (index >= 20).then(|| case.expected.clone()),
                error: (index == 1).then(|| "runtime failure".into()),
                elapsed_ms: None,
            };
            observations.push(observe(case, InputMode::Full, prediction.clone()));
            streaming.observe(case, prediction).unwrap();
        }
        let expected = build_report(&dataset, &[InputMode::Full], observations).unwrap();
        let report = streaming.finish(true);
        assert!(report.complete);
        assert_eq!(report.summaries, expected.summaries);
        assert_eq!(report.summaries.last().unwrap().accuracy, Some(4.0 / 24.0));
        assert_eq!(report.observations.len(), ERROR_EXAMPLES_PER_CORPUS);
        assert!(report.observations.iter().all(|row| !row.correct));
        assert_eq!(report.summaries[0].errors, 1);
        assert_eq!(report.summaries[0].no_prediction, 19);
    }

    #[test]
    fn streaming_rejects_duplicates_and_never_scores_an_incomplete_run() {
        let (dataset, info) = fixtures();
        let mut streaming = BenchmarkAccumulator::new(&info, "fingerprint", InputMode::Full);
        let prediction = Prediction {
            top1: Some("文字".into()),
            error: None,
            elapsed_ms: None,
        };
        streaming
            .observe(&dataset.cases[0], prediction.clone())
            .unwrap();
        assert!(streaming
            .observe(&dataset.cases[0], prediction.clone())
            .is_err());
        let report = streaming.finish(true);
        assert!(!report.complete);
        assert!(report.summaries.iter().all(|row| row.accuracy.is_none()));
        let mut streaming = BenchmarkAccumulator::new(&info, "fingerprint", InputMode::Full);
        for case in &dataset.cases {
            streaming.observe(case, prediction.clone()).unwrap();
        }
        let report = streaming.finish(false);
        assert!(!report.complete);
        assert!(report.summaries.iter().all(|row| row.accuracy.is_none()));
    }

    #[test]
    fn failed_batch_retains_completed_rows_and_marks_pending_rows() {
        let service = CoreService::default();
        let plan = plan_runs(
            request(),
            &[preset("first"), preset("second")],
            &Config::default(),
        )
        .unwrap();
        {
            let mut control = service.benchmark.lock().unwrap();
            control.state.results = plan.iter().map(|run| run.result.clone()).collect();
            control.state.results[0].status = BenchmarkRunStatus::Completed;
        }
        let before = service.config_snapshot();
        service.finish_benchmark_batch(&AtomicBool::new(true), None);
        let state = service.benchmark_status();
        assert_eq!(state.status, BenchmarkRunStatus::Cancelled);
        assert_eq!(state.results[0].status, BenchmarkRunStatus::Completed);
        assert!(state.results[1..]
            .iter()
            .all(|row| row.status == BenchmarkRunStatus::Cancelled));
        assert_eq!(service.config_snapshot(), before);
        assert!(service.model.lock().unwrap().is_none());
        assert!(service.history.lock().unwrap().is_empty());
    }

    #[test]
    fn maximum_batch_of_long_error_examples_fits_ipc_frame() {
        let (_, info) = fixtures();
        let mut report =
            BenchmarkAccumulator::new(&info, &"0".repeat(64), InputMode::Full).finish(false);
        let long_case = Case {
            id: "long".into(),
            category: "zhihu".into(),
            context: "𠀀".repeat(4096),
            expected: "文字".into(),
            syllables: vec!["wen".into(), "zi".into()],
        };
        report.observations = (0..2 * ERROR_EXAMPLES_PER_CORPUS)
            .map(|_| {
                observe(
                    &long_case,
                    InputMode::Full,
                    Prediction {
                        top1: None,
                        error: None,
                        elapsed_ms: None,
                    },
                )
            })
            .collect();
        let mut state = BenchmarkRunState::idle(&info);
        for index in 0..MAX_BATCH_RUNS {
            state.results.push(BenchmarkResult {
                id: format!("{index}"),
                model_name: "model".into(),
                model_sha256: Some("0".repeat(64)),
                configuration: configuration(128, 4096),
                mode: InputMode::Full,
                config: Config::default(),
                status: BenchmarkRunStatus::Cancelled,
                report: Some(report.clone()),
                error: None,
            });
        }
        let response = Response::BenchmarkState(state);
        let mut bytes = Vec::new();
        lime_ipc::write_json(&mut bytes, &response).unwrap();
        assert!(bytes.len() < lime_ipc::MAX_FRAME_BYTES);
    }
}
