use super::*;
use lime_benchmark::{build_report, builtin_dataset, observe, preedit, InputMode, Prediction};
use lime_protocol::{
    BenchmarkDataset, BenchmarkRunRequest, BenchmarkRunState, BenchmarkRunStatus, InputRequest,
    Response,
};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

/// Mutable state for the management benchmark worker.
pub(crate) struct BenchmarkControl {
    pub state: BenchmarkRunState,
    pub cancel: Option<Arc<AtomicBool>>,
}

impl BenchmarkControl {
    pub fn new() -> Self {
        let dataset = builtin_dataset().expect("bundled benchmark dataset must be valid");
        Self {
            state: BenchmarkRunState::idle(&dataset),
            cancel: None,
        }
    }
}

impl CoreService {
    pub(super) fn benchmark_dataset(&self) -> BenchmarkDataset {
        builtin_dataset().expect("bundled benchmark dataset must be valid")
    }

    pub(super) fn benchmark_status(&self) -> BenchmarkRunState {
        self.benchmark
            .lock()
            .expect("benchmark mutex poisoned")
            .state
            .clone()
    }

    pub(super) fn start_benchmark(&self, request: BenchmarkRunRequest) -> Response {
        let dataset = self.benchmark_dataset();
        let modes = match normalize_modes(request.modes) {
            Ok(modes) => modes,
            Err(error) => return invalid_benchmark_request(error),
        };
        let categories = match normalize_categories(&dataset, request.categories) {
            Ok(categories) => categories,
            Err(error) => return invalid_benchmark_request(error),
        };
        let selected_cases = dataset
            .cases
            .iter()
            .filter(|case| categories.contains(case.category.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if selected_cases.is_empty() {
            return invalid_benchmark_request("benchmark category selection is empty".to_owned());
        }

        let snapshot = self.config_snapshot();
        if snapshot.config.rime_schema != "rime_ice" {
            return invalid_benchmark_request(
                "benchmark currently requires the rime_ice full-pinyin schema".to_owned(),
            );
        }
        if self.model_loading.load(Ordering::Acquire) {
            return invalid_benchmark_request("模型正在加载，请稍后再开始评测".to_owned());
        }
        let (model_name, model_sha256) = self
            .model
            .lock()
            .expect("model mutex poisoned")
            .as_ref()
            .map(|model| {
                (
                    model
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(ToOwned::to_owned),
                    Some(model.sha256.clone()),
                )
            })
            .unwrap_or((None, None));
        let total = selected_cases.len().saturating_mul(modes.len()) as u32;
        let state = BenchmarkRunState {
            status: BenchmarkRunStatus::Running,
            dataset_id: dataset.id.clone(),
            dataset_name: dataset.name.clone(),
            dataset_version: dataset.version,
            config_revision: snapshot.revision,
            model_name,
            model_sha256: model_sha256.clone(),
            config: Some(snapshot.config.clone()),
            total,
            completed: 0,
            report: None,
            error: None,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut control = self.benchmark.lock().expect("benchmark mutex poisoned");
            if matches!(
                control.state.status,
                BenchmarkRunStatus::Running | BenchmarkRunStatus::Stopping
            ) {
                return invalid_benchmark_request("benchmark is already running".to_owned());
            }
            control.state = state.clone();
            control.cancel = Some(Arc::clone(&cancel));
        }

        let service = self.clone();
        let expected_model_sha256 = model_sha256.clone();
        thread::spawn(move || {
            service.run_benchmark_worker(
                dataset,
                selected_cases,
                modes,
                snapshot.revision,
                expected_model_sha256,
                cancel,
            );
        });
        Response::BenchmarkState(state)
    }

    pub(super) fn stop_benchmark(&self) -> Response {
        let mut control = self.benchmark.lock().expect("benchmark mutex poisoned");
        let cancel = control.cancel.clone();
        if control.state.status == BenchmarkRunStatus::Running {
            control.state.status = BenchmarkRunStatus::Stopping;
            if let Some(cancel) = cancel {
                cancel.store(true, Ordering::Release);
            }
        }
        Response::BenchmarkState(control.state.clone())
    }

    fn run_benchmark_worker(
        &self,
        dataset: BenchmarkDataset,
        cases: Vec<lime_benchmark::Case>,
        modes: Vec<InputMode>,
        config_revision: u64,
        model_sha256: Option<String>,
        cancel: Arc<AtomicBool>,
    ) {
        let selected_dataset = BenchmarkDataset {
            id: dataset.id.clone(),
            name: dataset.name.clone(),
            version: dataset.version,
            cases,
        };
        let mut observations =
            Vec::with_capacity(selected_dataset.cases.len().saturating_mul(modes.len()));
        let mut run_error = None;
        'cases: for case in &selected_dataset.cases {
            for mode in &modes {
                if cancel.load(Ordering::Acquire) {
                    break 'cases;
                }
                if self.config_snapshot().revision != config_revision {
                    run_error = Some("配置在评测期间发生变化，结果已停止".to_owned());
                    break 'cases;
                }
                if self.model_loading.load(Ordering::Acquire) {
                    run_error = Some("模型在评测期间发生变化，结果已停止".to_owned());
                    break 'cases;
                }
                let current_model_sha256 = self
                    .model
                    .lock()
                    .expect("model mutex poisoned")
                    .as_ref()
                    .map(|model| model.sha256.clone());
                if current_model_sha256 != model_sha256 {
                    run_error = Some("模型在评测期间发生变化，结果已停止".to_owned());
                    break 'cases;
                }
                let started = Instant::now();
                let request_id = self.benchmark_request_id.fetch_add(1, Ordering::Relaxed);
                let request = InputRequest {
                    request_id,
                    preedit: preedit(case, *mode),
                    preceding_text: case.context.clone(),
                    context_available: true,
                    config_revision,
                    candidate_extension_of: None,
                    candidate_limit: self.config_snapshot().config.page_size,
                };
                let prediction = match self.input_for_benchmark(request) {
                    Ok(response) => Prediction {
                        top1: response
                            .diagnostics
                            .iter()
                            .find_map(|row| row.llm_candidate.as_ref())
                            .map(|candidate| candidate.commit_text.clone()),
                        error: None,
                        elapsed_ms: response
                            .end_to_end_duration_ms
                            .or_else(|| Some(elapsed_ms(started.elapsed()))),
                    },
                    Err(code) => Prediction {
                        top1: None,
                        error: Some(code.name().to_owned()),
                        elapsed_ms: Some(elapsed_ms(started.elapsed())),
                    },
                };
                observations.push(observe(case, *mode, prediction));
                update_benchmark_progress(self, observations.len() as u32);
            }
        }

        let mut report = match build_report(&selected_dataset, &modes, observations) {
            Ok(report) => report,
            Err(error) => {
                finish_benchmark(self, BenchmarkRunStatus::Failed, None, Some(error));
                return;
            }
        };
        let stopped = cancel.load(Ordering::Acquire);
        if stopped || run_error.is_some() {
            report.complete = false;
            for summary in &mut report.summaries {
                summary.accuracy = None;
            }
        }
        let status = if stopped {
            BenchmarkRunStatus::Cancelled
        } else if run_error.is_some() {
            BenchmarkRunStatus::Failed
        } else {
            BenchmarkRunStatus::Completed
        };
        finish_benchmark(self, status, Some(report), run_error);
    }
}

fn normalize_modes(modes: Vec<InputMode>) -> Result<Vec<InputMode>, String> {
    if modes.is_empty() {
        return Err("at least one benchmark input mode is required".to_owned());
    }
    let mut result = Vec::with_capacity(modes.len());
    for mode in modes {
        if !result.contains(&mode) {
            result.push(mode);
        }
    }
    Ok(result)
}

fn normalize_categories(
    dataset: &BenchmarkDataset,
    categories: Vec<String>,
) -> Result<HashSet<String>, String> {
    let known = dataset
        .cases
        .iter()
        .map(|case| case.category.as_str())
        .collect::<HashSet<_>>();
    if categories.is_empty() {
        return Ok(known.into_iter().map(ToOwned::to_owned).collect());
    }
    let mut selected = HashSet::new();
    for category in categories {
        let category = category.trim();
        if !known.contains(category) {
            return Err(format!("unknown benchmark category: {category}"));
        }
        selected.insert(category.to_owned());
    }
    Ok(selected)
}

fn invalid_benchmark_request(_error: String) -> Response {
    Response::Error {
        code: ErrorCode::InvalidRequest,
    }
}

fn update_benchmark_progress(service: &CoreService, completed: u32) {
    if let Ok(mut control) = service.benchmark.lock() {
        control.state.completed = completed;
    }
}

fn finish_benchmark(
    service: &CoreService,
    status: BenchmarkRunStatus,
    report: Option<lime_benchmark::Report>,
    error: Option<String>,
) {
    if let Ok(mut control) = service.benchmark.lock() {
        control.state.status = status;
        control.state.report = report;
        control.state.error = error;
        control.cancel = None;
    }
}
