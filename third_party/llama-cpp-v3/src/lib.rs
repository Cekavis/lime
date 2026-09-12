use llama_cpp_sys_v3::{LlamaLib, LoadError};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::{
    ffi::{c_char, c_void, CStr},
    path::Path,
    sync::Mutex,
};

pub mod backend;

pub use backend::{Backend, BackendPreference};

#[derive(Debug, thiserror::Error)]
pub enum LlamaError {
    #[error("Failed to load DLL: {0}")]
    DllLoad(#[from] LoadError),
    #[error("Failed to initialize backend")]
    BackendInit,
    #[error("Requested {backend} backend is unavailable")]
    BackendUnavailable { backend: &'static str },
    #[error("Failed to load model from file")]
    ModelLoad,
    #[error("Failed to create context")]
    ContextCreate,
    #[error("Decode error with status code {0}")]
    Decode(i32),
    #[error("Missing or empty chat template")]
    MissingChatTemplate,
    #[error("Invalid string (contains internal null byte)")]
    InvalidString,
    #[error("Failed to apply chat template (check template syntax)")]
    TemplateApply,
}

pub struct LoadOptions<'a> {
    pub explicit_path: &'a Path,
}

/// The initialized Llama capabilities backend.
/// Holds the DLL handle alive.
pub struct LlamaBackend {
    pub lib: Arc<LlamaLib>,
    selected_backend: Backend,
}

static BACKEND_USERS: AtomicUsize = AtomicUsize::new(0);
static LOG_CAPTURE_LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();

impl Drop for LlamaBackend {
    fn drop(&mut self) {
        // Backend init/free is process-global in llama.cpp. A service may construct a replacement
        // model before dropping the previous one, so count live backend handles across wrappers.
        if BACKEND_USERS.fetch_sub(1, Ordering::AcqRel) == 1 {
            unsafe {
                (self.lib.symbols.llama_backend_free)();
            }
        }
    }
}

impl LlamaBackend {
    /// Load an explicitly selected llama.cpp shared library.
    ///
    /// Lime owns runtime discovery and packaging. The vendored wrapper deliberately performs no
    /// network access and never downloads native binaries at runtime.
    pub fn load(options: LoadOptions<'_>) -> Result<Self, LlamaError> {
        Self::load_with_preference(options, BackendPreference::Auto)
    }

    /// Load a runtime and select a concrete compute backend.
    ///
    /// The dynamic plugin registry is populated before selection. `Auto` uses
    /// CUDA when ggml reports a CUDA GPU and otherwise selects CPU. `Cuda` is
    /// strict at this layer; the higher-level Lime loader catches
    /// `BackendUnavailable` and retries the dedicated CPU runtime. `Cpu` never
    /// attempts GPU code.
    pub fn load_with_preference(
        options: LoadOptions<'_>,
        preference: BackendPreference,
    ) -> Result<Self, LlamaError> {
        let dll_path = options.explicit_path.to_path_buf();

        if let Some(parent) = dll_path.parent() {
            if let Some(path_ext) = std::env::var_os("PATH") {
                let mut paths = std::env::split_paths(&path_ext).collect::<Vec<_>>();
                let parent_buf = parent.to_path_buf();
                if !paths.contains(&parent_buf) {
                    paths.insert(0, parent_buf);
                    if let Ok(new_path) = std::env::join_paths(paths) {
                        std::env::set_var("PATH", new_path);
                    }
                }
            }
        }

        let lib = LlamaLib::open(&dll_path)?;

        if let Some(parent) = dll_path.parent() {
            let parent_str = parent.to_string_lossy().to_string();
            let c_parent = std::ffi::CString::new(parent_str).unwrap();
            unsafe {
                (lib.symbols.ggml_backend_load_all_from_path)(c_parent.as_ptr());
            }
        } else {
            unsafe {
                (lib.symbols.ggml_backend_load_all)();
            }
        }

        unsafe {
            (lib.symbols.llama_backend_init)();
        }

        // Count this initialized handle before capability selection. If a
        // strict CUDA request is rejected below, the same accounting path can
        // safely release the process-global backend without underflowing when
        // another model is already active.
        BACKEND_USERS.fetch_add(1, Ordering::AcqRel);

        let cuda_available = lib.has_cuda_device();
        let selected_backend = match preference {
            BackendPreference::Auto if cuda_available => Backend::Cuda,
            BackendPreference::Auto => Backend::Cpu,
            BackendPreference::Cuda if cuda_available => Backend::Cuda,
            BackendPreference::Cuda => {
                // We initialized the process-global backend above, so balance
                // the live-handle count before returning the strict-selection
                // error. This also preserves an already active model.
                if BACKEND_USERS.fetch_sub(1, Ordering::AcqRel) == 1 {
                    unsafe {
                        (lib.symbols.llama_backend_free)();
                    }
                }
                return Err(LlamaError::BackendUnavailable { backend: "CUDA" });
            }
            BackendPreference::Cpu => Backend::Cpu,
        };

        Ok(Self {
            lib: Arc::new(lib),
            selected_backend,
        })
    }

    /// The backend selected for this runtime (`cuda` or `cpu`).
    pub const fn selected_backend(&self) -> Backend {
        self.selected_backend
    }

    pub const fn backend_name(&self) -> &'static str {
        match self.selected_backend {
            Backend::Cuda => "cuda",
            Backend::Cpu => "cpu",
            Backend::Vulkan => "vulkan",
            Backend::Hip => "hip",
            Backend::Sycl => "sycl",
            Backend::OpenCl => "opencl",
        }
    }

    pub const fn uses_gpu(&self) -> bool {
        matches!(self.selected_backend, Backend::Cuda)
    }

    /// Run an initialization operation while collecting llama.cpp's native log
    /// output. llama.cpp exposes the callback as process-global state, so the
    /// previous callback and user data are restored before this method returns.
    /// Older runtimes without the public logging hooks simply return an empty
    /// log string and still run the operation normally.
    pub fn with_log_capture<T>(&self, operation: impl FnOnce() -> T) -> (T, String) {
        let (Some(set), Some(get)) = (
            self.lib.symbols.llama_log_set,
            self.lib.symbols.llama_log_get,
        ) else {
            return (operation(), String::new());
        };

        let _capture_lock = LOG_CAPTURE_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("llama log capture mutex poisoned");

        let mut previous_callback = None;
        let mut previous_user_data = std::ptr::null_mut();
        unsafe {
            get(&mut previous_callback, &mut previous_user_data);
        }

        let logs = Arc::new(Mutex::new(String::new()));
        let user_data = Arc::into_raw(Arc::clone(&logs)) as *mut c_void;
        unsafe {
            set(Some(capture_log), user_data);
        }
        let guard = LogCaptureGuard {
            set,
            previous_callback,
            previous_user_data,
            user_data,
        };

        let result = operation();
        drop(guard);
        let captured = logs.lock().map(|value| value.clone()).unwrap_or_default();
        (result, captured)
    }
}

struct LogCaptureGuard {
    set: unsafe extern "C" fn(llama_cpp_sys_v3::ggml_log_callback, *mut c_void),
    previous_callback: llama_cpp_sys_v3::ggml_log_callback,
    previous_user_data: *mut c_void,
    user_data: *mut c_void,
}

impl Drop for LogCaptureGuard {
    fn drop(&mut self) {
        unsafe {
            (self.set)(self.previous_callback, self.previous_user_data);
            // `user_data` owns one strong Arc reference created by into_raw.
            drop(Arc::from_raw(self.user_data as *const Mutex<String>));
        }
    }
}

unsafe extern "C" fn capture_log(
    _level: llama_cpp_sys_v3::ggml_log_level,
    text: *const c_char,
    user_data: *mut c_void,
) {
    if text.is_null() || user_data.is_null() {
        return;
    }
    let logs = &*(user_data as *const Mutex<String>);
    let Ok(mut logs) = logs.lock() else {
        return;
    };
    let text = CStr::from_ptr(text).to_string_lossy();
    logs.push_str(&text);
}

/// A loaded GGUF model
pub struct LlamaModel {
    pub backend: Arc<LlamaLib>,
    pub handle: *mut llama_cpp_sys_v3::llama_model,
}

impl Drop for LlamaModel {
    fn drop(&mut self) {
        unsafe {
            (self.backend.symbols.llama_model_free)(self.handle);
        }
    }
}

unsafe impl Send for LlamaModel {}
unsafe impl Sync for LlamaModel {}

impl LlamaModel {
    pub fn load_from_file(
        backend: &LlamaBackend,
        path: &str,
        params: llama_cpp_sys_v3::llama_model_params,
    ) -> Result<Self, LlamaError> {
        let c_path = std::ffi::CString::new(path).map_err(|_| LlamaError::InvalidString)?;
        let handle =
            unsafe { (backend.lib.symbols.llama_model_load_from_file)(c_path.as_ptr(), params) };

        if handle.is_null() {
            return Err(LlamaError::ModelLoad);
        }

        Ok(Self {
            backend: backend.lib.clone(),
            handle,
        })
    }

    pub fn default_params(backend: &LlamaBackend) -> llama_cpp_sys_v3::llama_model_params {
        unsafe { (backend.lib.symbols.llama_model_default_params)() }
    }

    pub fn is_recurrent(&self) -> bool {
        unsafe { (self.backend.symbols.llama_model_is_recurrent)(self.handle) }
    }

    pub fn is_hybrid(&self) -> bool {
        unsafe { (self.backend.symbols.llama_model_is_hybrid)(self.handle) }
    }

    pub fn is_diffusion(&self) -> bool {
        unsafe { (self.backend.symbols.llama_model_is_diffusion)(self.handle) }
    }

    pub fn has_encoder(&self) -> bool {
        unsafe { (self.backend.symbols.llama_model_has_encoder)(self.handle) }
    }

    pub fn has_decoder(&self) -> bool {
        unsafe { (self.backend.symbols.llama_model_has_decoder)(self.handle) }
    }

    pub fn metadata(&self, key: &str) -> Option<String> {
        let key = std::ffi::CString::new(key).ok()?;
        let mut buffer = vec![0_i8; 256];
        let size = unsafe {
            (self.backend.symbols.llama_model_meta_val_str)(
                self.handle,
                key.as_ptr(),
                buffer.as_mut_ptr(),
                buffer.len(),
            )
        };
        if size < 0 {
            return None;
        }
        if size as usize >= buffer.len() {
            buffer.resize(size as usize + 1, 0);
            let size = unsafe {
                (self.backend.symbols.llama_model_meta_val_str)(
                    self.handle,
                    key.as_ptr(),
                    buffer.as_mut_ptr(),
                    buffer.len(),
                )
            };
            if size < 0 || size as usize >= buffer.len() {
                return None;
            }
        }
        Some(
            unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) }
                .to_string_lossy()
                .into_owned(),
        )
    }

    pub fn get_vocab(&self) -> LlamaVocab {
        let handle = unsafe { (self.backend.symbols.llama_model_get_vocab)(self.handle) };
        LlamaVocab {
            backend: self.backend.clone(),
            handle,
        }
    }

    pub fn tokenize(
        &self,
        text: &str,
        add_special: bool,
        parse_special: bool,
    ) -> Result<Vec<llama_cpp_sys_v3::llama_token>, LlamaError> {
        let vocab = self.get_vocab();
        let c_text = std::ffi::CString::new(text).map_err(|_| LlamaError::InvalidString)?;

        // First call to get required size
        let n_tokens = unsafe {
            (self.backend.symbols.llama_tokenize)(
                vocab.handle,
                c_text.as_ptr(),
                text.len() as i32,
                std::ptr::null_mut(),
                0,
                add_special,
                parse_special,
            )
        };

        if n_tokens < 0 {
            let mut tokens = vec![0; (-n_tokens) as usize];
            let actual_tokens = unsafe {
                (self.backend.symbols.llama_tokenize)(
                    vocab.handle,
                    c_text.as_ptr(),
                    text.len() as i32,
                    tokens.as_mut_ptr(),
                    tokens.len() as i32,
                    add_special,
                    parse_special,
                )
            };
            if actual_tokens < 0 {
                return Err(LlamaError::Decode(actual_tokens));
            }
            tokens.truncate(actual_tokens as usize);
            Ok(tokens)
        } else {
            let mut tokens = vec![0; n_tokens as usize];
            let actual_tokens = unsafe {
                (self.backend.symbols.llama_tokenize)(
                    vocab.handle,
                    c_text.as_ptr(),
                    text.len() as i32,
                    tokens.as_mut_ptr(),
                    tokens.len() as i32,
                    add_special,
                    parse_special,
                )
            };
            if actual_tokens < 0 {
                return Err(LlamaError::Decode(actual_tokens));
            }
            tokens.truncate(actual_tokens as usize);
            Ok(tokens)
        }
    }

    pub fn token_to_piece(&self, token: llama_cpp_sys_v3::llama_token) -> String {
        let vocab = self.get_vocab();
        let mut buf = vec![0u8; 128];
        let n = unsafe {
            (self.backend.symbols.llama_token_to_piece)(
                vocab.handle,
                token,
                buf.as_mut_ptr() as *mut std::ffi::c_char,
                buf.len() as i32,
                0,
                true,
            )
        };

        if n < 0 {
            buf.resize((-n) as usize, 0);
            unsafe {
                (self.backend.symbols.llama_token_to_piece)(
                    vocab.handle,
                    token,
                    buf.as_mut_ptr() as *mut std::ffi::c_char,
                    buf.len() as i32,
                    0,
                    true,
                );
            }
        } else {
            buf.truncate(n as usize);
        }

        String::from_utf8_lossy(&buf).to_string()
    }

    pub fn apply_chat_template(
        &self,
        tmpl: Option<&str>,
        messages: &[ChatMessage],
        add_ass: bool,
    ) -> Result<String, LlamaError> {
        let resolved_tmpl = match tmpl {
            Some(s) => s.to_string(),
            None => self
                .get_chat_template(None)
                .ok_or(LlamaError::MissingChatTemplate)?,
        };

        if resolved_tmpl.trim().is_empty() {
            return Err(LlamaError::MissingChatTemplate);
        }

        let c_tmpl =
            std::ffi::CString::new(resolved_tmpl).map_err(|_| LlamaError::InvalidString)?;

        let mut c_messages = Vec::with_capacity(messages.len());
        let mut c_strings = Vec::with_capacity(messages.len() * 2);

        for msg in messages {
            let role =
                std::ffi::CString::new(msg.role.as_str()).map_err(|_| LlamaError::InvalidString)?;
            let content = std::ffi::CString::new(msg.content.as_str())
                .map_err(|_| LlamaError::InvalidString)?;

            let msg_struct = llama_cpp_sys_v3::llama_chat_message {
                role: role.as_ptr(),
                content: content.as_ptr(),
            };

            c_messages.push(msg_struct);
            c_strings.push(role);
            c_strings.push(content);
        }

        // First call to get required size
        let n_chars = unsafe {
            (self.backend.symbols.llama_chat_apply_template)(
                c_tmpl.as_ptr(),
                c_messages.as_ptr(),
                c_messages.len(),
                add_ass,
                std::ptr::null_mut(),
                0,
            )
        };

        if n_chars < 0 {
            return Err(LlamaError::Decode(n_chars));
        }

        let mut buf = vec![0u8; n_chars as usize + 1];
        let actual_chars = unsafe {
            (self.backend.symbols.llama_chat_apply_template)(
                c_tmpl.as_ptr(),
                c_messages.as_ptr(),
                c_messages.len(),
                add_ass,
                buf.as_mut_ptr() as *mut std::ffi::c_char,
                buf.len() as i32,
            )
        };

        if actual_chars < 0 {
            return Err(LlamaError::Decode(actual_chars));
        }

        buf.truncate(actual_chars as usize);
        Ok(String::from_utf8_lossy(&buf).to_string())
    }

    pub fn get_chat_template(&self, name: Option<&str>) -> Option<String> {
        let c_name = name.and_then(|s| std::ffi::CString::new(s).ok());
        let name_ptr = c_name
            .as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or(std::ptr::null());

        let mut buf = vec![0u8; 1024];
        let n = unsafe {
            (self.backend.symbols.llama_model_chat_template)(
                self.handle,
                name_ptr,
                buf.as_mut_ptr() as *mut std::ffi::c_char,
                buf.len(),
            )
        };

        if n < 0 {
            return None;
        }

        if n as usize >= buf.len() {
            buf.resize(n as usize + 1, 0);
            unsafe {
                (self.backend.symbols.llama_model_chat_template)(
                    self.handle,
                    name_ptr,
                    buf.as_mut_ptr() as *mut std::ffi::c_char,
                    buf.len(),
                );
            }
        }

        buf.truncate(n as usize);
        Some(String::from_utf8_lossy(&buf).to_string())
    }
}

pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

pub struct LlamaVocab {
    pub backend: Arc<LlamaLib>,
    pub handle: *const llama_cpp_sys_v3::llama_vocab,
}

impl LlamaVocab {
    pub fn bos(&self) -> llama_cpp_sys_v3::llama_token {
        unsafe { (self.backend.symbols.llama_vocab_bos)(self.handle) }
    }

    pub fn eos(&self) -> llama_cpp_sys_v3::llama_token {
        unsafe { (self.backend.symbols.llama_vocab_eos)(self.handle) }
    }

    pub fn is_eog(&self, token: llama_cpp_sys_v3::llama_token) -> bool {
        unsafe { (self.backend.symbols.llama_vocab_is_eog)(self.handle, token) }
    }
}

pub struct LlamaSampler {
    pub backend: Arc<LlamaLib>,
    pub handle: *mut llama_cpp_sys_v3::llama_sampler,
}

impl Drop for LlamaSampler {
    fn drop(&mut self) {
        unsafe {
            (self.backend.symbols.llama_sampler_free)(self.handle);
        }
    }
}

impl LlamaSampler {
    pub fn new_chain(backend: Arc<LlamaLib>, no_perf: bool) -> Self {
        let params = llama_cpp_sys_v3::llama_sampler_chain_params { no_perf };
        let handle = unsafe { (backend.symbols.llama_sampler_chain_init)(params) };
        Self { backend, handle }
    }

    pub fn new_greedy(backend: Arc<LlamaLib>) -> Self {
        let handle = unsafe { (backend.symbols.llama_sampler_init_greedy)() };
        Self { backend, handle }
    }

    pub fn new_temp(backend: Arc<LlamaLib>, temp: f32) -> Self {
        let handle = unsafe { (backend.symbols.llama_sampler_init_temp)(temp) };
        Self { backend, handle }
    }

    pub fn new_top_k(backend: Arc<LlamaLib>, k: i32) -> Self {
        let handle = unsafe { (backend.symbols.llama_sampler_init_top_k)(k) };
        Self { backend, handle }
    }

    pub fn new_top_p(backend: Arc<LlamaLib>, p: f32, min_keep: usize) -> Self {
        let handle = unsafe { (backend.symbols.llama_sampler_init_top_p)(p, min_keep) };
        Self { backend, handle }
    }

    pub fn new_min_p(backend: Arc<LlamaLib>, p: f32, min_keep: usize) -> Self {
        let handle = unsafe { (backend.symbols.llama_sampler_init_min_p)(p, min_keep) };
        Self { backend, handle }
    }

    pub fn new_typical(backend: Arc<LlamaLib>, p: f32, min_keep: usize) -> Self {
        let handle = unsafe { (backend.symbols.llama_sampler_init_typical)(p, min_keep) };
        Self { backend, handle }
    }

    pub fn new_mirostat_v2(backend: Arc<LlamaLib>, seed: u32, tau: f32, eta: f32) -> Self {
        let handle = unsafe { (backend.symbols.llama_sampler_init_mirostat_v2)(seed, tau, eta) };
        Self { backend, handle }
    }

    pub fn new_penalties(
        backend: Arc<LlamaLib>,
        last_n: i32,
        repeat: f32,
        freq: f32,
        present: f32,
    ) -> Self {
        let handle = unsafe {
            (backend.symbols.llama_sampler_init_penalties)(last_n, repeat, freq, present)
        };
        Self { backend, handle }
    }

    pub fn new_dist(backend: Arc<LlamaLib>, seed: u32) -> Self {
        let handle = unsafe { (backend.symbols.llama_sampler_init_dist)(seed) };
        Self { backend, handle }
    }

    pub fn add(&mut self, other: LlamaSampler) {
        unsafe {
            (self.backend.symbols.llama_sampler_chain_add)(self.handle, other.handle);
        }
        std::mem::forget(other);
    }

    pub fn sample(&self, ctx: &LlamaContext, idx: i32) -> llama_cpp_sys_v3::llama_token {
        unsafe { (self.backend.symbols.llama_sampler_sample)(self.handle, ctx.handle, idx) }
    }

    pub fn accept(&self, token: llama_cpp_sys_v3::llama_token) {
        unsafe {
            (self.backend.symbols.llama_sampler_accept)(self.handle, token);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_callback_round_trip_with_packaged_runtime() {
        let Ok(path) = std::env::var("LIME_LLAMA_DLL_PATH") else {
            return;
        };
        let backend = LlamaBackend::load_with_preference(
            LoadOptions {
                explicit_path: Path::new(&path),
            },
            BackendPreference::Cpu,
        )
        .expect("packaged llama.cpp runtime should load");
        assert!(backend.lib.symbols.llama_log_set.is_some());
        assert!(backend.lib.symbols.llama_log_get.is_some());
        let (value, logs) = backend.with_log_capture(|| 7_u8);
        assert_eq!(value, 7);
        assert!(logs.is_empty());
    }
}

/// Inference context attached to a model
pub struct LlamaContext {
    pub backend: Arc<LlamaLib>,
    pub handle: *mut llama_cpp_sys_v3::llama_context,
}

impl Drop for LlamaContext {
    fn drop(&mut self) {
        unsafe {
            (self.backend.symbols.llama_free)(self.handle);
        }
    }
}

unsafe impl Send for LlamaContext {}
unsafe impl Sync for LlamaContext {}

impl LlamaContext {
    pub fn new(
        model: &LlamaModel,
        params: llama_cpp_sys_v3::llama_context_params,
    ) -> Result<Self, LlamaError> {
        let handle = unsafe { (model.backend.symbols.llama_init_from_model)(model.handle, params) };

        if handle.is_null() {
            return Err(LlamaError::ContextCreate);
        }

        Ok(Self {
            backend: model.backend.clone(),
            handle,
        })
    }

    pub fn default_params(model: &LlamaModel) -> llama_cpp_sys_v3::llama_context_params {
        unsafe { (model.backend.symbols.llama_context_default_params)() }
    }

    pub fn decode(&mut self, batch: &LlamaBatch) -> Result<(), LlamaError> {
        let res = unsafe { (self.backend.symbols.llama_decode)(self.handle, batch.handle) };
        if res != 0 {
            Err(LlamaError::Decode(res))
        } else {
            Ok(())
        }
    }

    /// Clear the KV cache for this context.
    /// Resets all cached key/value state, allowing the context to be reused
    /// for a fresh generation without reallocating.
    pub fn kv_cache_clear(&mut self) {
        unsafe {
            let memory = (self.backend.symbols.llama_get_memory)(self.handle);
            (self.backend.symbols.llama_memory_clear)(memory, true);
        }
    }

    /// Remove KV cache entries for sequence `seq_id` in position range `[p0, p1)`.
    ///
    /// If `p0 < 0`, removes from the beginning. If `p1 < 0`, removes to the end.
    /// Returns `true` if the operation succeeded.
    ///
    /// This is used for incremental prompt encoding: when the conversation
    /// diverges from the cached prefix, only the divergent suffix needs to
    /// be removed and re-decoded, avoiding a full KV cache clear.
    pub fn kv_cache_seq_rm(
        &mut self,
        seq_id: llama_cpp_sys_v3::llama_seq_id,
        p0: llama_cpp_sys_v3::llama_pos,
        p1: llama_cpp_sys_v3::llama_pos,
    ) -> bool {
        unsafe {
            let memory = (self.backend.symbols.llama_get_memory)(self.handle);
            (self.backend.symbols.llama_memory_seq_rm)(memory, seq_id, p0, p1)
        }
    }
}

pub struct LlamaBatch {
    pub backend: Arc<LlamaLib>,
    pub handle: llama_cpp_sys_v3::llama_batch,
}

impl Drop for LlamaBatch {
    fn drop(&mut self) {
        unsafe {
            (self.backend.symbols.llama_batch_free)(self.handle);
        }
    }
}

impl LlamaBatch {
    pub fn new(backend: Arc<LlamaLib>, n_tokens: i32, embd: i32, n_seq_max: i32) -> Self {
        let handle = unsafe { (backend.symbols.llama_batch_init)(n_tokens, embd, n_seq_max) };
        Self { backend, handle }
    }

    pub fn clear(&mut self) {
        self.handle.n_tokens = 0;
    }

    pub fn add(
        &mut self,
        token: llama_cpp_sys_v3::llama_token,
        pos: llama_cpp_sys_v3::llama_pos,
        seq_ids: &[i32],
        logits: bool,
    ) {
        let n = self.handle.n_tokens as usize;
        unsafe {
            *self.handle.token.add(n) = token;
            *self.handle.pos.add(n) = pos;
            *self.handle.n_seq_id.add(n) = seq_ids.len() as i32;
            for (j, &seq_id) in seq_ids.iter().enumerate() {
                *(*self.handle.seq_id.add(n)).add(j) = seq_id;
            }
            *self.handle.logits.add(n) = if logits { 1 } else { 0 };
        }
        self.handle.n_tokens += 1;
    }
}
