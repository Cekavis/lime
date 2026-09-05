//! Runtime-loaded llama.cpp model support used by the production reranker.
//!
//! The binary is intentionally linked to no llama/ggml library at build time. Lime resolves the
//! packaged llama.cpp runtime when a model is loaded, preferring CUDA and falling back to CPU while
//! still making model-backed ranking use the real GGUF vocabulary and logits.

use lime_protocol::{Candidate, LlmPerformance};
use llama_cpp_sys_v3::llama_token;
pub use llama_cpp_v3::BackendPreference;
use llama_cpp_v3::{LlamaBackend, LlamaBatch, LlamaContext, LlamaModel, LoadOptions};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};

/// Maximum context exposed by Lime's validated configuration.
const MAX_CONTEXT_TOKENS: usize = 4096;
/// One shared prompt plus at most this many candidate sequences are decoded at once.
const MAX_SEQUENCE_COUNT: usize = 33;
/// Context allocated for callers that use [`LlamaRuntime::load`] directly. The service uses its
/// validated `llm_context_token_limit` through [`LlamaRuntime::load_with_context`].
pub const DEFAULT_CONTEXT_TOKENS: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelMetadata {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub sha256: String,
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
    pub performance: LlmPerformance,
}

#[derive(Default)]
struct ScoringTimings {
    tokenize: Duration,
    decode: Duration,
    logits: Duration,
    batch_count: u32,
    decode_input_tokens: usize,
    logits_output_count: usize,
    context_limit: usize,
    vocab_size: usize,
}

impl ScoringTimings {
    fn add_tokenize(&mut self, started: Instant) {
        self.tokenize += started.elapsed();
    }

    fn add_decode(&mut self, started: Instant) {
        self.decode += started.elapsed();
    }

    fn add_logits(&mut self, started: Instant) {
        self.logits += started.elapsed();
    }

    fn add_decode_workload(&mut self, input_tokens: usize, output_count: usize) {
        self.decode_input_tokens = self.decode_input_tokens.saturating_add(input_tokens);
        self.logits_output_count = self.logits_output_count.saturating_add(output_count);
    }
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
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
    // Field order is intentional: Rust drops fields in declaration order. The
    // native context and model must be released before LlamaBackend calls the
    // process-global llama_backend_free function.
    context: Mutex<LlamaContext>,
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
            .finish_non_exhaustive()
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
        let model_path = path.into();
        let dll_paths = discover_runtime_libraries(&model_path, preference)?;
        Self::load_from_libraries_with_context(model_path, dll_paths, context_tokens, preference)
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
        let model_path = path.into();
        let runtime_dir_or_library = runtime_dir_or_library.into();
        let dll_paths = resolve_runtime_libraries(&runtime_dir_or_library, preference)?;
        Self::load_from_libraries_with_context(model_path, dll_paths, context_tokens, preference)
    }

    fn load_from_libraries_with_context(
        model_path: PathBuf,
        dll_paths: Vec<PathBuf>,
        context_tokens: usize,
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
        let model = LlamaModel::load_from_file(&backend, &model_path_string, model_params)
            .map_err(|error| format!("load GGUF model {}: {error:?}", metadata.path.display()))?;

        let context_tokens = context_tokens.clamp(4, MAX_CONTEXT_TOKENS);
        // Keep the sequence count bounded. Recurrent models allocate state proportional to this
        // value; raising it to the context-token limit can consume many gigabytes. Thirty-two
        // candidate sequences plus one reserved slot cover the normal rerank path, while mismatch
        // targets are chunked when they exceed this native sequence capacity.
        let sequence_count = MAX_SEQUENCE_COUNT;
        let mut context_params = LlamaContext::default_params(&model);
        context_params.n_ctx = context_tokens as u32;
        // The application uses one total-token budget. Keep native logical, physical and output
        // capacities aligned with that budget so a request that fits the configured limit can be
        // submitted in one decode and one micro-batch.
        context_params.n_batch = context_tokens as u32;
        context_params.n_ubatch = context_tokens as u32;
        context_params.n_seq_max = sequence_count as u32;
        context_params.n_outputs_max = context_tokens as u32;
        context_params.n_outputs_max_per_seq = context_tokens as u32;
        context_params.kv_unified = true;
        let context = LlamaContext::new(&model, context_params)
            .map_err(|error| format!("create llama.cpp context: {error}"))?;

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
            backend,
            model,
            context: Mutex::new(context),
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
    /// Exact tokenizer-boundary candidates are scored in shared-prefix batches.  Candidates whose
    /// first token crosses the context/candidate boundary are scored token-by-token with a prompt
    /// rebuilt from the real token pieces and are marked `mismatch = true`.
    pub fn score_candidates(
        &self,
        preceding_text: &str,
        candidates: &[Candidate],
    ) -> Result<Vec<CandidateScore>, String> {
        self.score_candidates_with_performance(preceding_text, candidates)
            .map(|result| result.scores)
    }

    /// Compute candidate scores and collect timing/workload counters for management diagnostics.
    pub(crate) fn score_candidates_with_performance(
        &self,
        preceding_text: &str,
        candidates: &[Candidate],
    ) -> Result<ScoredCandidates, String> {
        if candidates.is_empty() {
            return Ok(ScoredCandidates {
                scores: Vec::new(),
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
                standalone,
                exact_boundary,
                mismatch,
            });
        }

        let mut context = self
            .context
            .lock()
            .map_err(|_| "llama.cpp context mutex poisoned".to_owned())?;
        let mut output = vec![None; plans.len()];

        // Keep the most recent context tokens when the input is longer than the configured
        // context.  This mirrors normal causal-LM truncation and leaves candidate tokenization
        // unchanged, so mismatch diagnostics still refer to the full user-visible context.
        let context_limit = self.runtime_context_tokens.min(self.context_tokens).max(4);
        timings.context_limit = context_limit;
        timings.vocab_size = self.vocab_size;
        let exact_indices = plans
            .iter()
            .enumerate()
            .filter(|(_, plan)| plan.exact_boundary)
            .collect::<Vec<_>>();
        let mut chunk_start = 0;
        while chunk_start < exact_indices.len() {
            let mut chunk_end = chunk_start;
            let mut prefix_tokens = 0_usize;
            let mut longest_prefix = 0_usize;
            while chunk_end < exact_indices.len()
                && chunk_end - chunk_start < self.sequence_count.saturating_sub(1)
            {
                let candidate_prefix = exact_indices[chunk_end].1.score_ids.len().saturating_sub(1);
                let next_longest = longest_prefix.max(candidate_prefix);
                if next_longest >= context_limit {
                    return Err(format!(
                        "candidate requires {} decoded prefix tokens, context limit is {context_limit}",
                        next_longest
                    ));
                }
                let base_budget = context_limit - next_longest;
                let decoded_base_tokens = if base_ids.is_empty() {
                    1 // score_batch supplies BOS for an empty user context
                } else {
                    base_ids.len().min(base_budget)
                };
                let next_batch_tokens = decoded_base_tokens
                    .saturating_add(prefix_tokens)
                    .saturating_add(candidate_prefix);
                if next_batch_tokens > context_limit {
                    if chunk_end == chunk_start {
                        return Err(format!(
                            "candidate decode requires {next_batch_tokens} tokens, context limit is {context_limit}"
                        ));
                    }
                    break;
                }
                prefix_tokens = prefix_tokens.saturating_add(candidate_prefix);
                longest_prefix = next_longest;
                chunk_end += 1;
            }

            let chunk = &exact_indices[chunk_start..chunk_end];
            let base_budget = context_limit - longest_prefix;
            let base_start = base_ids.len().saturating_sub(base_budget);
            let decode_base = base_ids[base_start..].to_vec();
            let sequences = chunk
                .iter()
                .map(|(_, plan)| plan.score_ids.clone())
                .collect::<Vec<_>>();
            timings.batch_count = timings.batch_count.saturating_add(1);
            let scores = score_batch(
                &self.backend,
                &self.model,
                &mut context,
                &decode_base,
                &sequences,
                self.sequence_count,
                &mut timings,
            )?;
            for (chunk_index, (plan_index, _)) in chunk.iter().enumerate() {
                output[*plan_index] = Some(scores[chunk_index].clone());
            }
            chunk_start = chunk_end;
        }

        // Boundary-mismatch candidates are intentionally evaluated with the real standalone token
        // ids. Each target is conditioned on the real context plus the already-consumed token
        // pieces, matching the previous fallback strategy. Build all prompt/target pairs during
        // tokenization, then submit them in shared decode batches instead of decoding once per
        // target token.
        let mismatch_indices = plans
            .iter()
            .enumerate()
            .filter(|(_, plan)| !plan.exact_boundary)
            .collect::<Vec<_>>();
        let mut mismatch_targets = Vec::new();
        for (plan_index, plan) in mismatch_indices {
            for token_index in 0..plan.score_ids.len() {
                let consumed = plan.standalone[..token_index]
                    .iter()
                    .map(|token| token.piece.as_str())
                    .collect::<String>();
                let tokenize_started = Instant::now();
                let prompt = self.tokenize(&format!("{preceding_text}{consumed}"))?;
                timings.add_tokenize(tokenize_started);
                let prompt_ids = prompt.iter().map(|token| token.id).collect::<Vec<_>>();
                let prompt_start = prompt_ids.len().saturating_sub(context_limit);
                mismatch_targets.push(MismatchTarget {
                    plan_index,
                    token_index,
                    prompt_tokens: prompt_ids[prompt_start..].to_vec(),
                    target: plan.score_ids[token_index],
                });
            }
        }
        let mismatch_scores = score_mismatch_targets(
            &self.backend,
            &self.model,
            &mut context,
            &mismatch_targets,
            self.sequence_count,
            &mut timings,
        )?;
        let mut mismatch_logprobs = plans
            .iter()
            .enumerate()
            .filter(|(_, plan)| !plan.exact_boundary)
            .map(|(index, plan)| (index, vec![None; plan.score_ids.len()]))
            .collect::<std::collections::HashMap<_, _>>();
        for (plan_index, token_index, value) in mismatch_scores {
            if let Some(values) = mismatch_logprobs.get_mut(&plan_index) {
                values[token_index] = Some(value);
            }
        }
        for (plan_index, values) in mismatch_logprobs {
            let token_logprobs = values
                .into_iter()
                .map(|value| {
                    value.ok_or_else(|| {
                        format!("mismatch candidate {plan_index} was not scored by llama.cpp")
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            output[plan_index] = Some(CandidateScore {
                token_ids: plans[plan_index].score_ids.clone(),
                logprob: token_logprobs.iter().sum(),
                token_logprobs,
                mismatch: plans[plan_index].mismatch,
            });
        }

        let scores = output
            .into_iter()
            .enumerate()
            .map(|(index, score)| {
                score.ok_or_else(|| format!("candidate {index} was not scored by llama.cpp"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let target_token_count = scores
            .iter()
            .map(|score| score.token_logprobs.len())
            .sum::<usize>();
        let mismatch_count = plans.iter().filter(|plan| !plan.exact_boundary).count();
        let scored_count = scores.len().min(u32::MAX as usize) as u32;
        Ok(ScoredCandidates {
            scores,
            performance: LlmPerformance {
                total_ms: duration_ms(total_started.elapsed()),
                tokenize_ms: duration_ms(timings.tokenize),
                decode_ms: duration_ms(timings.decode),
                logits_ms: duration_ms(timings.logits),
                candidate_count: candidates.len().min(u32::MAX as usize) as u32,
                scored_count,
                target_token_count: target_token_count.min(u32::MAX as usize) as u32,
                batch_count: timings.batch_count,
                mismatch_count: mismatch_count.min(u32::MAX as usize) as u32,
                context_token_count: base_ids.len().min(u32::MAX as usize) as u32,
                decode_input_token_count: timings.decode_input_tokens.min(u32::MAX as usize) as u32,
                logits_output_count: timings.logits_output_count.min(u32::MAX as usize) as u32,
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
    standalone: Vec<TokenInfo>,
    exact_boundary: bool,
    mismatch: bool,
}

#[derive(Clone, Debug)]
struct MismatchTarget {
    plan_index: usize,
    token_index: usize,
    prompt_tokens: Vec<llama_token>,
    target: llama_token,
}

fn score_batch(
    backend: &LlamaBackend,
    model: &LlamaModel,
    context: &mut MutexGuard<'_, LlamaContext>,
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

    context.kv_cache_clear();
    let mut batch = LlamaBatch::new(
        backend.lib.clone(),
        total_tokens.max(1) as i32,
        0,
        sequence_count as i32,
    );
    let sequence_ids = (1..=candidates.len() as i32).collect::<Vec<_>>();
    for (position, token) in decode_base.iter().copied().enumerate() {
        batch.add(
            token,
            position as i32,
            &sequence_ids,
            position + 1 == decode_base.len(),
        );
    }
    // `llama_get_logits_ith` takes the original batch-token index, not the ordinal
    // among tokens whose logits flag is enabled. Keep that index alongside each
    // target so a non-empty context does not incorrectly query batch token 0.
    let mut output_targets = Vec::new();
    for (candidate_index, tokens) in candidates.iter().enumerate() {
        for token_index in 0..tokens.len().saturating_sub(1) {
            let batch_index = batch.handle.n_tokens as usize;
            batch.add(
                tokens[token_index],
                (decode_base.len() + token_index) as i32,
                &[sequence_ids[candidate_index]],
                true,
            );
            output_targets.push((candidate_index, tokens[token_index + 1], batch_index));
        }
    }
    timings.add_decode_workload(total_tokens, output_targets.len().saturating_add(1));
    let decode_started = Instant::now();
    context
        .decode(&batch)
        .map_err(|error| format!("llama.cpp decode failed: {error}"))?;
    timings.add_decode(decode_started);

    let mut token_logprobs = candidates
        .iter()
        .map(|tokens| Vec::with_capacity(tokens.len()))
        .collect::<Vec<_>>();
    let logits_started = Instant::now();
    let base_logits = logits_for(
        backend,
        context,
        decode_base.len().saturating_sub(1),
        timings.vocab_size,
    )?;
    let base_normalizer = log_normalizer(&base_logits).ok_or_else(|| {
        "llama.cpp returned a non-finite logits normalizer for the shared context output".to_owned()
    })?;
    for (candidate_index, tokens) in candidates.iter().enumerate() {
        let value = logprob_with_normalizer(&base_logits, tokens[0], base_normalizer);
        if !value.is_finite() {
            return Err(format!(
                "llama.cpp returned a non-finite logprob for token {}",
                tokens[0]
            ));
        }
        token_logprobs[candidate_index].push(value);
    }
    for (candidate_index, target_token, batch_index) in output_targets {
        let logits = logits_for(backend, context, batch_index, timings.vocab_size)?;
        let normalizer = log_normalizer(&logits).ok_or_else(|| {
            format!("llama.cpp returned a non-finite logits normalizer for output {batch_index}")
        })?;
        let value = logprob_with_normalizer(&logits, target_token, normalizer);
        if !value.is_finite() {
            return Err(format!(
                "llama.cpp returned a non-finite logprob for token {target_token}"
            ));
        }
        token_logprobs[candidate_index].push(value);
    }
    timings.add_logits(logits_started);
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

fn score_mismatch_targets(
    backend: &LlamaBackend,
    model: &LlamaModel,
    context: &mut MutexGuard<'_, LlamaContext>,
    targets: &[MismatchTarget],
    sequence_count: usize,
    timings: &mut ScoringTimings,
) -> Result<Vec<(usize, usize, f64)>, String> {
    if targets.is_empty() {
        return Ok(Vec::new());
    }

    let max_targets_per_batch = sequence_count.saturating_sub(1).max(1);
    let mut scores = Vec::with_capacity(targets.len());
    let mut chunk_start = 0;
    while chunk_start < targets.len() {
        let mut chunk_end = chunk_start;
        let mut total_tokens = 0_usize;
        while chunk_end < targets.len() && chunk_end - chunk_start < max_targets_per_batch {
            let prompt_len = targets[chunk_end].prompt_tokens.len().max(1);
            if prompt_len > timings.context_limit {
                return Err(format!(
                    "mismatch prompt has {prompt_len} tokens, context limit is {}",
                    timings.context_limit
                ));
            }
            let next_total = total_tokens.saturating_add(prompt_len);
            if next_total > timings.context_limit {
                if chunk_end == chunk_start {
                    return Err(format!(
                        "mismatch decode batch has {next_total} tokens, context limit is {}",
                        timings.context_limit
                    ));
                }
                break;
            }
            total_tokens = next_total;
            chunk_end += 1;
        }

        let chunk = &targets[chunk_start..chunk_end];
        context.kv_cache_clear();
        let mut batch = LlamaBatch::new(
            backend.lib.clone(),
            total_tokens.max(1) as i32,
            0,
            sequence_count as i32,
        );
        let sequence_ids = (1..=chunk.len() as i32).collect::<Vec<_>>();
        let mut output_targets = Vec::with_capacity(chunk.len());
        for (target_index, target) in chunk.iter().enumerate() {
            let prompt = if target.prompt_tokens.is_empty() {
                vec![model.get_vocab().bos()]
            } else {
                target.prompt_tokens.clone()
            };
            let final_batch_index = batch.handle.n_tokens as usize + prompt.len() - 1;
            for (position, token) in prompt.into_iter().enumerate() {
                batch.add(
                    token,
                    position as i32,
                    &[sequence_ids[target_index]],
                    position + 1 == target.prompt_tokens.len().max(1),
                );
            }
            output_targets.push((target, final_batch_index));
        }
        timings.add_decode_workload(total_tokens, chunk.len());
        timings.batch_count = timings.batch_count.saturating_add(1);
        let decode_started = Instant::now();
        context
            .decode(&batch)
            .map_err(|error| format!("llama.cpp mismatch decode failed: {error}"))?;
        timings.add_decode(decode_started);

        let logits_started = Instant::now();
        for (target, batch_index) in output_targets {
            let logits = logits_for(backend, context, batch_index, timings.vocab_size)?;
            let normalizer = log_normalizer(&logits).ok_or_else(|| {
                format!(
                    "llama.cpp returned a non-finite logits normalizer for output {batch_index}"
                )
            })?;
            let value = logprob_with_normalizer(&logits, target.target, normalizer);
            if !value.is_finite() {
                return Err(format!(
                    "llama.cpp returned a non-finite logprob for token {}",
                    target.target
                ));
            }
            scores.push((target.plan_index, target.token_index, value));
        }
        timings.add_logits(logits_started);
        chunk_start = chunk_end;
    }
    Ok(scores)
}

fn logits_for(
    backend: &LlamaBackend,
    context: &MutexGuard<'_, LlamaContext>,
    index: usize,
    vocab_size: usize,
) -> Result<Vec<f32>, String> {
    let pointer =
        unsafe { (backend.lib.symbols.llama_get_logits_ith)(context.handle, index as i32) };
    if pointer.is_null() {
        return Err(format!(
            "llama.cpp returned null logits pointer for output {index}"
        ));
    }
    Ok(unsafe { std::slice::from_raw_parts(pointer, vocab_size) }.to_vec())
}

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
            assert!((single[0].logprob - scores[index].logprob).abs() < 1e-5);
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
}
