use super::*;
use lime_benchmark::{
    default_corpus_dir, load_corpus_with_pinyin_dictionary, observe, preedit, Case, Corpus,
    DatasetInfo, InputMode, PinyinDictionary, Prediction, Report, Summary,
};
use lime_protocol::{
    BenchmarkCellStatus, BenchmarkConfiguration, BenchmarkDataset, BenchmarkErrorPage,
    BenchmarkModelSelection, BenchmarkProgress, BenchmarkQueueItem, BenchmarkResult,
    BenchmarkResultCell, BenchmarkRunRequest, BenchmarkRunState, BenchmarkRunStatus, Config,
    Response,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

const MAX_BATCH_RUNS: usize = 32;
const BENCHMARK_ALGORITHM_VERSION: u32 = 2;
const CANCELLED: &str = "评测已停止";

type CandidateSnapshot = BTreeMap<String, CandidateBatch>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct CellIdentity {
    // Candidate snapshots are intentionally excluded: Rime may emit volatile
    // translator results without changing the benchmark inputs or model.
    model_fingerprint: String,
    effective_config: Config,
    mode: InputMode,
    corpus_id: String,
    corpus_sha256: String,
}

impl CellIdentity {
    fn key(&self) -> String {
        let bytes = serde_json::to_vec(&(
            BENCHMARK_ALGORITHM_VERSION,
            &self.model_fingerprint,
            &self.effective_config,
            self.mode,
            &self.corpus_id,
            &self.corpus_sha256,
        ))
        .expect("benchmark cell identity is serializable");
        format!("{:x}", Sha256::digest(bytes))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredBenchmarkItem {
    version: u32,
    #[serde(default)]
    saved_at: u64,
    key: String,
    model: BenchmarkModelSelection,
    model_name: String,
    model_sha256: Option<String>,
    configuration: BenchmarkConfiguration,
    mode: InputMode,
    config: Config,
    corpus_id: String,
    #[serde(default)]
    corpus_sha256: String,
    dataset_id: String,
    dataset_name: String,
    dataset_version: u32,
    dataset_sha256: String,
    rime_snapshot_sha256: String,
    report: Report,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredBenchmarkIndex {
    version: u32,
    items: BTreeMap<String, StoredBenchmarkIndexEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredBenchmarkIndexEntry {
    key: String,
    #[serde(default)]
    saved_at: u64,
    model: BenchmarkModelSelection,
    model_name: String,
    model_sha256: Option<String>,
    configuration: BenchmarkConfiguration,
    mode: InputMode,
    config: Config,
    corpus_id: String,
    #[serde(default)]
    corpus_sha256: String,
    dataset_id: String,
    dataset_name: String,
    dataset_version: u32,
    dataset_sha256: String,
    rime_snapshot_sha256: String,
}

struct BenchmarkStore {
    data_dir: Option<PathBuf>,
    items: BTreeMap<String, StoredBenchmarkItem>,
}

impl BenchmarkStore {
    fn valid_key(key: &str) -> bool {
        key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit())
    }

    fn new(data_dir: Option<&Path>) -> Self {
        let mut store = Self {
            data_dir: data_dir.map(Path::to_path_buf),
            items: BTreeMap::new(),
        };
        let Some(dir) = data_dir else {
            return store;
        };
        let Some(index) = persistence::read_json::<StoredBenchmarkIndex>(
            &dir.join("benchmark").join("index.json"),
        ) else {
            return store;
        };
        if index.version != BENCHMARK_ALGORITHM_VERSION {
            return store;
        }
        for (key, _entry) in index.items {
            if !Self::valid_key(&key) {
                continue;
            }
            let Some(item) = persistence::read_json::<StoredBenchmarkItem>(
                &dir.join("benchmark")
                    .join("items")
                    .join(format!("{key}.json")),
            ) else {
                continue;
            };
            if item.version == BENCHMARK_ALGORITHM_VERSION
                && item.key == key
                && item.report.complete
            {
                store.items.insert(key, item);
            }
        }
        store
    }

    fn stable_key_for_item(item: &StoredBenchmarkItem) -> Option<String> {
        let model_fingerprint = match (&item.model, &item.model_sha256) {
            (BenchmarkModelSelection::RimeOnly, _) => "rime-only".to_owned(),
            (BenchmarkModelSelection::Preset { .. }, Some(fingerprint)) => fingerprint.clone(),
            (BenchmarkModelSelection::Preset { .. }, None) => return None,
        };
        Some(
            CellIdentity {
                model_fingerprint,
                effective_config: item.config.clone(),
                mode: item.mode,
                corpus_id: item.corpus_id.clone(),
                corpus_sha256: item.corpus_sha256.clone(),
            }
            .key(),
        )
    }

    fn get(&self, key: &str) -> Option<&StoredBenchmarkItem> {
        self.items.get(key).or_else(|| {
            // Results written before the candidate snapshot was removed from
            // the key remain reusable through their stable identity.
            self.items
                .values()
                .filter(|item| Self::stable_key_for_item(item).as_deref() == Some(key))
                .max_by_key(|item| item.saved_at)
        })
    }

    fn save(&mut self, mut item: StoredBenchmarkItem) -> Result<(), String> {
        if !item.report.complete {
            return Err("未完成的评测结果不能保存为可复用记录".into());
        }
        if item.saved_at == 0 {
            item.saved_at = now_unix_ms();
        }
        let key = item.key.clone();
        if let Some(dir) = &self.data_dir {
            let items_dir = dir.join("benchmark").join("items");
            let bytes = serde_json::to_vec_pretty(&item).map_err(|error| error.to_string())?;
            persistence::atomic_write_file(&items_dir, &format!("{key}.json"), bytes)
                .map_err(|error| error.to_string())?;
        }
        let previous = self.items.insert(key.clone(), item);
        if let Err(error) = self.persist_index() {
            self.items.remove(&key);
            if let Some(previous) = previous {
                self.items.insert(key, previous);
            }
            return Err(error);
        }
        Ok(())
    }

    fn remove(&mut self, key: &str) -> Result<(), String> {
        let removed = self.items.remove(key);
        if let Err(error) = self.persist_index() {
            if let Some(removed) = removed {
                self.items.insert(key.to_owned(), removed);
            }
            return Err(error);
        }
        Ok(())
    }

    fn clear(&mut self) -> Result<(), String> {
        self.items.clear();
        let Some(dir) = &self.data_dir else {
            return Ok(());
        };
        let benchmark_dir = dir.join("benchmark");
        let items_dir = benchmark_dir.join("items");
        if items_dir.exists() {
            fs::remove_dir_all(items_dir).map_err(|error| error.to_string())?;
        }
        let index = benchmark_dir.join("index.json");
        if index.exists() {
            fs::remove_file(index).map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn logical_key(item: &StoredBenchmarkItem) -> String {
        serde_json::to_string(&(
            &item.model,
            &item.model_sha256,
            &item.configuration,
            item.mode,
            &item.config,
            &item.corpus_id,
        ))
        .unwrap_or_default()
    }

    fn latest_items(&self) -> BTreeMap<String, &StoredBenchmarkItem> {
        let mut latest = BTreeMap::<String, &StoredBenchmarkItem>::new();
        for item in self.items.values() {
            let logical_key = Self::logical_key(item);
            if latest
                .get(&logical_key)
                .map_or(true, |old| item.saved_at >= old.saved_at)
            {
                latest.insert(logical_key, item);
            }
        }
        latest
    }

    fn persist_index(&self) -> Result<(), String> {
        let Some(dir) = &self.data_dir else {
            return Ok(());
        };
        let items = self
            .latest_items()
            .values()
            .map(|item| {
                (
                    item.key.clone(),
                    StoredBenchmarkIndexEntry {
                        key: item.key.clone(),
                        saved_at: item.saved_at,
                        model: item.model.clone(),
                        model_name: item.model_name.clone(),
                        model_sha256: item.model_sha256.clone(),
                        configuration: item.configuration.clone(),
                        mode: item.mode,
                        config: item.config.clone(),
                        corpus_id: item.corpus_id.clone(),
                        corpus_sha256: item.corpus_sha256.clone(),
                        dataset_id: item.dataset_id.clone(),
                        dataset_name: item.dataset_name.clone(),
                        dataset_version: item.dataset_version,
                        dataset_sha256: item.dataset_sha256.clone(),
                        rime_snapshot_sha256: item.rime_snapshot_sha256.clone(),
                    },
                )
            })
            .collect();
        let bytes = serde_json::to_vec_pretty(&StoredBenchmarkIndex {
            version: BENCHMARK_ALGORITHM_VERSION,
            items,
        })
        .map_err(|error| error.to_string())?;
        persistence::atomic_write_file(&dir.join("benchmark"), "index.json", bytes)
            .map_err(|error| error.to_string())
    }

    fn error_page(&self, key: &str, page: u32, page_size: u32) -> Option<BenchmarkErrorPage> {
        let item = self.get(key)?;
        let page = page.max(1);
        let page_size = page_size.clamp(1, 500) as usize;
        let start = (page as usize - 1).saturating_mul(page_size);
        let mut total = 0u32;
        let mut items = Vec::with_capacity(page_size);
        for observation in item
            .report
            .observations
            .iter()
            .filter(|observation| !observation.correct)
        {
            if total >= start as u32 && items.len() < page_size {
                items.push(observation.clone());
            }
            total = total.saturating_add(1);
        }
        Some(BenchmarkErrorPage {
            key: key.to_owned(),
            items,
            page,
            page_size: page_size as u32,
            total,
        })
    }

    fn persisted_results(&self, dataset: &DatasetInfo) -> Vec<BenchmarkResult> {
        let mut rows = BTreeMap::<String, BenchmarkResult>::new();
        let current_corpus_hashes = dataset
            .corpora
            .iter()
            .map(|corpus| (corpus.id.as_str(), corpus.sha256.as_str()))
            .collect::<BTreeMap<_, _>>();
        let mut latest = BTreeMap::<String, &StoredBenchmarkItem>::new();
        for item in self.items.values().filter(|item| {
            item.dataset_id == dataset.id
                && !dataset.sha256.is_empty()
                && item.dataset_sha256 == dataset.sha256
                && current_corpus_hashes
                    .get(item.corpus_id.as_str())
                    .is_some_and(|sha256| !sha256.is_empty() && *sha256 == item.corpus_sha256)
        }) {
            let logical_key = Self::logical_key(item);
            if latest
                .get(&logical_key)
                .map_or(true, |old| item.saved_at >= old.saved_at)
            {
                latest.insert(logical_key, item);
            }
        }
        for item in latest.values() {
            let row_key = format!(
                "{}\u{1f}{}\u{1f}{:?}\u{1f}{}\u{1f}{}",
                serde_json::to_string(&item.model).unwrap_or_default(),
                serde_json::to_string(&item.configuration).unwrap_or_default(),
                item.mode,
                item.config.rime_schema,
                serde_json::to_string(&item.config).unwrap_or_default()
            );
            let id = format!("persisted-{}", rows.len() + 1);
            let row = rows.entry(row_key).or_insert_with(|| BenchmarkResult {
                id,
                model: item.model.clone(),
                model_name: item.model_name.clone(),
                model_sha256: item.model_sha256.clone(),
                configuration: item.configuration.clone(),
                mode: item.mode,
                config: item.config.clone(),
                status: BenchmarkRunStatus::Completed,
                cells: Vec::new(),
                report: None,
                error: None,
            });
            let summary = overall_summary(&item.report);
            row.cells.push(BenchmarkResultCell {
                key: item.key.clone(),
                corpus_id: item.corpus_id.clone(),
                status: BenchmarkCellStatus::Completed,
                total: summary.total as u32,
                completed: summary.completed as u32,
                correct: summary.correct as u32,
                no_prediction: summary.no_prediction as u32,
                errors: summary.total.saturating_sub(summary.correct) as u32,
                accuracy: summary.accuracy,
                error: None,
            });
        }
        rows.into_values().collect()
    }
}

pub(crate) struct BenchmarkControl {
    pub state: BenchmarkRunState,
    pub cancel: Option<Arc<AtomicBool>>,
    store: BenchmarkStore,
    corpus: Option<Arc<Corpus>>,
    pinyin_dictionary: Arc<PinyinDictionary>,
}

impl BenchmarkControl {
    pub fn new(data_dir: Option<&Path>, rime_dir: Option<&Path>) -> Self {
        let directory = default_corpus_dir();
        let pinyin_dictionary = Arc::new(PinyinDictionary::from_rime_dir(rime_dir));
        let (dataset, corpus) =
            match load_corpus_with_pinyin_dictionary(&directory, &pinyin_dictionary) {
                Ok(corpus) => (corpus.info().clone(), Some(Arc::new(corpus))),
                Err(error) => (
                    DatasetInfo {
                        id: "lime-user-corpus".into(),
                        name: "用户语料".into(),
                        version: 1,
                        directory: directory.display().to_string(),
                        error: Some(error),
                        sha256: String::new(),
                        corpora: Vec::new(),
                    },
                    None,
                ),
            };
        let store = BenchmarkStore::new(data_dir);
        let mut state = BenchmarkRunState::idle(&dataset);
        state.results = store.persisted_results(&dataset);
        Self {
            state,
            cancel: None,
            store,
            corpus,
            pinyin_dictionary,
        }
    }
}

#[derive(Clone)]
struct PlannedRun {
    result: BenchmarkResult,
    model_path: Option<PathBuf>,
    model_fingerprint: Option<String>,
    /// Index of this row in the live run state.  Normal batches use the
    /// planned order; a single-cell rerun points back at the preserved row.
    state_index: usize,
}

fn unique<T: Eq + std::hash::Hash + Clone>(items: Vec<T>) -> Vec<T> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert(item.clone()))
        .collect()
}

fn selected_corpora(requested: &[String], dataset: &DatasetInfo) -> Result<Vec<String>, String> {
    let available = dataset
        .corpora
        .iter()
        .map(|corpus| corpus.id.clone())
        .collect::<HashSet<_>>();
    let mut values = unique(requested.to_vec());
    if values.is_empty() {
        return Err("请选择至少一个语料".into());
    }
    if let Some(unknown) = values.iter().find(|value| !available.contains(*value)) {
        return Err(format!("语料不存在：{unknown}"));
    }
    values.sort();
    Ok(values)
}

fn plan_runs(
    request: BenchmarkRunRequest,
    presets: &[ModelPreset],
    base: &Config,
    dataset: &DatasetInfo,
) -> Result<Vec<PlannedRun>, String> {
    if request.models.is_empty() || request.modes.is_empty() {
        return Err("请选择模型和拼音方式".into());
    }
    if base.rime_schema != "rime_ice" {
        return Err("评测需要 rime_ice 全拼方案".into());
    }
    let models = unique(request.models);
    let modes = unique(request.modes);
    let configurations = unique(request.configurations);
    let has_llm = models
        .iter()
        .any(|model| matches!(model, BenchmarkModelSelection::Preset { .. }));
    if has_llm && configurations.is_empty() {
        return Err("请选择至少一种模型配置".into());
    }
    let count = models
        .iter()
        .map(|model| {
            if matches!(model, BenchmarkModelSelection::RimeOnly) {
                1
            } else {
                configurations.len()
            }
        })
        .sum::<usize>()
        .saturating_mul(modes.len());
    if count > MAX_BATCH_RUNS {
        return Err("一次最多评测 32 种组合".into());
    }
    let corpus_ids = selected_corpora(&request.corpora, dataset)?;
    let mut runs = Vec::new();
    for model in models {
        let (name, path, fingerprint) = match &model {
            BenchmarkModelSelection::RimeOnly => ("仅 Rime".to_owned(), None, None),
            BenchmarkModelSelection::Preset { name } => {
                let preset = presets
                    .iter()
                    .find(|preset| preset.name == *name)
                    .ok_or_else(|| format!("模型预设不存在：{name}"))?;
                (
                    name.clone(),
                    Some(PathBuf::from(&preset.path)),
                    preset.sha256.clone(),
                )
            }
        };
        let model_configs = if matches!(model, BenchmarkModelSelection::RimeOnly) {
            vec![BenchmarkConfiguration {
                llm_rerank_count: 1,
                preceding_text_char_limit: 1,
            }]
        } else {
            configurations.clone()
        };
        for configuration in &model_configs {
            let mut config = if matches!(model, BenchmarkModelSelection::RimeOnly) {
                Config::default()
            } else {
                base.clone()
            };
            config.llm_rerank_count = configuration.llm_rerank_count;
            config.preceding_text_char_limit = configuration.preceding_text_char_limit;
            config.page_size = 1;
            config.llm_effective_count = 1;
            config.context_preview_char_limit = 0;
            crate::config::validate(&config).map_err(|error| error.to_string())?;
            for mode in &modes {
                let cells = corpus_ids
                    .iter()
                    .map(|corpus_id| BenchmarkResultCell {
                        key: String::new(),
                        corpus_id: corpus_id.clone(),
                        status: BenchmarkCellStatus::Pending,
                        total: dataset
                            .corpora
                            .iter()
                            .find(|corpus| corpus.id == *corpus_id)
                            .map(|corpus| corpus.cases as u32)
                            .unwrap_or(0),
                        completed: 0,
                        correct: 0,
                        no_prediction: 0,
                        errors: 0,
                        accuracy: None,
                        error: None,
                    })
                    .collect();
                runs.push(PlannedRun {
                    model_path: path.clone(),
                    model_fingerprint: fingerprint.clone(),
                    state_index: 0,
                    result: BenchmarkResult {
                        id: format!("run-{}", runs.len() + 1),
                        model: model.clone(),
                        model_name: name.clone(),
                        model_sha256: None,
                        configuration: configuration.clone(),
                        mode: *mode,
                        config: config.clone(),
                        status: BenchmarkRunStatus::Idle,
                        cells,
                        report: None,
                        error: None,
                    },
                });
            }
        }
    }
    Ok(runs)
}

fn overall_summary(report: &Report) -> Summary {
    report
        .summaries
        .iter()
        .find(|summary| summary.category.is_none())
        .cloned()
        .unwrap_or(Summary {
            category: None,
            mode: report.modes.first().copied().unwrap_or(InputMode::Full),
            total: 0,
            completed: 0,
            correct: 0,
            no_prediction: 0,
            errors: 0,
            accuracy: None,
        })
}

fn corpus_dataset(dataset: &DatasetInfo, corpus_id: &str) -> DatasetInfo {
    DatasetInfo {
        id: dataset.id.clone(),
        name: dataset.name.clone(),
        version: dataset.version,
        directory: dataset.directory.clone(),
        error: dataset.error.clone(),
        sha256: dataset.sha256.clone(),
        corpora: dataset
            .corpora
            .iter()
            .filter(|corpus| corpus.id == corpus_id)
            .cloned()
            .collect(),
    }
}

fn visit_selected_cases(
    corpus: &Corpus,
    corpus_ids: &HashSet<String>,
    context_limit: usize,
    visitor: impl FnMut(Case) -> Result<(), String>,
) -> Result<(), String> {
    let categories = corpus_ids.iter().cloned().collect::<Vec<_>>();
    corpus.visit_cases(context_limit, Some(&categories), visitor)
}

impl CoreService {
    pub(super) fn benchmark_dataset(&self) -> BenchmarkDataset {
        let mut control = self.benchmark.lock().expect("benchmark mutex poisoned");
        if matches!(
            control.state.status,
            BenchmarkRunStatus::Running | BenchmarkRunStatus::Stopping
        ) {
            return control
                .corpus
                .as_ref()
                .map(|corpus| corpus.info().clone())
                .unwrap_or_else(|| DatasetInfo {
                    id: control.state.dataset_id.clone(),
                    name: control.state.dataset_name.clone(),
                    version: control.state.dataset_version,
                    directory: default_corpus_dir().display().to_string(),
                    error: Some("无法加载用户语料".into()),
                    sha256: String::new(),
                    corpora: Vec::new(),
                });
        }
        let directory = default_corpus_dir();
        let pinyin_dictionary = Arc::clone(&control.pinyin_dictionary);
        let (dataset, corpus) =
            match load_corpus_with_pinyin_dictionary(&directory, &pinyin_dictionary) {
                Ok(corpus) => (corpus.info().clone(), Some(Arc::new(corpus))),
                Err(error) => (
                    DatasetInfo {
                        id: "lime-user-corpus".into(),
                        name: "用户语料".into(),
                        version: 1,
                        directory: directory.display().to_string(),
                        error: Some(error),
                        sha256: String::new(),
                        corpora: Vec::new(),
                    },
                    None,
                ),
            };
        control.corpus = corpus;
        control.state.dataset_id = dataset.id.clone();
        control.state.dataset_name = dataset.name.clone();
        control.state.dataset_version = dataset.version;
        control.state.results = control.store.persisted_results(&dataset);
        control.state.total = 0;
        control.state.completed = 0;
        control.state.current = None;
        control.state.queue.clear();
        control.state.rime_snapshot_sha256 = None;
        control.state.error = None;
        control.state.status = BenchmarkRunStatus::Idle;
        dataset
    }

    pub(super) fn benchmark_status(&self) -> BenchmarkRunState {
        self.benchmark
            .lock()
            .expect("benchmark mutex poisoned")
            .state
            .clone()
    }

    pub(super) fn benchmark_errors(&self, key: &str, page: u32, page_size: u32) -> Response {
        let control = self.benchmark.lock().expect("benchmark mutex poisoned");
        match control.store.error_page(key, page, page_size) {
            Some(page) => Response::BenchmarkErrors(page),
            None => Response::Error {
                code: ErrorCode::InvalidRequest,
            },
        }
    }

    pub(super) fn clear_benchmark_results(&self) -> Response {
        let mut control = self.benchmark.lock().expect("benchmark mutex poisoned");
        if matches!(
            control.state.status,
            BenchmarkRunStatus::Running | BenchmarkRunStatus::Stopping
        ) {
            return Response::Error {
                code: ErrorCode::InvalidRequest,
            };
        }
        if control.store.clear().is_err() {
            return Response::Error {
                code: ErrorCode::Internal,
            };
        }
        control.state.results.clear();
        control.state.completed = 0;
        control.state.total = 0;
        control.state.current = None;
        control.state.queue.clear();
        control.state.rime_snapshot_sha256 = None;
        control.state.error = None;
        Response::Accepted
    }

    pub(super) fn rerun_benchmark_item(&self, key: &str) -> Response {
        let (item, previous_state) = {
            let control = self.benchmark.lock().expect("benchmark mutex poisoned");
            if matches!(
                control.state.status,
                BenchmarkRunStatus::Running | BenchmarkRunStatus::Stopping
            ) {
                return Response::Error {
                    code: ErrorCode::InvalidRequest,
                };
            }
            (control.store.get(key).cloned(), control.state.clone())
        };
        let Some(item) = item else {
            return Response::Error {
                code: ErrorCode::InvalidRequest,
            };
        };
        self.start_benchmark_internal(
            BenchmarkRunRequest {
                modes: vec![item.mode],
                models: vec![item.model.clone()],
                configurations: vec![item.configuration.clone()],
                corpora: vec![item.corpus_id.clone()],
            },
            Some((previous_state, item)),
        )
    }

    pub(super) fn start_benchmark(&self, request: BenchmarkRunRequest) -> Response {
        self.start_benchmark_internal(request, None)
    }

    fn start_benchmark_internal(
        &self,
        request: BenchmarkRunRequest,
        preserved: Option<(BenchmarkRunState, StoredBenchmarkItem)>,
    ) -> Response {
        let snapshot = self.config_snapshot();
        let dataset = self.benchmark_dataset();
        let corpus = self
            .benchmark
            .lock()
            .expect("benchmark mutex poisoned")
            .corpus
            .clone();
        let Some(corpus) = corpus else {
            return Response::Error {
                code: ErrorCode::InvalidRequest,
            };
        };
        let config = preserved
            .as_ref()
            .map(|(_, item)| &item.config)
            .unwrap_or(&snapshot.config);
        let removed_key = preserved.as_ref().map(|(_, item)| item.key.clone());
        let mut runs = match plan_runs(request, &self.model_presets(), config, &dataset) {
            Ok(runs) => runs,
            Err(_) => {
                return Response::Error {
                    code: ErrorCode::InvalidRequest,
                }
            }
        };
        for (index, run) in runs.iter_mut().enumerate() {
            run.state_index = index;
        }

        let mut state = preserved
            .as_ref()
            .map(|(state, _)| state.clone())
            .unwrap_or_else(|| BenchmarkRunState::idle(&dataset));
        state.dataset_id = dataset.id.clone();
        state.dataset_name = dataset.name.clone();
        state.dataset_version = dataset.version;
        state.status = BenchmarkRunStatus::Running;
        state.config_revision = snapshot.revision;
        state.rime_snapshot_sha256 = None;
        state.current = None;
        state.error = None;

        if let Some((previous, item)) = preserved {
            let run = runs
                .first_mut()
                .expect("single-item rerun always plans one row");
            run.result.config = item.config.clone();
            run.result.configuration = item.configuration.clone();
            let mut target_cell = run
                .result
                .cells
                .iter()
                .find(|cell| cell.corpus_id == item.corpus_id)
                .cloned()
                .expect("single-item rerun always plans one corpus cell");
            target_cell.status = BenchmarkCellStatus::Pending;
            target_cell.key.clear();
            target_cell.completed = 0;
            target_cell.correct = 0;
            target_cell.no_prediction = 0;
            target_cell.errors = 0;
            target_cell.accuracy = None;
            target_cell.error = None;
            let row_index = previous.results.iter().position(|row| {
                row.model == run.result.model
                    && row.configuration == run.result.configuration
                    && row.mode == run.result.mode
                    && row.config == run.result.config
            });
            let row_index = row_index.unwrap_or_else(|| state.results.len());
            if row_index == state.results.len() {
                let mut row = run.result.clone();
                row.cells = vec![target_cell.clone()];
                state.results.push(row);
            } else {
                let row = state
                    .results
                    .get_mut(row_index)
                    .expect("row index checked above");
                row.status = BenchmarkRunStatus::Idle;
                row.error = None;
                if let Some(cell) = row
                    .cells
                    .iter_mut()
                    .find(|cell| cell.corpus_id == target_cell.corpus_id)
                {
                    *cell = target_cell.clone();
                } else {
                    row.cells.push(target_cell.clone());
                }
            }
            // The worker receives the complete preserved row.  Completed and
            // failed cells remain visible and are skipped; only the target
            // cell is pending and will be executed.
            run.state_index = row_index;
            run.result = state
                .results
                .get(row_index)
                .cloned()
                .expect("rerun row exists");
        } else {
            state.results = runs.iter().map(|run| run.result.clone()).collect();
        }
        state.total = state
            .results
            .iter()
            .flat_map(|run| run.cells.iter())
            .map(|cell| cell.total)
            .sum();
        state.completed = state
            .results
            .iter()
            .flat_map(|run| run.cells.iter())
            .map(|cell| cell.completed)
            .sum();
        state.queue = state
            .results
            .iter()
            .flat_map(|run| {
                run.cells.iter().map(|cell| BenchmarkQueueItem {
                    key: cell.key.clone(),
                    model_name: run.model_name.clone(),
                    corpus_id: cell.corpus_id.clone(),
                    status: cell.status,
                })
            })
            .collect();
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
            if let Some(key) = removed_key {
                if control.store.remove(&key).is_err() {
                    return Response::Error {
                        code: ErrorCode::Internal,
                    };
                }
            }
            control.state = state.clone();
            control.cancel = Some(Arc::clone(&cancel));
        }
        let service = self.clone();
        std::thread::spawn(move || service.run_benchmark_batch(corpus, dataset, runs, cancel));
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

    fn capture_benchmark_candidates(
        &self,
        corpus: &Corpus,
        runs: &[PlannedRun],
        cancel: &AtomicBool,
    ) -> Result<(CandidateSnapshot, String), String> {
        let max_candidates = runs
            .iter()
            .map(|run| run.result.configuration.llm_rerank_count)
            .max()
            .unwrap_or(1) as usize;
        let modes = unique(runs.iter().map(|run| run.result.mode).collect());
        let categories = runs
            .iter()
            .flat_map(|run| {
                run.result
                    .cells
                    .iter()
                    .filter(|cell| cell.status == BenchmarkCellStatus::Pending)
                    .map(|cell| cell.corpus_id.clone())
            })
            .collect::<HashSet<_>>();
        let revision = {
            let engine = self.engine.lock().map_err(|_| "Rime 状态不可用")?;
            if !engine.is_available() || engine.active_schema() != Some("rime_ice") {
                return Err("Rime 不可用或未启用 rime_ice 方案".into());
            }
            engine.revision()
        };
        let limit = runs
            .iter()
            .map(|run| run.result.config.preceding_text_char_limit)
            .max()
            .unwrap_or(1) as usize;
        let mut candidates = BTreeMap::new();
        visit_selected_cases(corpus, &categories, limit, |case| {
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
                    return Err("生成评测候选时 Rime 方案或词库发生变化，请重新开始".into());
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
            return Err("生成评测候选时 Rime 方案或词库发生变化，请重新开始".into());
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
        corpus: Arc<Corpus>,
        dataset: DatasetInfo,
        mut runs: Vec<PlannedRun>,
        cancel: Arc<AtomicBool>,
    ) {
        let (candidates, rime_fingerprint) =
            match self.capture_benchmark_candidates(&corpus, &runs, &cancel) {
                Ok(value) => value,
                Err(error) => {
                    self.finish_benchmark_batch(&cancel, Some(error));
                    return;
                }
            };
        let dataset_sha256 = dataset.sha256.clone();
        {
            self.benchmark
                .lock()
                .expect("benchmark mutex poisoned")
                .state
                .rime_snapshot_sha256 = Some(rime_fingerprint.clone());
        }
        let mut model_fingerprints = BTreeMap::<PathBuf, Result<String, String>>::new();
        for run in &mut runs {
            if let (BenchmarkModelSelection::Preset { .. }, Some(path)) =
                (&run.result.model, &run.model_path)
            {
                let fingerprint = model_fingerprints.entry(path.clone()).or_insert_with(|| {
                    LlamaRuntime::inspect_gguf(path.clone()).map(|metadata| metadata.sha256)
                });
                match fingerprint {
                    Ok(value) => {
                        run.model_fingerprint = Some(value.clone());
                        run.result.model_sha256 = Some(value.clone());
                    }
                    Err(error) => {
                        run.model_fingerprint = None;
                        run.result.status = BenchmarkRunStatus::Failed;
                        run.result.error = Some(error.clone());
                        for cell in &mut run.result.cells {
                            if cell.status == BenchmarkCellStatus::Pending {
                                cell.status = BenchmarkCellStatus::Failed;
                                cell.error = Some(error.clone());
                            }
                        }
                    }
                }
            }
        }
        for run in &mut runs {
            for cell in &mut run.result.cells {
                if cell.status != BenchmarkCellStatus::Pending {
                    continue;
                }
                let corpus_sha256 = dataset
                    .corpora
                    .iter()
                    .find(|corpus| corpus.id == cell.corpus_id)
                    .map(|corpus| corpus.sha256.clone())
                    .unwrap_or_default();
                let model_fingerprint = run.model_fingerprint.clone().unwrap_or_else(|| {
                    if matches!(run.result.model, BenchmarkModelSelection::RimeOnly) {
                        "rime-only".into()
                    } else {
                        format!("uncacheable:{}", run.result.id)
                    }
                });
                cell.key = CellIdentity {
                    model_fingerprint,
                    effective_config: run.result.config.clone(),
                    mode: run.result.mode,
                    corpus_id: cell.corpus_id.clone(),
                    corpus_sha256,
                }
                .key();
                let cache_allowed = run.model_fingerprint.is_some()
                    || matches!(run.result.model, BenchmarkModelSelection::RimeOnly);
                let stored = cache_allowed
                    .then(|| {
                        self.benchmark
                            .lock()
                            .expect("benchmark mutex poisoned")
                            .store
                            .get(&cell.key)
                            .cloned()
                    })
                    .flatten();
                if let Some(item) = stored {
                    let summary = overall_summary(&item.report);
                    cell.status = BenchmarkCellStatus::Completed;
                    cell.completed = summary.completed as u32;
                    cell.correct = summary.correct as u32;
                    cell.no_prediction = summary.no_prediction as u32;
                    cell.errors = summary.total.saturating_sub(summary.correct) as u32;
                    cell.accuracy = summary.accuracy;
                }
            }
        }
        {
            let mut control = self.benchmark.lock().expect("benchmark mutex poisoned");
            for run in &runs {
                if let Some(row) = control.state.results.get_mut(run.state_index) {
                    row.cells = run.result.cells.clone();
                    row.model_sha256 = run.result.model_sha256.clone();
                    row.error = run.result.error.clone();
                    row.status = if run.result.status == BenchmarkRunStatus::Failed {
                        BenchmarkRunStatus::Failed
                    } else if run
                        .result
                        .cells
                        .iter()
                        .all(|cell| cell.status == BenchmarkCellStatus::Completed)
                    {
                        BenchmarkRunStatus::Completed
                    } else {
                        BenchmarkRunStatus::Idle
                    };
                }
                for cell in &run.result.cells {
                    let Some(cell_index) = run
                        .result
                        .cells
                        .iter()
                        .position(|candidate| candidate.corpus_id == cell.corpus_id)
                    else {
                        continue;
                    };
                    let queue_index = control
                        .state
                        .results
                        .iter()
                        .take(run.state_index)
                        .map(|row| row.cells.len())
                        .sum::<usize>()
                        .saturating_add(cell_index);
                    if let Some(item) = control.state.queue.get_mut(queue_index) {
                        item.key = cell.key.clone();
                        item.status = cell.status;
                    }
                }
            }
            control.state.completed = control
                .state
                .results
                .iter()
                .flat_map(|row| row.cells.iter())
                .map(|cell| cell.completed)
                .sum();
        }
        let mut runtime: Option<LlamaRuntime> = None;
        let mut runtime_key: Option<(PathBuf, u32, u32, String)> = None;
        for run in &runs {
            if cancel.load(Ordering::Acquire) {
                break;
            }
            for (cell_index, planned_cell) in run.result.cells.iter().enumerate() {
                if planned_cell.status != BenchmarkCellStatus::Pending
                    || cancel.load(Ordering::Acquire)
                {
                    continue;
                }
                let corpus_id = planned_cell.corpus_id.clone();
                let key = planned_cell.key.clone();
                self.set_cell_running(run.state_index, cell_index, &key, planned_cell.total);
                let model = match (&run.result.model, &run.model_path) {
                    (BenchmarkModelSelection::RimeOnly, _) => None,
                    (_, Some(path)) => {
                        let runtime_key_value = (
                            path.clone(),
                            run.result.config.llm_context_token_limit,
                            run.result.config.llm_rerank_count,
                            run.result.config.llm_backend.clone(),
                        );
                        if runtime_key.as_ref() != Some(&runtime_key_value) {
                            runtime_key = None;
                            let loaded = {
                                let _guard =
                                    self.model_load.lock().expect("model load mutex poisoned");
                                runtime = None;
                                LlamaRuntime::load_with_backend_preference_and_sequence_count(
                                    path.clone(),
                                    run.result.config.llm_context_token_limit as usize,
                                    backend_preference_for(&run.result.config.llm_backend),
                                    run.result.config.llm_rerank_count as usize,
                                )
                            };
                            match loaded {
                                Ok(value) => {
                                    runtime = Some(value);
                                    runtime_key = Some(runtime_key_value);
                                }
                                Err(error) => {
                                    self.set_cell_failed(run.state_index, cell_index, &key, error);
                                    continue;
                                }
                            }
                        }
                        runtime.as_ref()
                    }
                    _ => None,
                };
                if let Some(model) = model {
                    if run.model_fingerprint.as_deref() != Some(model.sha256.as_str()) {
                        self.set_cell_failed(
                            run.state_index,
                            cell_index,
                            &key,
                            "加载模型时文件内容发生变化，请重新开始评测".into(),
                        );
                        continue;
                    }
                }
                let corpus_info = corpus_dataset(&dataset, &corpus_id);
                let corpus_sha256 = corpus_info
                    .corpora
                    .first()
                    .map(|corpus| corpus.sha256.clone())
                    .unwrap_or_default();
                let mut accumulator =
                    BenchmarkAccumulator::new(&corpus_info, &dataset_sha256, run.result.mode);
                let ids = HashSet::from([corpus_id.clone()]);
                let cell_started = Instant::now();
                let evaluation = visit_selected_cases(
                    &corpus,
                    &ids,
                    run.result.config.preceding_text_char_limit as usize,
                    |case| {
                        if cancel.load(Ordering::Acquire) {
                            return Err(CANCELLED.into());
                        }
                        let input = preedit(&case, run.result.mode);
                        let batch = candidates.get(&input).ok_or("评测候选快照缺少目标拼音")?;
                        let started = Instant::now();
                        let prediction = if case.first_word || model.is_none() {
                            Prediction {
                                top1: batch
                                    .candidates
                                    .first()
                                    .map(|candidate| candidate.commit_text.clone()),
                                error: None,
                                elapsed_ms: Some(0),
                            }
                        } else {
                            match try_rerank_selected_candidates_with_preedit_and_limit(
                                &batch.candidates,
                                &batch.complete_candidate_indices,
                                &input,
                                &case.context,
                                model,
                                RerankOptions {
                                    rerank_count: run.result.config.llm_rerank_count as usize,
                                    effective_count: 1,
                                    inference_count_limit: run
                                        .result
                                        .config
                                        .llm_inference_count_limit
                                        as usize,
                                    ignore_emoji: run.result.config.llm_ignore_emoji,
                                },
                            ) {
                                Ok(ranking) => Prediction {
                                    top1: ranking
                                        .result
                                        .candidates
                                        .first()
                                        .map(|candidate| candidate.commit_text.clone()),
                                    error: None,
                                    elapsed_ms: Some(elapsed_ms(started.elapsed())),
                                },
                                Err(error) => Prediction {
                                    top1: None,
                                    error: Some(error),
                                    elapsed_ms: Some(elapsed_ms(started.elapsed())),
                                },
                            }
                        };
                        accumulator.observe(&case, prediction)?;
                        self.update_benchmark_progress(
                            &key,
                            &run.result.model_name,
                            &corpus_id,
                            cell_started,
                            planned_cell.total,
                        );
                        Ok(())
                    },
                );
                let stopped = cancel.load(Ordering::Acquire);
                let complete = !stopped && evaluation.is_ok();
                let report = accumulator.finish(complete);
                if report.complete {
                    let model_sha256 = model.map(|runtime| runtime.sha256.clone());
                    let item = StoredBenchmarkItem {
                        version: BENCHMARK_ALGORITHM_VERSION,
                        saved_at: 0,
                        key: key.clone(),
                        model: run.result.model.clone(),
                        model_name: run.result.model_name.clone(),
                        model_sha256,
                        configuration: run.result.configuration.clone(),
                        mode: run.result.mode,
                        config: run.result.config.clone(),
                        corpus_id: corpus_id.clone(),
                        corpus_sha256,
                        dataset_id: dataset.id.clone(),
                        dataset_name: dataset.name.clone(),
                        dataset_version: dataset.version,
                        dataset_sha256: dataset_sha256.clone(),
                        rime_snapshot_sha256: rime_fingerprint.clone(),
                        report: report.clone(),
                    };
                    if let Err(error) = self
                        .benchmark
                        .lock()
                        .expect("benchmark mutex poisoned")
                        .store
                        .save(item)
                    {
                        self.set_cell_failed(run.state_index, cell_index, &key, error);
                        continue;
                    }
                }
                let status = if stopped {
                    BenchmarkCellStatus::Cancelled
                } else if evaluation.is_err() {
                    BenchmarkCellStatus::Failed
                } else {
                    BenchmarkCellStatus::Completed
                };
                self.publish_cell_result(
                    run.state_index,
                    cell_index,
                    &key,
                    status,
                    &report,
                    evaluation.err().filter(|_| !stopped),
                );
            }
        }
        {
            let _guard = self.model_load.lock().expect("model load mutex poisoned");
            drop(runtime);
        }
        self.finish_benchmark_batch(&cancel, None);
    }

    fn set_cell_running(&self, run_index: usize, cell_index: usize, key: &str, total: u32) {
        let mut control = self.benchmark.lock().expect("benchmark mutex poisoned");
        if let Some(row) = control.state.results.get_mut(run_index) {
            row.status = BenchmarkRunStatus::Running;
            if let Some(cell) = row.cells.get_mut(cell_index) {
                cell.status = BenchmarkCellStatus::Running;
            }
        }
        if let Some(item) = control.state.queue.iter_mut().find(|item| item.key == key) {
            item.status = BenchmarkCellStatus::Running;
        }
        if let Some(row) = control.state.results.get(run_index) {
            if let Some(cell) = row.cells.get(cell_index) {
                control.state.current = Some(BenchmarkProgress {
                    key: key.to_owned(),
                    model_name: row.model_name.clone(),
                    corpus_id: cell.corpus_id.clone(),
                    completed: cell.completed,
                    total,
                    rate_per_second: None,
                    eta_seconds: None,
                });
            }
        }
    }

    fn set_cell_failed(&self, run_index: usize, cell_index: usize, key: &str, error: String) {
        let mut control = self.benchmark.lock().expect("benchmark mutex poisoned");
        if let Some(row) = control.state.results.get_mut(run_index) {
            row.status = BenchmarkRunStatus::Failed;
            row.error = Some(error.clone());
            if let Some(cell) = row.cells.get_mut(cell_index) {
                cell.status = BenchmarkCellStatus::Failed;
                cell.error = Some(error.clone());
            }
        }
        if let Some(item) = control.state.queue.iter_mut().find(|item| item.key == key) {
            item.status = BenchmarkCellStatus::Failed;
        }
    }

    fn publish_cell_result(
        &self,
        run_index: usize,
        cell_index: usize,
        key: &str,
        status: BenchmarkCellStatus,
        report: &Report,
        error: Option<String>,
    ) {
        let summary = overall_summary(report);
        let mut control = self.benchmark.lock().expect("benchmark mutex poisoned");
        if let Some(row) = control.state.results.get_mut(run_index) {
            if let Some(cell) = row.cells.get_mut(cell_index) {
                cell.status = status;
                cell.completed = summary.completed as u32;
                cell.correct = summary.correct as u32;
                cell.no_prediction = summary.no_prediction as u32;
                cell.errors = summary.total.saturating_sub(summary.correct) as u32;
                cell.accuracy = summary.accuracy;
                cell.error = error.clone();
            }
            row.status = if row
                .cells
                .iter()
                .all(|cell| cell.status == BenchmarkCellStatus::Completed)
            {
                BenchmarkRunStatus::Completed
            } else if row
                .cells
                .iter()
                .any(|cell| cell.status == BenchmarkCellStatus::Failed)
            {
                BenchmarkRunStatus::Failed
            } else {
                BenchmarkRunStatus::Running
            };
        }
        if let Some(item) = control.state.queue.iter_mut().find(|item| item.key == key) {
            item.status = status;
        }
    }

    fn update_benchmark_progress(
        &self,
        key: &str,
        model_name: &str,
        corpus_id: &str,
        started: Instant,
        total: u32,
    ) {
        let mut control = self.benchmark.lock().expect("benchmark mutex poisoned");
        control.state.completed = control.state.completed.saturating_add(1);
        let completed = control
            .state
            .results
            .iter_mut()
            .flat_map(|row| row.cells.iter_mut())
            .find(|cell| cell.key == key)
            .map(|cell| {
                cell.completed = cell.completed.saturating_add(1);
                cell.completed
            })
            .unwrap_or(1);
        let elapsed = started.elapsed().as_secs_f64();
        let rate = (elapsed > 0.0).then_some(completed as f64 / elapsed);
        let eta = rate.and_then(|rate| {
            let seconds = total.saturating_sub(completed) as f64 / rate;
            seconds.is_finite().then_some(seconds.ceil() as u64)
        });
        control.state.current = Some(BenchmarkProgress {
            key: key.to_owned(),
            model_name: model_name.to_owned(),
            corpus_id: corpus_id.to_owned(),
            completed,
            total,
            rate_per_second: rate,
            eta_seconds: eta,
        });
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
                for cell in &mut row.cells {
                    if matches!(
                        cell.status,
                        BenchmarkCellStatus::Pending | BenchmarkCellStatus::Running
                    ) {
                        cell.status = if stopped {
                            BenchmarkCellStatus::Cancelled
                        } else {
                            BenchmarkCellStatus::Failed
                        };
                    }
                }
            }
        }
        for item in &mut control.state.queue {
            if matches!(
                item.status,
                BenchmarkCellStatus::Pending | BenchmarkCellStatus::Running
            ) {
                item.status = if stopped {
                    BenchmarkCellStatus::Cancelled
                } else {
                    BenchmarkCellStatus::Failed
                };
            }
        }
        control.state.status = status;
        control.state.current = None;
        control.state.error = if stopped { None } else { error };
        control.cancel = None;
    }
}

struct BenchmarkAccumulator {
    report: Report,
    seen: HashSet<String>,
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
            self.report.observations.push(observation);
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
    use lime_benchmark::{build_report, Dataset};
    use std::time::Duration;

    fn preset(name: &str) -> ModelPreset {
        ModelPreset {
            name: name.into(),
            path: format!("{name}.gguf"),
            size_bytes: None,
            sha256: Some(format!("{name}-sha")),
            loaded: false,
        }
    }
    fn configuration(count: u32, context: u32) -> BenchmarkConfiguration {
        BenchmarkConfiguration {
            llm_rerank_count: count,
            preceding_text_char_limit: context,
        }
    }
    fn fixture_info() -> DatasetInfo {
        DatasetInfo {
            id: "fixture".into(),
            name: "Fixture".into(),
            version: 1,
            directory: "fixture".into(),
            error: None,
            sha256: "dataset".into(),
            corpora: vec![
                lime_benchmark::CorpusInfo {
                    id: "a".into(),
                    name: "A".into(),
                    characters: 4,
                    cases: 2,
                    sha256: "corpus-a".into(),
                    articles: 1,
                },
                lime_benchmark::CorpusInfo {
                    id: "b".into(),
                    name: "B".into(),
                    characters: 2,
                    cases: 1,
                    sha256: "corpus-b".into(),
                    articles: 1,
                },
            ],
        }
    }
    fn fixture_case(id: &str, category: &str, first_word: bool) -> Case {
        Case {
            id: id.into(),
            category: category.into(),
            context: if first_word {
                String::new()
            } else {
                "前".into()
            },
            expected: "文".into(),
            syllables: vec!["wen".into()],
            first_word,
        }
    }
    fn request(models: Vec<BenchmarkModelSelection>) -> BenchmarkRunRequest {
        BenchmarkRunRequest {
            modes: vec![InputMode::Full, InputMode::Initials, InputMode::Full],
            models,
            configurations: vec![
                configuration(1, 16),
                configuration(32, 128),
                configuration(1, 16),
            ],
            corpora: vec!["a".into(), "b".into(), "a".into()],
        }
    }

    #[test]
    fn matrix_deduplicates_and_rime_only_has_one_config() {
        let base = Config::default();
        let plan = plan_runs(
            request(vec![
                BenchmarkModelSelection::preset("first"),
                BenchmarkModelSelection::preset("first"),
            ]),
            &[preset("first")],
            &base,
            &fixture_info(),
        )
        .unwrap();
        assert_eq!(plan.len(), 4);
        assert_eq!(base, Config::default());
        assert!(plan.iter().all(|run| {
            run.result.config.page_size == 1
                && run.result.config.llm_effective_count == 1
                && run.result.config.context_preview_char_limit == 0
        }));
        assert!(plan.iter().all(|run| run.result.cells.len() == 2));
        let mut only = request(vec![BenchmarkModelSelection::RimeOnly]);
        only.configurations.clear();
        let plan = plan_runs(only, &[], &Config::default(), &fixture_info()).unwrap();
        assert_eq!(plan.len(), 2);
        assert!(plan
            .iter()
            .all(|run| run.result.configuration.llm_rerank_count == 1));
        assert!(plan.iter().all(|run| {
            run.result.configuration.preceding_text_char_limit == 1
                && run.result.config.preceding_text_char_limit == 1
        }));
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
            let mut value = request(vec![BenchmarkModelSelection::preset("first")]);
            value.configurations = configurations;
            assert!(plan_runs(value, &presets, &Config::default(), &fixture_info()).is_err());
        }
        let mut value = request(vec![BenchmarkModelSelection::preset("missing")]);
        assert!(plan_runs(value, &presets, &Config::default(), &fixture_info()).is_err());
        value = request(vec![BenchmarkModelSelection::preset("first")]);
        value.modes.clear();
        assert!(plan_runs(value, &presets, &Config::default(), &fixture_info()).is_err());
        value = request(Vec::new());
        assert!(plan_runs(value, &presets, &Config::default(), &fixture_info()).is_err());
        value = request(vec![BenchmarkModelSelection::preset("first")]);
        value.corpora.clear();
        assert!(plan_runs(value, &presets, &Config::default(), &fixture_info()).is_err());
        value = request(vec![
            BenchmarkModelSelection::preset("first"),
            BenchmarkModelSelection::preset("second"),
        ]);
        value.configurations = (1..=9).map(|count| configuration(count, 128)).collect();
        assert!(plan_runs(value, &presets, &Config::default(), &fixture_info()).is_err());
    }

    fn streaming_fixtures() -> (Dataset, DatasetInfo) {
        let cases = vec![
            fixture_case("a1", "a", true),
            fixture_case("a2", "a", false),
            fixture_case("b1", "b", true),
        ];
        let dataset = Dataset {
            id: "fixture".into(),
            name: "Fixture".into(),
            version: 1,
            cases,
        };
        let mut info = fixture_info();
        info.corpora[0].cases = 2;
        info.corpora[1].cases = 1;
        (dataset, info)
    }

    #[test]
    fn streaming_scores_match_full_report_and_weighted_total() {
        let (dataset, info) = streaming_fixtures();
        let mut streaming = BenchmarkAccumulator::new(&info, "fingerprint", InputMode::Full);
        let predictions = [
            Prediction {
                top1: Some("文".into()),
                error: None,
                elapsed_ms: None,
            },
            Prediction {
                top1: None,
                error: Some("runtime failure".into()),
                elapsed_ms: None,
            },
            Prediction {
                top1: Some("错".into()),
                error: None,
                elapsed_ms: None,
            },
        ];
        let mut observations = Vec::new();
        for (case, prediction) in dataset.cases.iter().zip(predictions) {
            observations.push(observe(case, InputMode::Full, prediction.clone()));
            streaming.observe(case, prediction).unwrap();
        }
        let expected = build_report(&dataset, &[InputMode::Full], observations).unwrap();
        let report = streaming.finish(true);
        assert!(report.complete);
        assert_eq!(report.summaries, expected.summaries);
        assert_eq!(overall_summary(&report).accuracy, Some(1.0 / 3.0));
        assert_eq!(report.observations.len(), 2);
        assert_eq!(report.summaries[0].errors, 1);
        assert_eq!(report.summaries[0].no_prediction, 0);
    }

    #[test]
    fn streaming_rejects_duplicates_and_never_scores_incomplete_run() {
        let (dataset, info) = streaming_fixtures();
        let mut streaming = BenchmarkAccumulator::new(&info, "fingerprint", InputMode::Full);
        let prediction = Prediction {
            top1: Some("文".into()),
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
    fn accumulator_counts_first_word_and_keeps_all_errors() {
        let info = fixture_info();
        let mut accumulator = BenchmarkAccumulator::new(
            &DatasetInfo {
                corpora: vec![info.corpora[0].clone()],
                ..info
            },
            "dataset",
            InputMode::Full,
        );
        for (id, first) in [("first", true), ("second", false)] {
            accumulator
                .observe(
                    &fixture_case(id, "a", first),
                    Prediction {
                        top1: None,
                        error: Some("error".into()),
                        elapsed_ms: None,
                    },
                )
                .unwrap();
        }
        let report = accumulator.finish(true);
        let summary = overall_summary(&report);
        assert_eq!(
            (summary.total, summary.completed, summary.correct),
            (2, 2, 0)
        );
        assert_eq!(report.observations.len(), 2);
    }

    #[test]
    fn store_pages_errors_and_clear_does_not_remove_corpora() {
        let root = std::env::temp_dir().join(format!(
            "lime-store-{}-{}",
            std::process::id(),
            now_unix_ms()
        ));
        let corpus_dir = root.join("benchmark").join("corpora");
        fs::create_dir_all(&corpus_dir).unwrap();
        fs::write(corpus_dir.join("keep.txt"), "keep").unwrap();
        let info = fixture_info();
        let mut accumulator = BenchmarkAccumulator::new(
            &DatasetInfo {
                corpora: vec![info.corpora[0].clone()],
                ..info.clone()
            },
            "dataset",
            InputMode::Full,
        );
        for id in ["a", "b"] {
            accumulator
                .observe(
                    &fixture_case(id, "a", false),
                    Prediction {
                        top1: None,
                        error: Some("error".into()),
                        elapsed_ms: None,
                    },
                )
                .unwrap();
        }
        let mut store = BenchmarkStore::new(Some(&root));
        let key = "a".repeat(64);
        store
            .save(StoredBenchmarkItem {
                version: BENCHMARK_ALGORITHM_VERSION,
                saved_at: 0,
                key: key.clone(),
                model: BenchmarkModelSelection::RimeOnly,
                model_name: "仅 Rime".into(),
                model_sha256: None,
                configuration: configuration(1, 16),
                mode: InputMode::Full,
                config: Config::default(),
                corpus_id: "a".into(),
                corpus_sha256: "corpus-a".into(),
                dataset_id: info.id,
                dataset_name: "Fixture".into(),
                dataset_version: 1,
                dataset_sha256: "dataset".into(),
                rime_snapshot_sha256: "rime".into(),
                report: accumulator.finish(true),
            })
            .unwrap();
        let page = store.error_page(&key, 2, 1).unwrap();
        assert_eq!((page.total, page.items.len()), (2, 1));
        store.clear().unwrap();
        assert!(corpus_dir.join("keep.txt").is_file());
        assert!(store.error_page(&key, 1, 10).is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rime_only_cell_key_is_stable_across_runs() {
        let identity = || CellIdentity {
            model_fingerprint: "rime-only".into(),
            effective_config: Config::default(),
            mode: InputMode::Full,
            corpus_id: "a".into(),
            corpus_sha256: "corpus-a".into(),
        };
        assert_eq!(identity().key(), identity().key());
        let mut changed = identity();
        changed.model_fingerprint = "uncacheable:run-2".into();
        assert_ne!(identity().key(), changed.key());
    }

    #[test]
    fn legacy_candidate_key_record_is_reused_by_stable_key() {
        let root = std::env::temp_dir().join(format!(
            "lime-legacy-cache-{}-{}",
            std::process::id(),
            now_unix_ms()
        ));
        let info = fixture_info();
        let mut accumulator = BenchmarkAccumulator::new(
            &DatasetInfo {
                corpora: vec![info.corpora[0].clone()],
                ..info.clone()
            },
            &info.sha256,
            InputMode::Full,
        );
        for (id, first_word) in [("first", true), ("second", false)] {
            accumulator
                .observe(
                    &fixture_case(id, "a", first_word),
                    Prediction {
                        top1: Some("错".into()),
                        error: None,
                        elapsed_ms: None,
                    },
                )
                .unwrap();
        }
        let config = Config::default();
        let legacy_bytes = serde_json::to_vec(&(
            BENCHMARK_ALGORITHM_VERSION,
            "rime-only",
            &config,
            InputMode::Full,
            "a",
            "corpus-a",
            "old-rime-snapshot",
        ))
        .unwrap();
        let legacy_key = format!("{:x}", Sha256::digest(legacy_bytes));
        let mut store = BenchmarkStore::new(Some(&root));
        store
            .save(StoredBenchmarkItem {
                version: BENCHMARK_ALGORITHM_VERSION,
                saved_at: 1,
                key: legacy_key,
                model: BenchmarkModelSelection::RimeOnly,
                model_name: "仅 Rime".into(),
                model_sha256: None,
                configuration: configuration(1, 16),
                mode: InputMode::Full,
                config: config.clone(),
                corpus_id: "a".into(),
                corpus_sha256: "corpus-a".into(),
                dataset_id: info.id,
                dataset_name: "Fixture".into(),
                dataset_version: 1,
                dataset_sha256: "dataset".into(),
                rime_snapshot_sha256: "old-rime-snapshot".into(),
                report: accumulator.finish(true),
            })
            .unwrap();
        let stable_key = CellIdentity {
            model_fingerprint: "rime-only".into(),
            effective_config: config,
            mode: InputMode::Full,
            corpus_id: "a".into(),
            corpus_sha256: "corpus-a".into(),
        }
        .key();
        let reloaded = BenchmarkStore::new(Some(&root));
        assert!(reloaded.get(&stable_key).is_some());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn current_progress_tracks_cell_count_and_rate() {
        let service = CoreService::new(None);
        let dataset = fixture_info();
        let mut state = BenchmarkRunState::idle(&dataset);
        state.results.push(BenchmarkResult {
            id: "run".into(),
            model: BenchmarkModelSelection::RimeOnly,
            model_name: "仅 Rime".into(),
            model_sha256: None,
            configuration: configuration(1, 16),
            mode: InputMode::Full,
            config: Config::default(),
            status: BenchmarkRunStatus::Idle,
            cells: vec![BenchmarkResultCell {
                key: "cell".into(),
                corpus_id: "a".into(),
                status: BenchmarkCellStatus::Pending,
                total: 3,
                completed: 0,
                correct: 0,
                no_prediction: 0,
                errors: 0,
                accuracy: None,
                error: None,
            }],
            report: None,
            error: None,
        });
        state.queue.push(BenchmarkQueueItem {
            key: "cell".into(),
            model_name: "仅 Rime".into(),
            corpus_id: "a".into(),
            status: BenchmarkCellStatus::Pending,
        });
        service.benchmark.lock().unwrap().state = state;

        service.set_cell_running(0, 0, "cell", 3);
        let running = service.benchmark_status();
        let current = running.current.expect("current cell should be exposed");
        assert_eq!((current.completed, current.total), (0, 3));
        assert_eq!(current.rate_per_second, None);
        assert_eq!(
            running.results[0].cells[0].status,
            BenchmarkCellStatus::Running
        );

        service.update_benchmark_progress(
            "cell",
            "仅 Rime",
            "a",
            Instant::now() - Duration::from_secs(2),
            3,
        );
        let progress = service.benchmark_status();
        let current = progress
            .current
            .expect("current cell should remain visible");
        assert_eq!((current.completed, current.total), (1, 3));
        assert!(current.rate_per_second.unwrap_or_default() > 0.0);
        assert!(current.eta_seconds.is_some());
        assert_eq!(progress.results[0].cells[0].completed, 1);
    }

    #[test]
    fn persisted_results_require_current_corpus_fingerprint() {
        let info = fixture_info();
        let mut store = BenchmarkStore::new(None);
        let mut accumulator = BenchmarkAccumulator::new(
            &DatasetInfo {
                corpora: vec![info.corpora[0].clone()],
                ..info.clone()
            },
            &info.sha256,
            InputMode::Full,
        );
        for (id, first_word) in [("first", true), ("second", false)] {
            accumulator
                .observe(
                    &fixture_case(id, "a", first_word),
                    Prediction {
                        top1: None,
                        error: Some("error".into()),
                        elapsed_ms: None,
                    },
                )
                .unwrap();
        }
        let report = accumulator.finish(true);
        store
            .save(StoredBenchmarkItem {
                version: BENCHMARK_ALGORITHM_VERSION,
                saved_at: 1,
                key: "b".repeat(64),
                model: BenchmarkModelSelection::RimeOnly,
                model_name: "仅 Rime".into(),
                model_sha256: None,
                configuration: configuration(1, 16),
                mode: InputMode::Full,
                config: Config::default(),
                corpus_id: "a".into(),
                corpus_sha256: "corpus-a".into(),
                dataset_id: info.id.clone(),
                dataset_name: info.name.clone(),
                dataset_version: info.version,
                dataset_sha256: info.sha256.clone(),
                rime_snapshot_sha256: "rime".into(),
                report,
            })
            .unwrap();
        assert_eq!(store.persisted_results(&info).len(), 1);
        let mut unrelated_change = info.clone();
        unrelated_change.sha256 = "dataset-new".into();
        assert!(store.persisted_results(&unrelated_change).is_empty());
        let mut changed = info;
        changed.sha256 = "dataset-new".into();
        changed.corpora[0].sha256 = "corpus-new".into();
        assert!(store.persisted_results(&changed).is_empty());
    }

    #[test]
    fn cancelled_batch_keeps_completed_rows_and_settles_pending_queue() {
        let service = CoreService::new(None);
        let dataset = fixture_info();
        let mut state = BenchmarkRunState::idle(&dataset);
        let completed_row = BenchmarkResult {
            id: "done".into(),
            model: BenchmarkModelSelection::RimeOnly,
            model_name: "仅 Rime".into(),
            model_sha256: None,
            configuration: configuration(1, 16),
            mode: InputMode::Full,
            config: Config::default(),
            status: BenchmarkRunStatus::Completed,
            cells: vec![BenchmarkResultCell {
                key: "done-cell".into(),
                corpus_id: "a".into(),
                status: BenchmarkCellStatus::Completed,
                total: 1,
                completed: 1,
                correct: 1,
                no_prediction: 0,
                errors: 0,
                accuracy: Some(1.0),
                error: None,
            }],
            report: None,
            error: None,
        };
        let pending_row = BenchmarkResult {
            id: "pending".into(),
            model: BenchmarkModelSelection::RimeOnly,
            model_name: "仅 Rime".into(),
            model_sha256: None,
            configuration: configuration(1, 16),
            mode: InputMode::Initials,
            config: Config::default(),
            status: BenchmarkRunStatus::Idle,
            cells: vec![BenchmarkResultCell {
                key: "pending-cell".into(),
                corpus_id: "a".into(),
                status: BenchmarkCellStatus::Pending,
                total: 1,
                completed: 0,
                correct: 0,
                no_prediction: 0,
                errors: 0,
                accuracy: None,
                error: None,
            }],
            report: None,
            error: None,
        };
        state.results = vec![completed_row, pending_row];
        state.queue = vec![
            BenchmarkQueueItem {
                key: "done-cell".into(),
                model_name: "仅 Rime".into(),
                corpus_id: "a".into(),
                status: BenchmarkCellStatus::Completed,
            },
            BenchmarkQueueItem {
                key: "pending-cell".into(),
                model_name: "仅 Rime".into(),
                corpus_id: "a".into(),
                status: BenchmarkCellStatus::Pending,
            },
        ];
        service.benchmark.lock().unwrap().state = state;
        service.finish_benchmark_batch(&AtomicBool::new(true), None);
        let state = service.benchmark_status();
        assert_eq!(state.status, BenchmarkRunStatus::Cancelled);
        assert_eq!(state.results[0].status, BenchmarkRunStatus::Completed);
        assert_eq!(state.results[1].status, BenchmarkRunStatus::Cancelled);
        assert_eq!(state.queue[0].status, BenchmarkCellStatus::Completed);
        assert_eq!(state.queue[1].status, BenchmarkCellStatus::Cancelled);
    }

    #[test]
    fn failed_batch_keeps_completed_rows_and_marks_pending_rows() {
        let service = CoreService::new(None);
        let dataset = fixture_info();
        let mut state = BenchmarkRunState::idle(&dataset);
        let completed_row = BenchmarkResult {
            id: "done".into(),
            model: BenchmarkModelSelection::RimeOnly,
            model_name: "仅 Rime".into(),
            model_sha256: None,
            configuration: configuration(1, 16),
            mode: InputMode::Full,
            config: Config::default(),
            status: BenchmarkRunStatus::Completed,
            cells: vec![BenchmarkResultCell {
                key: "done-cell".into(),
                corpus_id: "a".into(),
                status: BenchmarkCellStatus::Completed,
                total: 1,
                completed: 1,
                correct: 1,
                no_prediction: 0,
                errors: 0,
                accuracy: Some(1.0),
                error: None,
            }],
            report: None,
            error: None,
        };
        let pending_row = BenchmarkResult {
            id: "pending".into(),
            model: BenchmarkModelSelection::RimeOnly,
            model_name: "仅 Rime".into(),
            model_sha256: None,
            configuration: configuration(1, 16),
            mode: InputMode::Initials,
            config: Config::default(),
            status: BenchmarkRunStatus::Idle,
            cells: vec![BenchmarkResultCell {
                key: "pending-cell".into(),
                corpus_id: "a".into(),
                status: BenchmarkCellStatus::Pending,
                total: 1,
                completed: 0,
                correct: 0,
                no_prediction: 0,
                errors: 0,
                accuracy: None,
                error: None,
            }],
            report: None,
            error: None,
        };
        state.results = vec![completed_row, pending_row];
        state.queue = vec![
            BenchmarkQueueItem {
                key: "done-cell".into(),
                model_name: "仅 Rime".into(),
                corpus_id: "a".into(),
                status: BenchmarkCellStatus::Completed,
            },
            BenchmarkQueueItem {
                key: "pending-cell".into(),
                model_name: "仅 Rime".into(),
                corpus_id: "a".into(),
                status: BenchmarkCellStatus::Pending,
            },
        ];
        service.benchmark.lock().unwrap().state = state;
        service.finish_benchmark_batch(
            &AtomicBool::new(false),
            Some("candidate capture failed".into()),
        );
        let state = service.benchmark_status();
        assert_eq!(state.status, BenchmarkRunStatus::Failed);
        assert_eq!(state.results[0].status, BenchmarkRunStatus::Completed);
        assert_eq!(state.results[1].status, BenchmarkRunStatus::Failed);
        assert_eq!(state.queue[0].status, BenchmarkCellStatus::Completed);
        assert_eq!(state.queue[1].status, BenchmarkCellStatus::Failed);
    }

    #[test]
    fn paged_long_errors_fit_one_ipc_frame() {
        let long_case = fixture_case("long", "a", false);
        let page = BenchmarkErrorPage {
            key: "0".repeat(64),
            items: (0..32)
                .map(|index| {
                    let mut observation = observe(
                        &long_case,
                        InputMode::Full,
                        Prediction {
                            top1: None,
                            error: Some("runtime failure".into()),
                            elapsed_ms: None,
                        },
                    );
                    observation.case_id = format!("long-{index}");
                    observation.context = "前缀".repeat(2048);
                    observation
                })
                .collect(),
            page: 1,
            page_size: 32,
            total: 32,
        };
        let response = Response::BenchmarkErrors(page);
        let mut bytes = Vec::new();
        lime_ipc::write_json(&mut bytes, &response).unwrap();
        assert!(bytes.len() < lime_ipc::MAX_FRAME_BYTES);
    }
}
