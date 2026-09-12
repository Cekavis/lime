//! Runtime-loaded llama.cpp model support used by the production reranker.
//!
//! The binary is intentionally linked to no llama/ggml library at build time. Lime resolves the
//! packaged llama.cpp runtime when a model is loaded, preferring CUDA and falling back to CPU while
//! still making model-backed ranking use the real GGUF vocabulary and logits.

use lime_protocol::{Candidate, LlmPerformance, ModelScoringPath};
use llama_cpp_sys_v3::{
    ggml_backend_buffer_type, ggml_cgraph, ggml_context, ggml_tensor, llama_sampler,
    llama_sampler_data, llama_sampler_i, llama_token,
};
pub use llama_cpp_v3::BackendPreference;
use llama_cpp_v3::{LlamaBackend, LlamaBatch, LlamaContext, LlamaModel, LoadOptions};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};

/// Maximum context exposed by Lime's validated configuration.
const MAX_CONTEXT_TOKENS: usize = 4096;
/// Maximum sequence slots accepted by the configuration schema.
const MAX_SEQUENCE_COUNT: usize = 128;
/// Sequence slots used by direct callers that do not provide the rerank setting.
pub const DEFAULT_SEQUENCE_COUNT: usize = 32;
/// Context allocated for callers that use [`LlamaRuntime::load`] directly. The service uses its
/// validated `llm_context_token_limit` and `llm_rerank_count` through the explicit load helper.
pub const DEFAULT_CONTEXT_TOKENS: usize = 1024;
/// Default limit used by direct runtime callers that do not provide service configuration.
pub const DEFAULT_INFERENCE_COUNT_LIMIT: usize = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScoringPath {
    PackedAttention,
    PaddedRecurrent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelMetadata {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub sha256: String,
}

/// Buffer sizes emitted by llama.cpp while loading a model and creating its
/// context. Values are bytes and the keys preserve native device qualifiers
/// when llama.cpp reports them (for example, `cuda0.model`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InitializationMemory {
    pub breakdown: BTreeMap<String, u64>,
}

#[derive(Clone, Debug)]
pub struct TokenInfo {
    pub id: llama_token,
    pub piece: String,
}

#[derive(Clone, Debug)]
pub struct CandidateScore {
    pub token_ids: Vec<llama_token>,
    pub token_logprobs: Vec<f64>,
    pub logprob: f64,
    pub mismatch: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ScoredCandidates {
    pub scores: Vec<CandidateScore>,
    pub scored_indices: Vec<usize>,
    pub performance: LlmPerformance,
}

#[derive(Default)]
struct ScoringTimings {
    tokenize: Duration,
    decode: Duration,
    logprob: Duration,
    batch_count: u32,
    decode_input_tokens: usize,
    logits_output_count: usize,
    context_limit: usize,
    vocab_size: usize,
    inference_count: usize,
}

impl ScoringTimings {
    fn add_tokenize(&mut self, started: Instant) {
        self.tokenize += started.elapsed();
    }

    fn add_decode(&mut self, started: Instant) {
        self.decode += started.elapsed();
    }

    fn add_logprob(&mut self, started: Instant) {
        self.logprob += started.elapsed();
    }

    fn add_decode_workload(&mut self, input_tokens: usize, output_count: usize) {
        self.decode_input_tokens = self.decode_input_tokens.saturating_add(input_tokens);
        self.logits_output_count = self.logits_output_count.saturating_add(output_count);
    }
}

#[derive(Clone, Copy)]
struct GpuSamplerFns {
    reshape_1d: unsafe extern "C" fn(*mut ggml_context, *mut ggml_tensor, i64) -> *mut ggml_tensor,
    reshape_2d:
        unsafe extern "C" fn(*mut ggml_context, *mut ggml_tensor, i64, i64) -> *mut ggml_tensor,
    soft_max: unsafe extern "C" fn(*mut ggml_context, *mut ggml_tensor) -> *mut ggml_tensor,
    get_rows: unsafe extern "C" fn(
        *mut ggml_context,
        *mut ggml_tensor,
        *mut ggml_tensor,
    ) -> *mut ggml_tensor,
    log: unsafe extern "C" fn(*mut ggml_context, *mut ggml_tensor) -> *mut ggml_tensor,
    new_tensor_1d: unsafe extern "C" fn(*mut ggml_context, i32, i64) -> *mut ggml_tensor,
    set_input: unsafe extern "C" fn(*mut ggml_tensor),
    get_data: unsafe extern "C" fn(*const ggml_tensor) -> *mut std::ffi::c_void,
    nelements: unsafe extern "C" fn(*const ggml_tensor) -> i64,
    backend_tensor_get: unsafe extern "C" fn(*mut ggml_tensor, *mut std::ffi::c_void, usize, usize),
}

struct GpuSamplerState {
    groups: Vec<Vec<llama_token>>,
    cursor: usize,
    target_tensors: Vec<*mut ggml_tensor>,
    results: Vec<*mut ggml_tensor>,
    result_ids: Vec<Vec<llama_token>>,
    target_capacity: usize,
    iface: *mut llama_sampler_i,
    fns: GpuSamplerFns,
}

struct GpuSamplerSet {
    chains: Vec<*mut llama_sampler>,
    states: Vec<*mut GpuSamplerState>,
    target_capacity: usize,
    free: unsafe extern "C" fn(*mut llama_sampler),
}

unsafe impl Send for GpuSamplerSet {}
unsafe impl Sync for GpuSamplerSet {}

impl Drop for GpuSamplerSet {
    fn drop(&mut self) {
        // Freeing the chain recursively frees each custom sampler and invokes sampler_free,
        // which releases its state and interface allocation.
        for chain in self.chains.drain(..) {
            if !chain.is_null() {
                unsafe { (self.free)(chain) };
            }
        }
        self.states.clear();
    }
}

unsafe extern "C" fn gpu_sampler_name(_: *const llama_sampler) -> *const std::ffi::c_char {
    c"lime-gpu-logprob".as_ptr()
}
unsafe extern "C" fn gpu_sampler_apply(_: *mut llama_sampler, _: *mut std::ffi::c_void) {}
unsafe extern "C" fn gpu_sampler_accept(_: *mut llama_sampler, _: llama_token) {}
unsafe extern "C" fn gpu_sampler_reset(_: *mut llama_sampler) {}
unsafe extern "C" fn gpu_sampler_backend_init(
    _: *mut llama_sampler,
    _: *mut ggml_backend_buffer_type,
    _: u32,
) -> bool {
    true
}
unsafe extern "C" fn gpu_sampler_backend_accept(
    _: *mut llama_sampler,
    _: *mut ggml_context,
    _: *mut ggml_cgraph,
    _: *mut ggml_tensor,
) {
}
unsafe extern "C" fn gpu_sampler_backend_reset(sampler: *mut llama_sampler) {
    let state = &mut *((*sampler).ctx as *mut GpuSamplerState);
    state.cursor = 0;
    state.target_tensors.clear();
    state.results.clear();
    state.result_ids.clear();
}
unsafe extern "C" fn gpu_sampler_backend_set_input(sampler: *mut llama_sampler) {
    let state = &mut *((*sampler).ctx as *mut GpuSamplerState);
    for (tensor, ids) in state
        .target_tensors
        .iter()
        .copied()
        .zip(state.result_ids.iter())
    {
        if tensor.is_null() {
            continue;
        }
        let data = (state.fns.get_data)(tensor) as *mut llama_token;
        if !data.is_null() {
            std::ptr::write_bytes(
                data.cast::<u8>(),
                0,
                state.target_capacity * std::mem::size_of::<llama_token>(),
            );
            for (index, value) in ids.iter().copied().enumerate() {
                if index >= state.target_capacity {
                    break;
                }
                *data.add(index) = value;
            }
        }
    }
}
unsafe extern "C" fn gpu_sampler_free(sampler: *mut llama_sampler) {
    let state = Box::from_raw((*sampler).ctx as *mut GpuSamplerState);
    if !state.iface.is_null() {
        drop(Box::from_raw(state.iface));
    }
}
unsafe extern "C" fn gpu_sampler_backend_apply(
    sampler: *mut llama_sampler,
    ctx: *mut ggml_context,
    _: *mut ggml_cgraph,
    data: *mut llama_sampler_data,
) {
    let state = &mut *((*sampler).ctx as *mut GpuSamplerState);
    let selected = state
        .groups
        .get(state.cursor)
        .cloned()
        .unwrap_or_else(|| vec![0]);
    state.cursor = state.cursor.saturating_add(1);
    let logits = (state.fns.reshape_1d)(ctx, (*data).logits, (state.fns.nelements)((*data).logits));
    let probs = (state.fns.soft_max)(ctx, logits);
    let rows = (state.fns.reshape_2d)(ctx, probs, 1, (state.fns.nelements)(probs));
    let ids = (state.fns.new_tensor_1d)(ctx, 26, state.target_capacity as i64);
    (state.fns.set_input)(ids);
    state.target_tensors.push(ids);
    let gathered = (state.fns.get_rows)(ctx, rows, ids);
    let result = (state.fns.log)(ctx, gathered);
    state.results.push(result);
    state.result_ids.push(selected);
    (*data).logits = result;
    (*data).candidates = ids;
}

fn new_gpu_sampler_set(
    backend: &LlamaBackend,
    sequence_count: usize,
    target_capacity: usize,
) -> Result<GpuSamplerSet, String> {
    let symbols = &backend.lib.symbols;
    let fns = GpuSamplerFns {
        reshape_1d: symbols.ggml_reshape_1d,
        reshape_2d: symbols.ggml_reshape_2d,
        soft_max: symbols.ggml_soft_max,
        get_rows: symbols.ggml_get_rows,
        log: symbols.ggml_log,
        new_tensor_1d: symbols.ggml_new_tensor_1d,
        set_input: symbols.ggml_set_input,
        get_data: symbols.ggml_get_data,
        nelements: symbols.ggml_nelements,
        backend_tensor_get: symbols.ggml_backend_tensor_get,
    };
    let mut set = GpuSamplerSet {
        chains: Vec::with_capacity(sequence_count),
        states: Vec::with_capacity(sequence_count),
        target_capacity,
        free: symbols.llama_sampler_free,
    };
    for _ in 0..sequence_count {
        let iface = Box::into_raw(Box::new(llama_sampler_i {
            name: Some(gpu_sampler_name),
            accept: Some(gpu_sampler_accept),
            apply: Some(gpu_sampler_apply),
            reset: Some(gpu_sampler_reset),
            clone: None,
            free: Some(gpu_sampler_free),
            backend_init: Some(gpu_sampler_backend_init),
            backend_accept: Some(gpu_sampler_backend_accept),
            backend_apply: Some(gpu_sampler_backend_apply),
            backend_set_input: Some(gpu_sampler_backend_set_input),
            backend_reset: Some(gpu_sampler_backend_reset),
            copy_state: None,
        }));
        let state = Box::into_raw(Box::new(GpuSamplerState {
            groups: Vec::new(),
            cursor: 0,
            target_tensors: Vec::new(),
            results: Vec::new(),
            result_ids: Vec::new(),
            target_capacity,
            iface,
            fns,
        }));
        let custom = unsafe { (symbols.llama_sampler_init)(iface, state.cast()) };
        if custom.is_null() {
            unsafe {
                drop(Box::from_raw(state));
                drop(Box::from_raw(iface));
            }
            return Err("llama_sampler_init returned null".to_owned());
        }
        let chain = unsafe {
            (symbols.llama_sampler_chain_init)((symbols.llama_sampler_chain_default_params)())
        };
        if chain.is_null() {
            unsafe {
                (symbols.llama_sampler_free)(custom);
            }
            return Err("llama_sampler_chain_init returned null".to_owned());
        }
        unsafe {
            (symbols.llama_sampler_chain_add)(chain, custom);
        }
        set.chains.push(chain);
        set.states.push(state);
    }
    Ok(set)
}

fn configure_gpu_samplers(
    gpu_samplers: &GpuSamplerSet,
    target_groups: Vec<Vec<Vec<llama_token>>>,
) -> Result<(), String> {
    if target_groups.len() != gpu_samplers.states.len() {
        return Err(format!(
            "llama.cpp sampler group count {} does not match sequence count {}",
            target_groups.len(),
            gpu_samplers.states.len()
        ));
    }

    let max_target_count = target_groups
        .iter()
        .flat_map(|groups| groups.iter().map(Vec::len))
        .max()
        .unwrap_or(0);
    if max_target_count > gpu_samplers.target_capacity {
        return Err(format!(
            "llama.cpp sampler target count {max_target_count} exceeds capacity {}",
            gpu_samplers.target_capacity
        ));
    }

    for (state_ptr, groups) in gpu_samplers.states.iter().copied().zip(target_groups) {
        let state = unsafe { &mut *state_ptr };
        state.groups = groups;
        state.cursor = 0;
        state.result_ids = if state.groups.is_empty() {
            vec![vec![0]]
        } else {
            state.groups.clone()
        };
    }

    Ok(())
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn read_gpu_sampler_row(state: &GpuSamplerState, row_index: usize) -> Result<Vec<f64>, String> {
    let result = state
        .results
        .get(row_index)
        .copied()
        .ok_or_else(|| format!("llama.cpp GPU sampler result row {row_index} is missing"))?;
    let ids = state
        .result_ids
        .get(row_index)
        .ok_or_else(|| format!("llama.cpp GPU sampler target row {row_index} is missing"))?;
    if result.is_null() {
        return Err(format!(
            "llama.cpp GPU sampler result row {row_index} is null"
        ));
    }

    let mut values = vec![0.0f32; ids.len()];
    unsafe {
        (state.fns.backend_tensor_get)(
            result,
            values.as_mut_ptr().cast(),
            0,
            values.len() * std::mem::size_of::<f32>(),
        );
    }
    if values.iter().any(|value| !value.is_finite()) {
        return Err(format!(
            "llama.cpp GPU sampler returned non-finite values for result row {row_index}"
        ));
    }
    Ok(values.into_iter().map(f64::from).collect())
}

/// A loaded GGUF model and its runtime context.
///
/// The context is protected by a mutex because llama.cpp contexts are mutable and the service can
/// receive concurrent IPC requests.  The model/vocabulary remain immutable after construction.
pub struct LlamaRuntime {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub sha256: String,
    pub dll_path: PathBuf,
    pub context_tokens: usize,
    pub runtime_context_tokens: usize,
    sequence_count: usize,
    pub vocab_size: usize,
    pub backend_name: &'static str,
    pub initialization_memory: Option<InitializationMemory>,
    scoring_path: ScoringPath,
    // Field order is intentional: Rust drops fields in declaration order. The
    // native context and model must be released before LlamaBackend calls the
    // process-global llama_backend_free function.
    context: Mutex<LlamaContext>,
    gpu_samplers: Mutex<GpuSamplerSet>,
    model: LlamaModel,
    backend: LlamaBackend,
}

impl std::fmt::Debug for LlamaRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LlamaRuntime")
            .field("path", &self.path)
            .field("size_bytes", &self.size_bytes)
            .field("sha256", &self.sha256)
            .field("dll_path", &self.dll_path)
            .field("context_tokens", &self.context_tokens)
            .field("runtime_context_tokens", &self.runtime_context_tokens)
            .field("vocab_size", &self.vocab_size)
            .field("backend_name", &self.backend_name)
            .field("initialization_memory", &self.initialization_memory)
            .field("scoring_path", &self.scoring_path)
            .finish_non_exhaustive()
    }
}

impl Drop for LlamaRuntime {
    fn drop(&mut self) {
        if let (Ok(context), Ok(samplers)) = (self.context.lock(), self.gpu_samplers.lock()) {
            for seq_id in 0..samplers.chains.len() {
                unsafe {
                    (self.backend.lib.symbols.llama_set_sampler)(
                        context.handle,
                        seq_id as i32,
                        std::ptr::null_mut(),
                    );
                }
            }
        }
    }
}

impl LlamaRuntime {
    /// Inspect a local GGUF file without loading the native runtime or allocating model memory.
    ///
    /// This is used when saving a model preset.  Actual model activation always goes through
    /// [`Self::load`] or [`Self::load_with_runtime_dir`] and therefore requires successful
    /// llama.cpp model/context initialization.
    pub fn inspect_gguf(path: impl Into<PathBuf>) -> Result<ModelMetadata, String> {
        let path = path.into();
        let mut file = File::open(&path).map_err(|error| format!("open model: {error}"))?;
        let mut magic = [0_u8; 4];
        file.read_exact(&mut magic)
            .map_err(|error| format!("read GGUF header: {error}"))?;
        if &magic != b"GGUF" {
            return Err("model is not a GGUF file".to_owned());
        }
        let size_bytes = file
            .metadata()
            .map_err(|error| format!("read model metadata: {error}"))?
            .len();
        let mut hasher = Sha256::new();
        hasher.update(magic);
        let mut buffer = [0_u8; 1024 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|error| format!("hash model: {error}"))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let sha256 = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(ModelMetadata {
            path,
            size_bytes,
            sha256,
        })
    }

    /// Load a GGUF model using the configured runtime discovery rules.
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, String> {
        let model_path = path.into();
        let preference = BackendPreference::from_env()?;
        let dll_paths = discover_runtime_libraries(&model_path, preference)?;
        Self::load_from_libraries_with_context(
            model_path,
            dll_paths,
            DEFAULT_CONTEXT_TOKENS,
            DEFAULT_SEQUENCE_COUNT,
            preference,
        )
    }

    /// Load a GGUF model using the configured runtime discovery rules and context limit.
    ///
    /// The service passes its validated `llm_context_token_limit` here so the native context and
    /// the scoring budget use the same limit.  The parameter is clamped to the minimum context
    /// size accepted by llama.cpp while preserving the caller's upper bound.
    pub fn load_with_context(
        path: impl Into<PathBuf>,
        context_tokens: usize,
    ) -> Result<Self, String> {
        let preference = BackendPreference::from_env()?;
        Self::load_with_backend_preference(path, context_tokens, preference)
    }

    /// Load a GGUF model with an explicit CUDA/CPU selection policy.
    ///
    /// `Auto` and `Cuda` attempt CUDA first and fall back to CPU when the CUDA
    /// plugin, its toolkit dependencies, GPU device, or GPU model load is
    /// unavailable. `Cpu` is strict and never attempts GPU code.
    pub fn load_with_backend_preference(
        path: impl Into<PathBuf>,
        context_tokens: usize,
        preference: BackendPreference,
    ) -> Result<Self, String> {
        Self::load_with_backend_preference_and_sequence_count(
            path,
            context_tokens,
            preference,
            DEFAULT_SEQUENCE_COUNT,
        )
    }

    /// Load a GGUF model with an explicit CUDA/CPU policy and sequence capacity.
    ///
    /// The service passes `llm_rerank_count` as the sequence capacity because each candidate is
    /// represented by one llama.cpp sequence during scoring. The value is clamped to the same
    /// bounds as the configuration schema.
    pub fn load_with_backend_preference_and_sequence_count(
        path: impl Into<PathBuf>,
        context_tokens: usize,
        preference: BackendPreference,
        sequence_count: usize,
    ) -> Result<Self, String> {
        let model_path = path.into();
        let dll_paths = discover_runtime_libraries(&model_path, preference)?;
        Self::load_from_libraries_with_context(
            model_path,
            dll_paths,
            context_tokens,
            sequence_count,
            preference,
        )
    }

    /// Load a GGUF model from an explicit runtime directory or shared-library path.
    ///
    /// A directory is searched for the platform's standard llama.cpp library name.  Passing a
    /// file path is also supported and is useful for packaging/tests that rename the library.
    pub fn load_with_runtime_dir(
        path: impl Into<PathBuf>,
        runtime_dir_or_library: impl Into<PathBuf>,
    ) -> Result<Self, String> {
        let model_path = path.into();
        let runtime_dir_or_library = runtime_dir_or_library.into();
        let preference = BackendPreference::from_env()?;
        Self::load_with_runtime_dir_and_backend_preference(
            model_path,
            runtime_dir_or_library,
            DEFAULT_CONTEXT_TOKENS,
            preference,
        )
    }

    /// Same as [`Self::load_with_runtime_dir`] with an explicit context allocation.
    pub fn load_with_runtime_dir_and_context(
        path: impl Into<PathBuf>,
        runtime_dir_or_library: impl Into<PathBuf>,
        context_tokens: usize,
    ) -> Result<Self, String> {
        let preference = BackendPreference::from_env()?;
        Self::load_with_runtime_dir_and_backend_preference(
            path,
            runtime_dir_or_library,
            context_tokens,
            preference,
        )
    }

    /// Same as [`Self::load_with_backend_preference`] with an explicit runtime
    /// directory or shared-library path.
    pub fn load_with_runtime_dir_and_backend_preference(
        path: impl Into<PathBuf>,
        runtime_dir_or_library: impl Into<PathBuf>,
        context_tokens: usize,
        preference: BackendPreference,
    ) -> Result<Self, String> {
        Self::load_with_runtime_dir_and_backend_preference_and_sequence_count(
            path,
            runtime_dir_or_library,
            context_tokens,
            preference,
            DEFAULT_SEQUENCE_COUNT,
        )
    }

    /// Same as [`Self::load_with_runtime_dir_and_backend_preference`] with an explicit sequence
    /// capacity.
    pub fn load_with_runtime_dir_and_backend_preference_and_sequence_count(
        path: impl Into<PathBuf>,
        runtime_dir_or_library: impl Into<PathBuf>,
        context_tokens: usize,
        preference: BackendPreference,
        sequence_count: usize,
    ) -> Result<Self, String> {
        let model_path = path.into();
        let runtime_dir_or_library = runtime_dir_or_library.into();
        let dll_paths = resolve_runtime_libraries(&runtime_dir_or_library, preference)?;
        Self::load_from_libraries_with_context(
            model_path,
            dll_paths,
            context_tokens,
            sequence_count,
            preference,
        )
    }

    fn load_from_libraries_with_context(
        model_path: PathBuf,
        dll_paths: Vec<PathBuf>,
        context_tokens: usize,
        sequence_count: usize,
        preference: BackendPreference,
    ) -> Result<Self, String> {
        let metadata = Self::inspect_gguf(model_path.clone())?;
        let mut errors = Vec::new();
        // Keep a failed CUDA load from immediately retrying CPU against the CUDA
        // directory.  The packaged layout contains a dedicated CPU runtime, so
        // CPU fallback must use that directory first (otherwise llama.cpp can
        // still discover a colocated CUDA plugin even with zero GPU layers).
        for backend in preference.attempts() {
            let mut ordered_paths = dll_paths.clone();
            ordered_paths.sort_by_key(|path| {
                let directory = path
                    .parent()
                    .and_then(|parent| parent.file_name())
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                let preferred = match backend {
                    llama_cpp_v3::Backend::Cuda => "cuda",
                    llama_cpp_v3::Backend::Cpu => "cpu",
                    _ => "",
                };
                if directory == preferred {
                    0_u8
                } else if directory == "cuda" || directory == "cpu" {
                    1_u8
                } else {
                    2_u8
                }
            });
            for dll_path in ordered_paths {
                match Self::load_from_library_with_metadata(
                    &metadata,
                    dll_path.clone(),
                    context_tokens,
                    sequence_count,
                    *backend,
                ) {
                    Ok(runtime) => return Ok(runtime),
                    Err(error) => errors.push(format!(
                        "{} backend at {}: {error}",
                        backend_name(*backend),
                        dll_path.display()
                    )),
                }
            }
        }
        Err(format!(
            "unable to load llama.cpp with {:?} backend preference: {}",
            preference,
            errors.join("; ")
        ))
    }

    fn load_from_library_with_metadata(
        metadata: &ModelMetadata,
        dll_path: PathBuf,
        context_tokens: usize,
        sequence_count: usize,
        backend_preference: llama_cpp_v3::Backend,
    ) -> Result<Self, String> {
        let backend_preference = match backend_preference {
            llama_cpp_v3::Backend::Cuda => BackendPreference::Cuda,
            llama_cpp_v3::Backend::Cpu => BackendPreference::Cpu,
            other => {
                return Err(format!(
                    "unsupported Lime backend selection {}",
                    backend_name(other)
                ));
            }
        };
        let backend = LlamaBackend::load_with_preference(
            LoadOptions {
                explicit_path: &dll_path,
            },
            backend_preference,
        )
        .map_err(|error| format!("load llama.cpp runtime: {error}"))?;

        let mut model_params = LlamaModel::default_params(&backend);
        // llama.cpp uses -1 to offload every model layer. CUDA is selected only
        // after a real CUDA device has been registered; CPU keeps the explicit
        // zero-layer behavior used by the original implementation.
        model_params.n_gpu_layers = if backend.uses_gpu() { -1 } else { 0 };
        let model_path_string = metadata.path.to_string_lossy();
        let (model_result, model_logs) = backend.with_log_capture(|| {
            LlamaModel::load_from_file(&backend, &model_path_string, model_params)
        });
        let model = model_result
            .map_err(|error| format!("load GGUF model {}: {error:?}", metadata.path.display()))?;
        let architecture = model.metadata("general.architecture").ok_or_else(|| {
            "unsupported model for Lime ranking: GGUF has no general.architecture metadata"
                .to_owned()
        })?;
        let architecture_lower = architecture.to_ascii_lowercase();
        let unsupported_architecture = [
            "clip",
            "bert",
            "embedding",
            "t5",
            "wavtokenizer",
            "tts",
            "pangu-embedded",
            "llama-embed",
            "eagle",
            "dflash",
            "diffusion",
        ]
        .iter()
        .any(|marker| architecture_lower.contains(marker));
        if !model.has_decoder()
            || model.has_encoder()
            || model.is_diffusion()
            || unsupported_architecture
        {
            return Err(format!(
                "unsupported model for Lime ranking: architecture {architecture} is not a causal decoder"
            ));
        }
        // llama.cpp reports Qwen3.5 and related models as hybrid rather than recurrent; both
        // require the equal-length padded path because their recurrent state graph cannot consume
        // a ragged tree. Pure attention decoders use the packed tree path.
        let scoring_path = if model.is_recurrent() || model.is_hybrid() {
            ScoringPath::PaddedRecurrent
        } else {
            ScoringPath::PackedAttention
        };

        let context_tokens = context_tokens.clamp(4, MAX_CONTEXT_TOKENS);
        let sequence_count = sequence_count.clamp(1, MAX_SEQUENCE_COUNT);
        let mut context_params = LlamaContext::default_params(&model);
        context_params.n_ctx = context_tokens as u32;
        // The application uses one total-token budget. Keep native logical, physical and output
        // capacities aligned with that budget so a request that fits the configured limit can be
        // submitted without native padding; attention scoring uses one shared-context decode and
        // one ragged continuation decode, while recurrent scoring keeps its padded continuation.
        context_params.n_batch = context_tokens as u32;
        context_params.n_ubatch = context_tokens as u32;
        context_params.n_seq_max = sequence_count as u32;
        context_params.n_outputs_max = context_tokens.max(sequence_count) as u32;
        context_params.n_outputs_max_per_seq = context_tokens as u32;
        context_params.kv_unified = true;
        let (context_result, context_logs) =
            backend.with_log_capture(|| LlamaContext::new(&model, context_params));
        let context =
            context_result.map_err(|error| format!("create llama.cpp context: {error}"))?;
        // Sampler chains are initialized by llama_set_sampler exactly once. Target-index tensors
        // keep a fixed capacity so llama.cpp can reuse the sampling graph; unused entries are
        // zero-filled by the backend sampler. The first shared-context row contains one target
        // per candidate, while continuation rows use one target per candidate sequence.
        let gpu_samplers =
            new_gpu_sampler_set(&backend, sequence_count, context_tokens.max(sequence_count))?;
        for (seq_id, chain) in gpu_samplers.chains.iter().copied().enumerate() {
            if !unsafe {
                (backend.lib.symbols.llama_set_sampler)(context.handle, seq_id as i32, chain)
            } {
                return Err(format!(
                    "llama.cpp rejected GPU sampler for sequence {seq_id}"
                ));
            }
        }

        let runtime_context_tokens =
            unsafe { (backend.lib.symbols.llama_n_ctx)(context.handle) as usize };
        let vocab = model.get_vocab();
        let vocab_size = unsafe { (backend.lib.symbols.llama_vocab_n_tokens)(vocab.handle) };
        if vocab_size <= 0 {
            return Err(format!(
                "llama.cpp returned invalid vocabulary size {vocab_size}"
            ));
        }

        Ok(Self {
            path: metadata.path.clone(),
            size_bytes: metadata.size_bytes,
            sha256: metadata.sha256.clone(),
            dll_path,
            context_tokens,
            runtime_context_tokens,
            sequence_count,
            vocab_size: vocab_size as usize,
            backend_name: backend.backend_name(),
            initialization_memory: parse_initialization_memory(
                &format!("{model_logs}{context_logs}"),
                backend.backend_name(),
            ),
            scoring_path,
            backend,
            model,
            context: Mutex::new(context),
            gpu_samplers: Mutex::new(gpu_samplers),
        })
    }

    pub fn context_tokens(&self) -> usize {
        self.context_tokens
    }

    pub fn runtime_context_tokens(&self) -> usize {
        self.runtime_context_tokens
    }

    pub fn vocab_size(&self) -> usize {
        self.vocab_size
    }

    pub fn scoring_path(&self) -> ModelScoringPath {
        match self.scoring_path {
            ScoringPath::PackedAttention => ModelScoringPath::Attention,
            ScoringPath::PaddedRecurrent => ModelScoringPath::Recurrent,
        }
    }

    /// Tokenize text with the exact vocabulary loaded from the GGUF model.
    pub fn tokenize(&self, text: &str) -> Result<Vec<TokenInfo>, String> {
        self.model
            .tokenize(text, false, false)
            .map(|ids| {
                ids.into_iter()
                    .map(|id| TokenInfo {
                        id,
                        piece: self.model.token_to_piece(id),
                    })
                    .collect()
            })
            .map_err(|error| format!("llama.cpp tokenize failed: {error}"))
    }

    /// Compute real chain log probabilities for each candidate.
    ///
    /// Candidates are scored in shared-prefix batches. Tokenizer-boundary mismatches are retained
    /// as diagnostics, while the candidate's standalone tokenization is appended to the context
    /// through the same batch path as every other candidate.
    pub fn score_candidates(
        &self,
        preceding_text: &str,
        candidates: &[Candidate],
    ) -> Result<Vec<CandidateScore>, String> {
        self.score_candidates_with_inference_limit(
            preceding_text,
            candidates,
            DEFAULT_INFERENCE_COUNT_LIMIT,
        )
        .map(|result| result.scores)
    }

    /// Compute candidate scores and collect timing/workload counters for management diagnostics.
    pub(crate) fn score_candidates_with_performance(
        &self,
        preceding_text: &str,
        candidates: &[Candidate],
    ) -> Result<ScoredCandidates, String> {
        self.score_candidates_with_inference_limit(
            preceding_text,
            candidates,
            DEFAULT_INFERENCE_COUNT_LIMIT,
        )
    }

    pub(crate) fn score_candidates_with_inference_limit(
        &self,
        preceding_text: &str,
        candidates: &[Candidate],
        inference_count_limit: usize,
    ) -> Result<ScoredCandidates, String> {
        if candidates.is_empty() {
            return Ok(ScoredCandidates {
                scores: Vec::new(),
                scored_indices: Vec::new(),
                performance: LlmPerformance::default(),
            });
        }

        let total_started = Instant::now();
        let mut timings = ScoringTimings::default();
        let tokenize_started = Instant::now();
        let base = self.tokenize(preceding_text)?;
        timings.add_tokenize(tokenize_started);
        let base_ids = base.iter().map(|token| token.id).collect::<Vec<_>>();
        let mut plans = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let tokenize_started = Instant::now();
            let standalone = self.tokenize(&candidate.commit_text)?;
            timings.add_tokenize(tokenize_started);
            if standalone.is_empty() {
                return Err(format!(
                    "candidate {:?} tokenizes to an empty sequence",
                    candidate.commit_text
                ));
            }
            let tokenize_started = Instant::now();
            let combined = self.tokenize(&format!("{preceding_text}{}", candidate.commit_text))?;
            timings.add_tokenize(tokenize_started);
            let combined_ids = combined.iter().map(|token| token.id).collect::<Vec<_>>();
            // A token crosses the boundary exactly when tokenizing the concatenated text changes
            // the tokenized prefix of the preceding text. Token IDs are authoritative here;
            // token-piece strings can contain tokenizer-specific markers (for example GPT-2's
            // leading-space marker) and therefore must not be used as byte offsets.
            let prefix_matches = combined_ids.len() >= base_ids.len()
                && combined_ids[..base_ids.len()] == base_ids[..];
            let exact_boundary = prefix_matches && combined_ids.len() > base_ids.len();
            let mismatch = !prefix_matches;
            let score_ids = if exact_boundary {
                combined_ids[base_ids.len()..].to_vec()
            } else {
                standalone.iter().map(|token| token.id).collect()
            };
            if score_ids.is_empty() {
                return Err(format!(
                    "candidate {:?} has no tokens after context tokenization",
                    candidate.commit_text
                ));
            }
            plans.push(CandidatePlan {
                score_ids,
                mismatch,
            });
        }

        let mut context = self
            .context
            .lock()
            .map_err(|_| "llama.cpp context mutex poisoned".to_owned())?;
        let mut gpu_samplers = self
            .gpu_samplers
            .lock()
            .map_err(|_| "llama.cpp GPU sampler mutex poisoned".to_owned())?;
        let mut output = vec![None; plans.len()];

        // Keep the most recent context tokens when the input is longer than the configured
        // context.  This mirrors normal causal-LM truncation and leaves candidate tokenization
        // unchanged, so mismatch diagnostics still refer to the full user-visible context.
        let context_limit = self.runtime_context_tokens.min(self.context_tokens).max(4);
        timings.context_limit = context_limit;
        timings.vocab_size = self.vocab_size;
        // Every candidate uses the same batch decode path. Exact-boundary candidates use the
        // concatenated-text suffix; mismatch candidates use their standalone tokenization and
        // append those ids to the preceding-text prompt. The boundary check remains diagnostic.
        let mut candidate_indices = (0..plans.len()).collect::<Vec<_>>();
        candidate_indices.sort_by_key(|index| (plans[*index].score_ids.len(), *index));
        let mut chunk_start = 0;
        let inference_count_limit = inference_count_limit.max(1);
        let mut omitted_due_to_limit = false;
        while chunk_start < candidate_indices.len() {
            let remaining = candidate_indices[chunk_start..]
                .iter()
                .map(|index| plans[*index].clone())
                .collect::<Vec<_>>();
            let (chunk_len, base_budget) = match self.scoring_path {
                ScoringPath::PackedAttention => plan_attention_chunk(
                    &remaining,
                    self.sequence_count,
                    context_limit,
                    base_ids.len(),
                )?,
                ScoringPath::PaddedRecurrent => plan_candidate_chunk(
                    &remaining,
                    self.sequence_count,
                    context_limit,
                    base_ids.len(),
                )?,
            };
            if chunk_len == 0 {
                return Err("candidate chunk planner produced an empty chunk".to_owned());
            }
            let chunk_end = chunk_start + chunk_len;

            let chunk = &candidate_indices[chunk_start..chunk_end];
            let has_continuation = chunk.iter().any(|index| plans[*index].score_ids.len() > 1);
            if has_continuation && timings.inference_count >= inference_count_limit {
                omitted_due_to_limit = true;
                break;
            }
            let base_start = base_ids.len().saturating_sub(base_budget);
            let decode_base = base_ids[base_start..].to_vec();
            let sequences = chunk
                .iter()
                .map(|index| plans[*index].score_ids.clone())
                .collect::<Vec<_>>();
            let scores = match self.scoring_path {
                ScoringPath::PackedAttention => score_attention_tree_batch(
                    &self.backend,
                    &self.model,
                    &mut context,
                    &mut gpu_samplers,
                    &decode_base,
                    &sequences,
                    self.sequence_count,
                    &mut timings,
                )?,
                ScoringPath::PaddedRecurrent => score_recurrent_batch(
                    &self.backend,
                    &self.model,
                    &mut context,
                    &mut gpu_samplers,
                    &decode_base,
                    &sequences,
                    self.sequence_count,
                    &mut timings,
                )?,
            };
            for (chunk_index, plan_index) in chunk.iter().enumerate() {
                let mut score = scores[chunk_index].clone();
                score.mismatch = plans[*plan_index].mismatch;
                output[*plan_index] = Some(score);
            }
            if has_continuation {
                timings.inference_count = timings.inference_count.saturating_add(1);
            }
            timings.batch_count = timings.batch_count.saturating_add(1);
            chunk_start = chunk_end;
        }

        let scored_indices = output
            .iter()
            .enumerate()
            .filter_map(|(index, score)| score.as_ref().map(|_| index))
            .collect::<Vec<_>>();
        let scores = output
            .into_iter()
            .enumerate()
            .filter_map(|(_, score)| score)
            .collect::<Vec<_>>();
        let target_token_count = scores
            .iter()
            .map(|score| score.token_logprobs.len())
            .sum::<usize>();
        let mismatch_count = scored_indices
            .iter()
            .filter(|index| plans[**index].mismatch)
            .count();
        let scored_count = scores.len().min(u32::MAX as usize) as u32;
        Ok(ScoredCandidates {
            scores,
            scored_indices,
            performance: LlmPerformance {
                total_ms: duration_ms(total_started.elapsed()),
                tokenize_ms: duration_ms(timings.tokenize),
                decode_ms: duration_ms(timings.decode),
                logits_ms: duration_ms(timings.logprob),
                candidate_count: candidates.len().min(u32::MAX as usize) as u32,
                scored_count,
                target_token_count: target_token_count.min(u32::MAX as usize) as u32,
                batch_count: timings.batch_count,
                mismatch_count: mismatch_count.min(u32::MAX as usize) as u32,
                context_token_count: base_ids.len().min(u32::MAX as usize) as u32,
                decode_input_token_count: timings.decode_input_tokens.min(u32::MAX as usize) as u32,
                logits_output_count: timings.logits_output_count.min(u32::MAX as usize) as u32,
                inference_count_limit: Some(inference_count_limit.min(u32::MAX as usize) as u32),
                omitted_candidate_count: if omitted_due_to_limit {
                    candidates
                        .len()
                        .saturating_sub(scored_count as usize)
                        .min(u32::MAX as usize) as u32
                } else {
                    0
                },
            },
        })
    }

    /// Compatibility helper for callers that need one candidate's aggregate score.
    /// Errors are represented as negative infinity; production service code uses
    /// [`Self::score_candidates`] and propagates the error instead of calling this helper.
    pub fn score(&self, preceding_text: &str, candidate: &Candidate) -> f64 {
        self.score_candidates(preceding_text, std::slice::from_ref(candidate))
            .ok()
            .and_then(|scores| scores.into_iter().next())
            .map(|score| score.logprob)
            .unwrap_or(f64::NEG_INFINITY)
    }
}

#[derive(Clone, Debug)]
struct CandidatePlan {
    score_ids: Vec<llama_token>,
    mismatch: bool,
}

/// Choose the largest first chunk that fits llama.cpp's padded continuation
/// budget. A candidate that fits by itself may still force the current chunk
/// to split because every active sequence is padded to the longest suffix.
fn plan_candidate_chunk(
    plans: &[CandidatePlan],
    sequence_count: usize,
    context_limit: usize,
    base_token_count: usize,
) -> Result<(usize, usize), String> {
    if sequence_count == 0 {
        return Err("llama sequence capacity is zero".to_owned());
    }
    let mut chunk_len = 0_usize;
    let mut longest_prefix = 0_usize;
    let mut active_prefix_count = 0_usize;
    for plan in plans.iter().take(sequence_count) {
        let candidate_prefix = plan.score_ids.len().saturating_sub(1);
        let next_longest = longest_prefix.max(candidate_prefix);
        if next_longest >= context_limit {
            return Err(format!(
                "candidate requires {} decoded prefix tokens, context limit is {context_limit}",
                next_longest
            ));
        }
        let next_active_prefix_count =
            active_prefix_count.saturating_add(usize::from(candidate_prefix > 0));
        let continuation_budget = next_longest.saturating_mul(next_active_prefix_count);
        if continuation_budget >= context_limit {
            // The candidate fits individually, but adding it would overfill
            // this padded batch. Leave it for the next chunk.
            break;
        }
        let base_budget = context_limit - continuation_budget;
        let decoded_base_tokens = if base_token_count == 0 {
            1 // score_batch supplies BOS for an empty user context
        } else {
            base_token_count.min(base_budget)
        };
        let next_batch_tokens = decoded_base_tokens.saturating_add(continuation_budget);
        if next_batch_tokens > context_limit {
            if chunk_len == 0 {
                return Err(format!(
                    "candidate decode requires {next_batch_tokens} tokens, context limit is {context_limit}"
                ));
            }
            break;
        }
        longest_prefix = next_longest;
        active_prefix_count = next_active_prefix_count;
        chunk_len += 1;
    }
    let base_budget =
        context_limit.saturating_sub(longest_prefix.saturating_mul(active_prefix_count));
    Ok((chunk_len, base_budget))
}

/// Choose the largest short-first chunk for the ragged packed attention tree. Unlike the
/// recurrent planner, each continuation contributes only its real prefix length.
fn plan_attention_chunk(
    plans: &[CandidatePlan],
    sequence_count: usize,
    context_limit: usize,
    base_token_count: usize,
) -> Result<(usize, usize), String> {
    if sequence_count == 0 {
        return Err("llama sequence capacity is zero".to_owned());
    }
    let minimum_base_tokens = if base_token_count == 0 { 1 } else { 1 };
    let mut chunk_len = 0_usize;
    let mut continuation_budget = 0_usize;
    for plan in plans.iter().take(sequence_count) {
        let next_budget =
            continuation_budget.saturating_add(plan.score_ids.len().saturating_sub(1));
        if next_budget.saturating_add(minimum_base_tokens) > context_limit {
            if chunk_len == 0 {
                return Err(format!(
                    "candidate requires {} packed continuation tokens, context limit is {context_limit}",
                    plan.score_ids.len().saturating_sub(1)
                ));
            }
            break;
        }
        continuation_budget = next_budget;
        chunk_len += 1;
    }
    if chunk_len == 0 {
        return Err("packed attention planner produced an empty chunk".to_owned());
    }
    let base_budget = context_limit.saturating_sub(continuation_budget);
    Ok((chunk_len, base_budget))
}

#[allow(clippy::too_many_arguments)]
fn score_attention_tree_batch(
    backend: &LlamaBackend,
    model: &LlamaModel,
    context: &mut MutexGuard<'_, LlamaContext>,
    gpu_samplers: &mut GpuSamplerSet,
    base_tokens: &[llama_token],
    candidates: &[Vec<llama_token>],
    sequence_count: usize,
    timings: &mut ScoringTimings,
) -> Result<Vec<CandidateScore>, String> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let decode_base = if base_tokens.is_empty() {
        vec![model.get_vocab().bos()]
    } else {
        base_tokens.to_vec()
    };
    if candidates.iter().any(|tokens| tokens.is_empty()) {
        return Err("attention tree contains an empty candidate".to_owned());
    }
    let total_tokens = decode_base.len()
        + candidates
            .iter()
            .map(|tokens| tokens.len().saturating_sub(1))
            .sum::<usize>();
    if total_tokens > timings.context_limit {
        return Err(format!(
            "packed attention tree has {total_tokens} tokens, context limit is {}",
            timings.context_limit
        ));
    }
    if candidates.len() > gpu_samplers.states.len() {
        return Err(format!(
            "llama sampler has {} sequence slots, but {} candidates were requested",
            gpu_samplers.states.len(),
            candidates.len()
        ));
    }

    context.kv_cache_clear();
    let sequence_ids = (0..candidates.len() as i32).collect::<Vec<_>>();
    let mut token_logprobs = candidates
        .iter()
        .map(|tokens| Vec::with_capacity(tokens.len()))
        .collect::<Vec<_>>();
    let output_count = 1 + candidates
        .iter()
        .map(|tokens| tokens.len().saturating_sub(1))
        .sum::<usize>();
    timings.add_decode_workload(total_tokens, output_count);

    // Decode the shared context separately. The root row is shared by all sequence ids, while
    // candidate branches are decoded afterwards; putting both in one llama.cpp batch couples the
    // shared prefix to future branch positions and changes the root logits for attention models.
    let mut base_batch = LlamaBatch::new(
        backend.lib.clone(),
        decode_base.len().max(1) as i32,
        0,
        sequence_count as i32,
    );
    for (position, token) in decode_base.iter().copied().enumerate() {
        base_batch.add(
            token,
            position as i32,
            &sequence_ids,
            position + 1 == decode_base.len(),
        );
    }
    let mut base_target_groups = vec![Vec::new(); gpu_samplers.states.len()];
    base_target_groups[0].push(candidates.iter().map(|tokens| tokens[0]).collect());
    configure_gpu_samplers(gpu_samplers, base_target_groups)?;
    let decode_started = Instant::now();
    context
        .decode(&base_batch)
        .map_err(|error| format!("llama.cpp decode failed: {error}"))?;
    timings.add_decode(decode_started);
    let logprob_started = Instant::now();
    unsafe { (backend.lib.symbols.llama_synchronize)(context.handle) };

    let base_state = unsafe { &*gpu_samplers.states[0] };
    let base_values = read_gpu_sampler_row(base_state, 0)?;
    for (candidate_index, value) in base_values.into_iter().enumerate() {
        token_logprobs[candidate_index].push(value);
    }
    let max_continuations = candidates
        .iter()
        .map(|tokens| tokens.len().saturating_sub(1))
        .max()
        .unwrap_or(0);
    if max_continuations > 0 {
        let continuation_tokens = total_tokens.saturating_sub(decode_base.len());
        let mut continuation_batch = LlamaBatch::new(
            backend.lib.clone(),
            continuation_tokens.max(1) as i32,
            0,
            sequence_count as i32,
        );
        let mut continuation_target_groups = vec![Vec::new(); gpu_samplers.states.len()];
        for (candidate_index, tokens) in candidates.iter().enumerate() {
            for token_index in 0..tokens.len().saturating_sub(1) {
                continuation_batch.add(
                    tokens[token_index],
                    (decode_base.len() + token_index) as i32,
                    &[sequence_ids[candidate_index]],
                    true,
                );
                continuation_target_groups[candidate_index].push(vec![tokens[token_index + 1]]);
            }
        }
        configure_gpu_samplers(gpu_samplers, continuation_target_groups)?;
        let decode_started = Instant::now();
        context
            .decode(&continuation_batch)
            .map_err(|error| format!("llama.cpp decode failed: {error}"))?;
        timings.add_decode(decode_started);
        unsafe { (backend.lib.symbols.llama_synchronize)(context.handle) };
        for (candidate_index, tokens) in candidates.iter().enumerate() {
            let state = unsafe { &*gpu_samplers.states[candidate_index] };
            for row_index in 0..tokens.len().saturating_sub(1) {
                let values = read_gpu_sampler_row(state, row_index)?;
                if let Some(value) = values.first().copied() {
                    token_logprobs[candidate_index].push(value);
                }
            }
        }
    }
    timings.add_logprob(logprob_started);

    if let Some((candidate_index, (lp, tokens))) = token_logprobs
        .iter()
        .zip(candidates)
        .enumerate()
        .find(|(_, (lp, tokens))| {
            lp.len() != tokens.len() || lp.iter().any(|value| !value.is_finite())
        })
    {
        let sampler_state = unsafe { &*gpu_samplers.states[candidate_index] };
        return Err(format!(
            "llama.cpp GPU sampler returned incomplete packed-tree logprob rows for candidate {candidate_index}: got {}, expected {}; sampler results={}, target rows={}, current groups={}",
            lp.len(),
            tokens.len(),
            sampler_state.results.len(),
            sampler_state.target_tensors.len(),
            sampler_state.groups.len(),
        ));
    }
    Ok(token_logprobs
        .into_iter()
        .zip(candidates)
        .map(|(logprobs, tokens)| CandidateScore {
            token_ids: tokens.clone(),
            logprob: logprobs.iter().sum(),
            token_logprobs: logprobs,
            mismatch: false,
        })
        .collect())
}

#[allow(clippy::too_many_arguments)]
fn score_recurrent_batch(
    backend: &LlamaBackend,
    model: &LlamaModel,
    context: &mut MutexGuard<'_, LlamaContext>,
    gpu_samplers: &mut GpuSamplerSet,
    base_tokens: &[llama_token],
    candidates: &[Vec<llama_token>],
    sequence_count: usize,
    timings: &mut ScoringTimings,
) -> Result<Vec<CandidateScore>, String> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let decode_base = if base_tokens.is_empty() {
        vec![model.get_vocab().bos()]
    } else {
        base_tokens.to_vec()
    };
    if decode_base.len() > timings.context_limit {
        return Err(format!(
            "base prompt has {} tokens, context limit is {}",
            decode_base.len(),
            timings.context_limit
        ));
    }
    if candidates.iter().any(|tokens| {
        tokens.is_empty()
            || decode_base.len() + tokens.len().saturating_sub(1) > timings.context_limit
    }) {
        return Err(format!(
            "candidate exceeds llama context limit of {} tokens",
            timings.context_limit
        ));
    }
    let total_tokens = decode_base.len()
        + candidates
            .iter()
            .map(|tokens| tokens.len().saturating_sub(1))
            .sum::<usize>();
    if total_tokens > timings.context_limit {
        return Err(format!(
            "llama decode batch has {total_tokens} tokens, context limit is {}",
            timings.context_limit
        ));
    }
    let max_continuations = candidates
        .iter()
        .map(|tokens| tokens.len().saturating_sub(1))
        .max()
        .unwrap_or(0);
    let active_candidates = candidates.iter().filter(|tokens| tokens.len() > 1).count();
    let physical_total_tokens = decode_base
        .len()
        .saturating_add(max_continuations.saturating_mul(active_candidates));
    if physical_total_tokens > timings.context_limit {
        return Err(format!(
            "llama padded decode batch has {physical_total_tokens} tokens, context limit is {}",
            timings.context_limit
        ));
    }
    if candidates.len() > gpu_samplers.states.len() {
        return Err(format!(
            "llama sampler has {} sequence slots, but {} candidates were requested",
            gpu_samplers.states.len(),
            candidates.len()
        ));
    }

    context.kv_cache_clear();
    // Sequence ids are zero-based in llama.cpp. Keeping them in `0..N` lets `n_seq_max=N`
    // describe exactly the number of candidate sequences without a reserved extra slot.
    let sequence_ids = (0..candidates.len() as i32).collect::<Vec<_>>();
    let mut token_logprobs = candidates
        .iter()
        .map(|tokens| Vec::with_capacity(tokens.len()))
        .collect::<Vec<_>>();
    timings.add_decode_workload(
        physical_total_tokens,
        1 + candidates
            .iter()
            .map(|tokens| tokens.len().saturating_sub(1))
            .sum::<usize>(),
    );
    // Decode the shared context by itself so every candidate's first token is scored from the
    // same final context row. Later continuation rows are read from the sampler result tensors.
    let mut base_batch = LlamaBatch::new(
        backend.lib.clone(),
        decode_base.len().max(1) as i32,
        0,
        sequence_count as i32,
    );
    for (position, token) in decode_base.iter().copied().enumerate() {
        base_batch.add(
            token,
            position as i32,
            &sequence_ids,
            position + 1 == decode_base.len(),
        );
    }
    let mut base_target_groups = vec![Vec::new(); gpu_samplers.states.len()];
    base_target_groups[0].push(candidates.iter().map(|tokens| tokens[0]).collect());
    configure_gpu_samplers(gpu_samplers, base_target_groups)?;

    let decode_started = Instant::now();
    context
        .decode(&base_batch)
        .map_err(|error| format!("llama.cpp decode failed: {error}"))?;
    timings.add_decode(decode_started);
    let logprob_started = Instant::now();
    unsafe { (backend.lib.symbols.llama_synchronize)(context.handle) };
    let base_state = unsafe { &*gpu_samplers.states[0] };
    let base_values = read_gpu_sampler_row(base_state, 0)?;
    for (candidate_index, value) in base_values.into_iter().enumerate() {
        token_logprobs[candidate_index].push(value);
    }
    timings.add_logprob(logprob_started);

    // Submit all candidate continuation prefixes in one logical batch. Padding keeps the
    // recurrent sequences equal-length inside llama.cpp; only real candidate positions request
    // sampler output.
    let max_continuations = candidates
        .iter()
        .map(|tokens| tokens.len().saturating_sub(1))
        .max()
        .unwrap_or(0);
    if max_continuations > 0 {
        let active_candidates = candidates
            .iter()
            .enumerate()
            .filter_map(|(candidate_index, tokens)| (tokens.len() > 1).then_some(candidate_index))
            .collect::<Vec<_>>();
        let mut continuation_batch = LlamaBatch::new(
            backend.lib.clone(),
            (active_candidates.len() * max_continuations) as i32,
            0,
            sequence_count as i32,
        );
        let mut continuation_target_groups = vec![Vec::new(); gpu_samplers.states.len()];
        let mut output_candidates = Vec::new();
        for &candidate_index in &active_candidates {
            let tokens = &candidates[candidate_index];
            for token_index in 0..max_continuations {
                let is_real = token_index + 1 < tokens.len();
                let token = if token_index < tokens.len() {
                    tokens[token_index]
                } else {
                    model.get_vocab().eos()
                };
                continuation_batch.add(
                    token,
                    (decode_base.len() + token_index) as i32,
                    &[sequence_ids[candidate_index]],
                    is_real,
                );
                if is_real {
                    continuation_target_groups[candidate_index].push(vec![tokens[token_index + 1]]);
                    output_candidates.push(candidate_index);
                }
            }
        }
        configure_gpu_samplers(gpu_samplers, continuation_target_groups)?;
        let decode_started = Instant::now();
        context
            .decode(&continuation_batch)
            .map_err(|error| format!("llama.cpp decode failed: {error}"))?;
        timings.add_decode(decode_started);
        let logprob_started = Instant::now();
        unsafe { (backend.lib.symbols.llama_synchronize)(context.handle) };
        let mut row_offsets = vec![0_usize; candidates.len()];
        for candidate_index in output_candidates {
            let state = unsafe { &*gpu_samplers.states[candidate_index] };
            let row_index = row_offsets[candidate_index];
            row_offsets[candidate_index] += 1;
            let values = read_gpu_sampler_row(state, row_index)?;
            token_logprobs[candidate_index].push(values[0]);
        }
        timings.add_logprob(logprob_started);
    }
    if let Some((candidate_index, (lp, tokens))) = token_logprobs
        .iter()
        .zip(candidates)
        .enumerate()
        .find(|(_, (lp, tokens))| {
            lp.len() != tokens.len() || lp.iter().any(|value| !value.is_finite())
        })
    {
        let sampler_state = gpu_samplers
            .states
            .get(candidate_index)
            .map(|state| unsafe { &**state });
        return Err(format!(
            "llama.cpp GPU sampler returned incomplete logprob rows for candidate {candidate_index}: got {} values, expected {}; sampler results={}, target rows={}, current groups={}",
            lp.len(),
            tokens.len(),
            sampler_state.map_or(0, |state| state.results.len()),
            sampler_state.map_or(0, |state| state.target_tensors.len()),
            sampler_state.map_or(0, |state| state.groups.len()),
        ));
    }
    Ok(token_logprobs
        .into_iter()
        .zip(candidates)
        .map(|(logprobs, tokens)| CandidateScore {
            token_ids: tokens.clone(),
            logprob: logprobs.iter().sum(),
            token_logprobs: logprobs,
            mismatch: false,
        })
        .collect())
}

#[cfg(test)]
fn log_normalizer(logits_pointer: &[f32]) -> Option<f64> {
    let maximum = logits_pointer
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max) as f64;
    if !maximum.is_finite() {
        return None;
    }
    let denominator = maximum
        + logits_pointer
            .iter()
            .map(|value| (*value as f64 - maximum).exp())
            .sum::<f64>()
            .ln();
    if !denominator.is_finite() {
        None
    } else {
        Some(denominator)
    }
}

#[cfg(test)]
fn logprob_with_normalizer(logits_pointer: &[f32], token: llama_token, normalizer: f64) -> f64 {
    if token < 0 || logits_pointer.len() <= token as usize {
        return f64::NEG_INFINITY;
    }
    let value = logits_pointer[token as usize] as f64;
    if !value.is_finite() {
        f64::NEG_INFINITY
    } else {
        value - normalizer
    }
}

#[cfg(test)]
fn logprob_for(logits_pointer: &[f32], token: llama_token) -> f64 {
    // `logits_for` is expanded to the full vocabulary below; this helper is kept separate so the
    // numerically stable log-sum-exp implementation is easy to test.
    log_normalizer(logits_pointer)
        .map(|normalizer| logprob_with_normalizer(logits_pointer, token, normalizer))
        .unwrap_or(f64::NEG_INFINITY)
}

/// Parse the buffer-size lines emitted by llama.cpp during model/context
/// initialization. The native log is intentionally treated as diagnostics:
/// missing or unfamiliar lines leave the corresponding fields absent instead
/// of turning a guessed value into a claimed allocation.
fn parse_initialization_memory(logs: &str, fallback_backend: &str) -> Option<InitializationMemory> {
    const MARKERS: [(&str, &str); 7] = [
        ("model", "model buffer size ="),
        ("compute", "compute buffer size ="),
        ("kv", "kv buffer size ="),
        ("output", "output buffer size ="),
        ("rs", "rs buffer size ="),
        ("lora", "lora buffer size ="),
        ("state", "state buffer size ="),
    ];
    let mut breakdown = BTreeMap::new();
    for line in logs.lines() {
        let lower = line.to_ascii_lowercase();
        for (kind, marker) in MARKERS {
            let Some(index) = lower.find(marker) else {
                continue;
            };
            let Some(bytes) = parse_mib_value(&lower[index + marker.len()..]) else {
                continue;
            };
            let backend = if kind == "state" {
                fallback_backend.to_ascii_lowercase()
            } else {
                native_memory_backend(line, index)
                    .unwrap_or_else(|| fallback_backend.to_ascii_lowercase())
            };
            let key = format!("{backend}.{kind}");
            let entry = breakdown.entry(key).or_insert(0_u64);
            *entry = entry.saturating_add(bytes);
        }
    }
    (!breakdown.is_empty()).then_some(InitializationMemory { breakdown })
}

fn parse_mib_value(value: &str) -> Option<u64> {
    let value = value.trim_start();
    let end = value
        .find(|character: char| !character.is_ascii_digit() && character != '.')
        .unwrap_or(value.len());
    if end == 0 || !value[end..].trim_start().starts_with("mib") {
        return None;
    }
    let mib = value[..end].parse::<f64>().ok()?;
    if !mib.is_finite() || mib < 0.0 {
        return None;
    }
    let bytes = mib * 1024.0 * 1024.0;
    (bytes <= u64::MAX as f64).then_some(bytes.round() as u64)
}

fn native_memory_backend(line: &str, marker_index: usize) -> Option<String> {
    let prefix = line.get(..marker_index)?;
    let token = prefix
        .rsplit(':')
        .next()
        .and_then(|segment| segment.split_whitespace().last())?
        .trim_matches(':');
    let lower = token.to_ascii_lowercase();
    (lower == "cpu" || lower.starts_with("cuda") || lower.starts_with("gpu")).then_some(lower)
}

fn backend_name(backend: llama_cpp_v3::Backend) -> &'static str {
    match backend {
        llama_cpp_v3::Backend::Cuda => "cuda",
        llama_cpp_v3::Backend::Cpu => "cpu",
        llama_cpp_v3::Backend::Vulkan => "vulkan",
        llama_cpp_v3::Backend::Hip => "hip",
        llama_cpp_v3::Backend::Sycl => "sycl",
        llama_cpp_v3::Backend::OpenCl => "opencl",
    }
}

fn platform_library_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["llama.dll", "llama-cpu.dll"]
    } else if cfg!(target_os = "macos") {
        &["libllama.dylib", "llama.dylib"]
    } else {
        &["libllama.so", "llama.so"]
    }
}

fn resolve_runtime_library(path: &Path) -> Result<PathBuf, String> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    if !path.exists() {
        return Err(format!(
            "llama runtime path does not exist: {}",
            path.display()
        ));
    }
    for name in platform_library_names() {
        let candidate = path.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "no llama.cpp library found in {}; expected {}",
        path.display(),
        platform_library_names().join(", ")
    ))
}

/// Resolve all runtime libraries that can satisfy a backend preference.
///
/// Release packages may either contain a single `llama.dll` directly under
/// the runtime root or keep backend-specific payloads under `cuda/` and
/// `cpu/`. Returning every matching path lets the loader retry CPU when CUDA
/// initialization/model offload fails.
fn resolve_runtime_libraries(
    path: &Path,
    preference: BackendPreference,
) -> Result<Vec<PathBuf>, String> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    if !path.exists() {
        return Err(format!(
            "llama runtime path does not exist: {}",
            path.display()
        ));
    }

    let mut libraries = Vec::new();
    let mut add = |candidate: PathBuf| {
        if candidate.is_file() && !libraries.iter().any(|path| path == &candidate) {
            libraries.push(candidate);
        }
    };
    for subdirectory in preference_subdirectories(preference) {
        if path.join(subdirectory).is_dir() {
            if let Ok(library) = resolve_runtime_library(&path.join(subdirectory)) {
                add(library);
            }
        }
    }
    if let Ok(library) = resolve_runtime_library(path) {
        add(library);
    }
    if libraries.is_empty() {
        return Err(format!(
            "no llama.cpp library found in {}; expected {} or backend subdirectories cuda/cpu",
            path.display(),
            platform_library_names().join(", ")
        ));
    }
    Ok(libraries)
}

fn preference_subdirectories(preference: BackendPreference) -> &'static [&'static str] {
    match preference {
        BackendPreference::Auto | BackendPreference::Cuda => &["cuda", "cpu"],
        BackendPreference::Cpu => &["cpu"],
    }
}

fn discover_runtime_libraries(
    model_path: &Path,
    preference: BackendPreference,
) -> Result<Vec<PathBuf>, String> {
    let mut attempted = Vec::new();
    if let Some(path) = std::env::var_os("LIME_LLAMA_DLL_PATH") {
        let path = PathBuf::from(path);
        attempted.push(path.display().to_string());
        return resolve_runtime_libraries(&path, preference);
    }
    if let Some(path) = std::env::var_os("LIME_LLAMA_RUNTIME_DIR") {
        let path = PathBuf::from(path);
        attempted.push(path.display().to_string());
        return resolve_runtime_libraries(&path, preference);
    }

    let mut roots = Vec::new();
    if let Some(parent) = model_path.parent() {
        roots.extend([
            parent.to_path_buf(),
            parent.join("runtime"),
            parent.join("llama"),
        ]);
    }
    if let Ok(executable) = std::env::current_exe() {
        if let Some(parent) = executable.parent() {
            roots.extend([
                parent.to_path_buf(),
                parent.join("runtime"),
                parent.join("llama"),
                parent.join("resources").join("runtime"),
                parent.join("resources").join("llama"),
            ]);
        }
    }
    if let Ok(current) = std::env::current_dir() {
        roots.extend([
            current.join("resources").join("runtime"),
            current.join("resources").join("llama"),
            current.join("runtime"),
        ]);
    }
    for root in roots {
        attempted.push(root.display().to_string());
        if let Ok(paths) = resolve_runtime_libraries(&root, preference) {
            return Ok(paths);
        }
    }
    Err(format!(
        "llama.cpp runtime library not found; set LIME_LLAMA_RUNTIME_DIR or LIME_LLAMA_DLL_PATH (searched: {})",
        attempted.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logprob_uses_stable_logsumexp() {
        let logits = [0.0_f32, -1.0];
        let value = logprob_for(&logits, 0);
        assert!(value < 0.0 && value > -1.0);
    }

    #[test]
    fn initialization_memory_parses_native_buffer_logs() {
        let logs = concat!(
            "llama_model_load:       CUDA0 model buffer size = 12.50 MiB\n",
            "llama_context:           CUDA0 KV buffer size = 2.00 MiB\n",
            "llama_context:           CUDA0 compute buffer size = 3.25 MiB\n",
            "llama_context:           CUDA0 output buffer size = 0.25 MiB\n",
        );
        let memory = parse_initialization_memory(logs, "cuda").unwrap();
        assert_eq!(
            memory.breakdown["cuda0.model"],
            12 * 1024 * 1024 + 512 * 1024
        );
        assert_eq!(memory.breakdown["cuda0.kv"], 2 * 1024 * 1024);
        assert_eq!(
            memory.breakdown["cuda0.compute"],
            3 * 1024 * 1024 + 256 * 1024
        );
        assert_eq!(memory.breakdown["cuda0.output"], 256 * 1024);
    }

    #[test]
    fn runtime_path_resolution_accepts_explicit_library_file() {
        let directory =
            std::env::temp_dir().join(format!("lime-llama-runtime-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&directory);
        let library = directory.join(platform_library_names()[0]);
        std::fs::write(&library, b"not-a-real-library").unwrap();
        assert_eq!(resolve_runtime_library(&library).unwrap(), library);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn runtime_path_resolution_reports_missing_directory() {
        let path =
            std::env::temp_dir().join(format!("lime-llama-runtime-missing-{}", std::process::id()));
        let error = resolve_runtime_library(&path).unwrap_err();
        assert!(error.contains("does not exist"));
    }

    #[test]
    fn runtime_path_resolution_reports_directory_without_library() {
        let directory =
            std::env::temp_dir().join(format!("lime-llama-runtime-empty-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&directory);
        let error = resolve_runtime_library(&directory).unwrap_err();
        assert!(error.contains("no llama.cpp library found"));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn candidate_chunk_planner_splits_padded_continuation_budget() {
        let plans = (0..32)
            .map(|_| CandidatePlan {
                score_ids: vec![1; 8],
                mismatch: false,
            })
            .collect::<Vec<_>>();

        let (first_len, first_base_budget) = plan_candidate_chunk(&plans, 32, 128, 2).unwrap();
        assert_eq!(first_len, 18);
        assert_eq!(first_base_budget, 2);

        let (second_len, second_base_budget) =
            plan_candidate_chunk(&plans[first_len..], 32, 128, 2).unwrap();
        assert_eq!(second_len, 14);
        assert_eq!(second_base_budget, 30);
        assert_eq!(first_len + second_len, plans.len());
    }

    #[test]
    fn attention_chunk_planner_counts_real_branch_rows_without_padding() {
        let plans = [2_usize, 2, 4]
            .into_iter()
            .map(|length| CandidatePlan {
                score_ids: vec![1; length],
                mismatch: false,
            })
            .collect::<Vec<_>>();
        let (chunk_len, base_budget) = plan_attention_chunk(&plans, 32, 8, 4).unwrap();
        assert_eq!(chunk_len, 3);
        assert_eq!(base_budget, 3);

        let padded = plan_candidate_chunk(&plans, 32, 8, 4).unwrap();
        assert!(padded.0 < chunk_len);
    }

    #[test]
    fn cuda_is_the_default_service_backend_and_auto_is_capability_policy() {
        assert_eq!(lime_protocol::DEFAULT_LLM_BACKEND, "cuda");
        assert_eq!(BackendPreference::default(), BackendPreference::Auto);
        assert_eq!(
            BackendPreference::Auto.attempts(),
            &[llama_cpp_v3::Backend::Cuda, llama_cpp_v3::Backend::Cpu]
        );
        assert_eq!(
            BackendPreference::Cuda.attempts(),
            &[llama_cpp_v3::Backend::Cuda, llama_cpp_v3::Backend::Cpu]
        );
        assert_eq!(
            BackendPreference::Cpu.attempts(),
            &[llama_cpp_v3::Backend::Cpu]
        );
    }

    #[test]
    fn runtime_path_resolution_prefers_cuda_subdirectory() {
        let root = std::env::temp_dir().join(format!(
            "lime-llama-runtime-backends-{}",
            std::process::id()
        ));
        let cuda = root.join("cuda");
        let cpu = root.join("cpu");
        let _ = std::fs::create_dir_all(&cuda);
        let _ = std::fs::create_dir_all(&cpu);
        std::fs::write(cuda.join(platform_library_names()[0]), b"cuda").unwrap();
        std::fs::write(cpu.join(platform_library_names()[0]), b"cpu").unwrap();

        let paths = resolve_runtime_libraries(&root, BackendPreference::Auto).unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], cuda.join(platform_library_names()[0]));
        assert_eq!(paths[1], cpu.join(platform_library_names()[0]));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    #[ignore = "requires a local llama.cpp shared library and GGUF model"]
    fn configured_runtime_scores_real_logits() {
        let model = std::env::var_os("LIME_LLAMA_TEST_MODEL")
            .map(PathBuf::from)
            .expect("LIME_LLAMA_TEST_MODEL must point to a GGUF model");
        let runtime_dir = std::env::var_os("LIME_LLAMA_RUNTIME_DIR")
            .map(PathBuf::from)
            .expect("LIME_LLAMA_RUNTIME_DIR must point to llama.cpp runtime");
        let runtime = LlamaRuntime::load_with_runtime_dir(&model, &runtime_dir).unwrap();
        let candidates = vec![
            Candidate {
                display_text: "你".to_owned(),
                commit_text: "你".to_owned(),
            },
            Candidate {
                display_text: "好".to_owned(),
                commit_text: "好".to_owned(),
            },
        ];
        let scores = runtime.score_candidates("我", &candidates).unwrap();
        assert_eq!(scores.len(), candidates.len());
        assert!(scores.iter().all(|score| score.logprob.is_finite()));
        assert!(scores
            .iter()
            .all(|score| (score.logprob - score.token_logprobs.iter().sum::<f64>()).abs() < 1e-9));
        for (index, candidate) in candidates.iter().enumerate() {
            let single = runtime
                .score_candidates("我", std::slice::from_ref(candidate))
                .unwrap();
            println!(
                "REAL compare {index}: ids={:?} merged_tokens={:?} separate_tokens={:?} merged={} separate={} diff={}",
                scores[index].token_ids,
                scores[index].token_logprobs,
                single[0].token_logprobs,
                scores[index].logprob,
                single[0].logprob,
                (single[0].logprob - scores[index].logprob).abs()
            );
            assert!(scores[index].logprob.is_finite());
            assert_eq!(
                scores[index].token_logprobs.len(),
                single[0].token_logprobs.len()
            );
            assert!(
                (scores[index].token_logprobs[0] - single[0].token_logprobs[0]).abs() < 1e-4,
                "shared-context first-token logprob changed for candidate {index}"
            );
        }

        let mismatch_case = [("hel", "lo"), ("a", "bc"), ("abc", "def"), ("你", "好")]
            .into_iter()
            .find(|(preceding, candidate)| {
                let base = runtime.tokenize(preceding).unwrap();
                let combined = runtime
                    .tokenize(&format!("{preceding}{candidate}"))
                    .unwrap();
                let base_ids = base.iter().map(|token| token.id).collect::<Vec<_>>();
                let combined_ids = combined.iter().map(|token| token.id).collect::<Vec<_>>();
                combined_ids.len() < base_ids.len()
                    || combined_ids[..base_ids.len()] != base_ids[..]
            })
            .expect("test model should provide a tokenizer-boundary mismatch case");
        let mismatch_scores = runtime
            .score_candidates(
                mismatch_case.0,
                &[Candidate {
                    display_text: mismatch_case.1.to_owned(),
                    commit_text: mismatch_case.1.to_owned(),
                }],
            )
            .unwrap();
        assert!(mismatch_scores[0].mismatch);
        assert!(mismatch_scores[0].logprob.is_finite());

        let ranked = crate::ranking::try_rerank_candidates_with_diagnostics(
            &candidates,
            "我",
            Some(&runtime),
            candidates.len(),
            1,
        )
        .unwrap();
        assert_eq!(ranked.diagnostics.len(), candidates.len());
        assert!(ranked.diagnostics.iter().all(|row| {
            row.logprob.is_finite() && (row.logprob - row.logprobs.iter().sum::<f64>()).abs() < 1e-9
        }));
    }

    #[test]
    #[ignore = "requires a local llama.cpp shared library and GGUF model"]
    fn configured_runtime_scores_rime_sized_batch_with_context() {
        let model = std::env::var_os("LIME_LLAMA_TEST_MODEL")
            .map(PathBuf::from)
            .expect("LIME_LLAMA_TEST_MODEL must point to a GGUF model");
        let runtime_dir = std::env::var_os("LIME_LLAMA_RUNTIME_DIR")
            .map(PathBuf::from)
            .expect("LIME_LLAMA_RUNTIME_DIR must point to llama.cpp runtime");
        let runtime =
            LlamaRuntime::load_with_runtime_dir_and_backend_preference_and_sequence_count(
                &model,
                &runtime_dir,
                128,
                BackendPreference::Cuda,
                32,
            )
            .unwrap();
        let candidates = [
            "你好", "👋", "拟好", "你", "尼", "泥", "逆", "拟", "腻", "倪", "霓", "匿", "妮", "溺",
            "昵", "睨", "旎", "怩", "猊", "鲵", "伲", "坭", "铌", "麑", "薿", "鿭", "呢", "𨺙",
            "𫐐", "𫠜",
        ]
        .into_iter()
        .map(|text| Candidate {
            display_text: text.to_owned(),
            commit_text: text.to_owned(),
        })
        .collect::<Vec<_>>();
        let scores = runtime.score_candidates("我", &candidates).unwrap();
        assert_eq!(scores.len(), candidates.len());
        assert!(scores
            .iter()
            .all(|score| { score.logprob.is_finite() && !score.token_logprobs.is_empty() }));
    }

    #[test]
    #[ignore = "requires a local llama.cpp shared library and GGUF model"]
    fn configured_runtime_scores_ragged_batch_with_sampler_readback() {
        let model = std::env::var_os("LIME_LLAMA_TEST_MODEL")
            .map(PathBuf::from)
            .expect("LIME_LLAMA_TEST_MODEL must point to a GGUF model");
        let runtime_dir = std::env::var_os("LIME_LLAMA_RUNTIME_DIR")
            .map(PathBuf::from)
            .expect("LIME_LLAMA_RUNTIME_DIR must point to llama.cpp runtime");
        let runtime =
            LlamaRuntime::load_with_runtime_dir_and_backend_preference_and_sequence_count(
                &model,
                &runtime_dir,
                128,
                BackendPreference::Cuda,
                32,
            )
            .unwrap();
        let texts = vec!["你"; 25]
            .into_iter()
            .chain(["😄", "🌷", "👌", "🔥", "🈴", "🙆‍♀️", "🙆‍♂️"])
            .collect::<Vec<_>>();
        let candidates = texts
            .into_iter()
            .map(|text| Candidate {
                display_text: text.to_owned(),
                commit_text: text.to_owned(),
            })
            .collect::<Vec<_>>();

        let mut scored = None;
        for iteration in 0..5 {
            let current = runtime
                .score_candidates_with_performance("我", &candidates)
                .unwrap();
            println!(
                "RAGGED performance[{iteration}]: total_ms={}, decode_ms={}, logits_ms={}, target_tokens={}, decode_rows={}, batches={}",
                current.performance.total_ms,
                current.performance.decode_ms,
                current.performance.logits_ms,
                current.performance.target_token_count,
                current.performance.logits_output_count,
                current.performance.batch_count,
            );
            scored = Some(current);
        }
        let scored = scored.expect("benchmark loop always produces a score");
        assert_eq!(scored.scores.len(), candidates.len());
        let expected_target_tokens = candidates
            .iter()
            .map(|candidate| {
                runtime
                    .tokenize(&format!("我{}", candidate.commit_text))
                    .unwrap()
                    .len()
                    .saturating_sub(runtime.tokenize("我").unwrap().len())
            })
            .sum::<usize>();
        assert_eq!(
            scored.performance.target_token_count as usize,
            expected_target_tokens
        );
        assert_eq!(scored.performance.batch_count, 1);
        assert!(scored
            .scores
            .iter()
            .all(|score| score.token_logprobs.len() == score.token_ids.len()
                && score.token_logprobs.iter().all(|value| value.is_finite())));
        for &index in &[0_usize, 25_usize, 31_usize] {
            let single = runtime
                .score_candidates("我", std::slice::from_ref(&candidates[index]))
                .unwrap();
            assert_eq!(
                single[0].token_logprobs.len(),
                scored.scores[index].token_logprobs.len()
            );
            for (merged, separate) in scored.scores[index]
                .token_logprobs
                .iter()
                .zip(&single[0].token_logprobs)
            {
                println!(
                    "COMPARE candidate {index}: merged={merged} separate={separate} diff={}",
                    (merged - separate).abs()
                );
            }
            assert!(
                (scored.scores[index].token_logprobs[0] - single[0].token_logprobs[0]).abs() < 1e-4
            );
        }
    }

    #[test]
    #[ignore = "requires a local llama.cpp shared library and GGUF model"]
    fn inference_count_limit_scores_short_candidates_first() {
        let model = std::env::var_os("LIME_LLAMA_TEST_MODEL")
            .map(PathBuf::from)
            .expect("LIME_LLAMA_TEST_MODEL must point to a GGUF model");
        let runtime_dir = std::env::var_os("LIME_LLAMA_RUNTIME_DIR")
            .map(PathBuf::from)
            .expect("LIME_LLAMA_RUNTIME_DIR must point to llama.cpp runtime");
        let runtime =
            LlamaRuntime::load_with_runtime_dir_and_backend_preference_and_sequence_count(
                &model,
                &runtime_dir,
                8,
                BackendPreference::Cpu,
                32,
            )
            .unwrap();
        let candidates = ["你", "你好", "你好吗", "🙆‍♀️", "🙆‍♂️", "🙆‍♀️🙆‍♂️"]
            .into_iter()
            .map(|text| Candidate {
                display_text: text.to_owned(),
                commit_text: text.to_owned(),
            })
            .collect::<Vec<_>>();
        let scored = runtime
            .score_candidates_with_inference_limit("我", &candidates, 1)
            .unwrap();
        assert_eq!(scored.performance.inference_count_limit, Some(1));
        assert!(scored.performance.omitted_candidate_count > 0);
        assert!(scored.scored_indices.contains(&0));
        assert!(scored.scored_indices.contains(&1));
        assert!(scored.scored_indices.contains(&2));
        assert!(scored.scored_indices.contains(&3));
        assert!(!scored.scored_indices.contains(&4));
        assert!(!scored.scored_indices.contains(&5));
    }
}
