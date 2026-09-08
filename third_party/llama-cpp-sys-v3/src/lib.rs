use libloading::Library;
use std::{
    ffi::c_char,
    path::{Path, PathBuf},
};

pub mod types;
pub use types::*;

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("DLL not found: {0}")]
    NotFound(PathBuf),
    #[error("Failed to load DLL: {0}")]
    LoadFailed(#[from] libloading::Error),
    #[error("Symbol not found: {0}")]
    SymbolMissing(&'static str),
}

/// A loaded instance of the llama.cpp dynamic library.
/// This struct holds the library handle and all resolved function pointers.
pub struct LlamaLib {
    // We must keep the libraries alive as long as the functions are used.
    _libs: Vec<Library>,
    pub symbols: LlamaSymbols,
}

fn resolve_optional<T: Copy>(libs: &[Library], name: &'static [u8]) -> Option<T> {
    libs.iter()
        .find_map(|lib| unsafe { lib.get::<T>(name).ok().map(|symbol| *symbol) })
}

#[cfg(target_os = "windows")]
fn load_library(path: &Path) -> Result<Library, libloading::Error> {
    let library = unsafe {
        libloading::os::windows::Library::load_with_flags(
            path,
            libloading::os::windows::LOAD_WITH_ALTERED_SEARCH_PATH,
        )?
    };
    Ok(Library::from(library))
}

#[cfg(not(target_os = "windows"))]
fn load_library(path: &Path) -> Result<Library, libloading::Error> {
    unsafe { Library::new(path) }
}

/// Files that may be needed by the CUDA ggml backend.  The names are matched
/// case-insensitively and may include a version suffix (for example
/// `cudart64_13.dll` or `cublasLt64_12.dll`).
fn is_cuda_runtime_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "ggml-cuda.dll"
        || lower.starts_with("ggml-cuda-")
        || lower.starts_with("cudart")
        || lower.starts_with("cublas")
        || lower.starts_with("cusolver")
        || lower.starts_with("cusparse")
        || lower.starts_with("cufft")
        || lower.starts_with("curand")
        || lower.starts_with("nvrtc")
        || lower.starts_with("nvjitlink")
        || lower.starts_with("npp")
        || lower.starts_with("nvblas")
}

/// Best-effort preload of CUDA's plugin and its colocated dependencies.
///
/// `ggml_backend_load_all_from_path` remains the source of truth for backend
/// registration. Preloading here gives Windows' loader an explicit search path
/// for CUDA 12/13 DLLs and keeps those handles alive for the lifetime of the
/// llama library. Missing/incompatible CUDA files are intentionally ignored so
/// CPU-only installations continue to work and can be selected by the caller.
fn preload_cuda_runtime(parent: &Path, libs: &mut Vec<Library>) {
    let mut paths = std::fs::read_dir(parent)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("dll"))
        })
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_cuda_runtime_file)
        })
        .collect::<Vec<_>>();
    paths.sort_by(|left, right| {
        left.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase()
            .cmp(
                &right
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_ascii_lowercase(),
            )
    });

    for path in paths {
        // A second LoadLibrary call is harmless on Windows and lets ggml's
        // plugin registry resolve the same module by its canonical filename.
        if let Ok(library) = load_library(&path) {
            libs.push(library);
        }
    }
}

fn c_string_to_string(pointer: *const c_char) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    Some(
        unsafe { std::ffi::CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned(),
    )
}

fn c_string_contains_ignore_ascii_case(pointer: *const c_char, needle: &str) -> bool {
    c_string_to_string(pointer).is_some_and(|value| value.to_ascii_lowercase().contains(needle))
}

macro_rules! resolve_symbols {
    ($libs:expr, { $( $name:ident : $type:ty ),* $(,)? }) => {
        LlamaSymbols {
            device_count: None,
            device_get: None,
            device_type: None,
            device_name: None,
            device_backend_reg: None,
            backend_reg_name: None,
            llama_log_set: None,
            llama_log_get: None,
            $(
                $name: {
                    let mut found = None;
                    for lib in $libs.iter() {
                        if let Ok(sym) = unsafe { lib.get::<$type>(stringify!($name).as_bytes()) } {
                            found = Some(*sym);
                            break;
                        }
                    }
                    found.ok_or(LoadError::SymbolMissing(stringify!($name)))?
                },
            )*
        }
    };
}

impl LlamaLib {
    /// Attempt to load the llama.cpp library from the given path.
    pub fn open(path: &Path) -> Result<Self, LoadError> {
        if !path.exists() {
            return Err(LoadError::NotFound(path.to_path_buf()));
        }

        let mut libs = Vec::new();

        // Load the shared ggml core first. The CUDA plugin depends on this
        // module and Windows' altered-search-path flag then resolves colocated
        // dependencies deterministically.
        if let Some(parent) = path.parent() {
            let ggml_base_path = parent.join("ggml-base.dll");
            if ggml_base_path.exists() {
                if let Ok(lib) = load_library(&ggml_base_path) {
                    libs.push(lib);
                }
            }

            let ggml_path = parent.join("ggml.dll");
            if ggml_path.exists() {
                libs.push(load_library(&ggml_path)?);
            }

            // Keep CUDA and its versioned toolkit DLLs alive before asking
            // ggml to discover dynamic backends. This is best effort: a CPU
            // runtime simply has no matching files and remains valid.
            preload_cuda_runtime(parent, &mut libs);
        }

        libs.push(load_library(path)?);

        // Resolve all required symbols here
        let mut symbols = resolve_symbols!(libs, {
            llama_backend_init: unsafe extern "C" fn(),
            llama_backend_free: unsafe extern "C" fn(),
            ggml_backend_load_all: unsafe extern "C" fn(),
            ggml_backend_load_all_from_path: unsafe extern "C" fn(*const std::ffi::c_char),

            llama_model_default_params: unsafe extern "C" fn() -> llama_model_params,
            llama_model_load_from_file: unsafe extern "C" fn(*const std::ffi::c_char, llama_model_params) -> *mut llama_model,
            llama_model_free: unsafe extern "C" fn(*mut llama_model),

            llama_context_default_params: unsafe extern "C" fn() -> llama_context_params,
            llama_init_from_model: unsafe extern "C" fn(*mut llama_model, llama_context_params) -> *mut llama_context,
            llama_free: unsafe extern "C" fn(*mut llama_context),

            llama_batch_get_one: unsafe extern "C" fn(*mut llama_token, i32) -> llama_batch,
            llama_batch_init: unsafe extern "C" fn(i32, i32, i32) -> llama_batch,
            llama_batch_free: unsafe extern "C" fn(llama_batch),

            llama_decode: unsafe extern "C" fn(*mut llama_context, llama_batch) -> i32,
            llama_get_memory: unsafe extern "C" fn(*const llama_context) -> *mut llama_memory,
            llama_memory_clear: unsafe extern "C" fn(*mut llama_memory, bool),
            llama_memory_seq_rm: unsafe extern "C" fn(*mut llama_memory, llama_seq_id, llama_pos, llama_pos) -> bool,

            llama_set_n_threads: unsafe extern "C" fn(*mut llama_context, u32, u32),
            llama_model_get_vocab: unsafe extern "C" fn(*const llama_model) -> *const llama_vocab,
            llama_vocab_n_tokens: unsafe extern "C" fn(*const llama_vocab) -> i32,
            llama_n_vocab: unsafe extern "C" fn(*const llama_vocab) -> i32,
            llama_n_ctx: unsafe extern "C" fn(*const llama_context) -> u32,

            llama_get_logits: unsafe extern "C" fn(*mut llama_context) -> *mut f32,
            llama_get_logits_ith: unsafe extern "C" fn(*mut llama_context, i32) -> *mut f32,

            llama_token_get_text: unsafe extern "C" fn(*const llama_vocab, llama_token) -> *const std::ffi::c_char,
            llama_tokenize: unsafe extern "C" fn(*const llama_vocab, *const std::ffi::c_char, i32, *mut llama_token, i32, bool, bool) -> i32,
            llama_token_to_piece: unsafe extern "C" fn(*const llama_vocab, llama_token, *mut std::ffi::c_char, i32, i32, bool) -> i32,

            llama_vocab_bos: unsafe extern "C" fn(*const llama_vocab) -> llama_token,
            llama_vocab_eos: unsafe extern "C" fn(*const llama_vocab) -> llama_token,
            llama_vocab_nl: unsafe extern "C" fn(*const llama_vocab) -> llama_token,
            llama_vocab_is_eog: unsafe extern "C" fn(*const llama_vocab, llama_token) -> bool,

            llama_print_system_info: unsafe extern "C" fn() -> *const std::ffi::c_char,

            // Sampler API
            llama_sampler_chain_init: unsafe extern "C" fn(llama_sampler_chain_params) -> *mut llama_sampler,
            llama_sampler_chain_default_params: unsafe extern "C" fn() -> llama_sampler_chain_params,
            llama_sampler_init: unsafe extern "C" fn(*mut llama_sampler_i, *mut std::ffi::c_void) -> *mut llama_sampler,
            llama_set_sampler: unsafe extern "C" fn(*mut llama_context, llama_seq_id, *mut llama_sampler) -> bool,
            llama_synchronize: unsafe extern "C" fn(*mut llama_context),
            llama_sampler_init_greedy: unsafe extern "C" fn() -> *mut llama_sampler,
            llama_sampler_free: unsafe extern "C" fn(*mut llama_sampler),
            llama_sampler_init_temp: unsafe extern "C" fn(f32) -> *mut llama_sampler,
            llama_sampler_init_top_k: unsafe extern "C" fn(i32) -> *mut llama_sampler,
            llama_sampler_init_top_p: unsafe extern "C" fn(f32, usize) -> *mut llama_sampler,
            llama_sampler_init_dist: unsafe extern "C" fn(u32) -> *mut llama_sampler,
            llama_sampler_init_min_p: unsafe extern "C" fn(f32, usize) -> *mut llama_sampler,
            llama_sampler_init_typical: unsafe extern "C" fn(f32, usize) -> *mut llama_sampler,
            llama_sampler_init_mirostat_v2: unsafe extern "C" fn(u32, f32, f32) -> *mut llama_sampler,
            llama_sampler_init_penalties: unsafe extern "C" fn(i32, f32, f32, f32) -> *mut llama_sampler,
            llama_sampler_chain_add: unsafe extern "C" fn(*mut llama_sampler, *mut llama_sampler),
            llama_sampler_sample: unsafe extern "C" fn(*mut llama_sampler, *mut llama_context, i32) -> llama_token,
            llama_sampler_accept: unsafe extern "C" fn(*mut llama_sampler, llama_token),
            llama_chat_apply_template: unsafe extern "C" fn(*const std::ffi::c_char, *const llama_chat_message, usize, bool, *mut std::ffi::c_char, i32) -> i32,
            llama_model_chat_template: unsafe extern "C" fn(*const llama_model, *const std::ffi::c_char, *mut std::ffi::c_char, usize) -> i32,
            ggml_reshape_1d: unsafe extern "C" fn(*mut ggml_context, *mut ggml_tensor, i64) -> *mut ggml_tensor,
            ggml_reshape_2d: unsafe extern "C" fn(*mut ggml_context, *mut ggml_tensor, i64, i64) -> *mut ggml_tensor,
            ggml_soft_max: unsafe extern "C" fn(*mut ggml_context, *mut ggml_tensor) -> *mut ggml_tensor,
            ggml_get_rows: unsafe extern "C" fn(*mut ggml_context, *mut ggml_tensor, *mut ggml_tensor) -> *mut ggml_tensor,
            ggml_log: unsafe extern "C" fn(*mut ggml_context, *mut ggml_tensor) -> *mut ggml_tensor,
            ggml_new_tensor_1d: unsafe extern "C" fn(*mut ggml_context, i32, i64) -> *mut ggml_tensor,
            ggml_set_input: unsafe extern "C" fn(*mut ggml_tensor),
            ggml_get_data: unsafe extern "C" fn(*const ggml_tensor) -> *mut std::ffi::c_void,
            ggml_nelements: unsafe extern "C" fn(*const ggml_tensor) -> i64,
            ggml_backend_tensor_get: unsafe extern "C" fn(*mut ggml_tensor, *mut std::ffi::c_void, usize, usize),
        });

        // Backend capability symbols were added to the dynamic backend API
        // after the original Lime wrapper was written. Resolve them
        // optionally so older CPU-only runtimes still load; when unavailable,
        // the higher-level wrapper simply treats CUDA as unsupported.
        let device_count =
            resolve_optional::<unsafe extern "C" fn() -> usize>(&libs, b"ggml_backend_dev_count\0");
        let device_get = resolve_optional::<unsafe extern "C" fn(usize) -> *mut ggml_backend_device>(
            &libs,
            b"ggml_backend_dev_get\0",
        );
        let device_type = resolve_optional::<unsafe extern "C" fn(*mut ggml_backend_device) -> i32>(
            &libs,
            b"ggml_backend_dev_type\0",
        );
        let device_name = resolve_optional::<
            unsafe extern "C" fn(*mut ggml_backend_device) -> *const c_char,
        >(&libs, b"ggml_backend_dev_name\0");
        let device_backend_reg = resolve_optional::<
            unsafe extern "C" fn(*mut ggml_backend_device) -> *mut ggml_backend_reg,
        >(&libs, b"ggml_backend_dev_backend_reg\0");
        let backend_reg_name = resolve_optional::<
            unsafe extern "C" fn(*mut ggml_backend_reg) -> *const c_char,
        >(&libs, b"ggml_backend_reg_name\0");

        symbols.device_count = device_count;
        symbols.device_get = device_get;
        symbols.device_type = device_type;
        symbols.device_name = device_name;
        symbols.device_backend_reg = device_backend_reg;
        symbols.backend_reg_name = backend_reg_name;
        symbols.llama_log_set = resolve_optional::<
            unsafe extern "C" fn(ggml_log_callback, *mut std::ffi::c_void),
        >(&libs, b"llama_log_set\0");
        symbols.llama_log_get = resolve_optional::<
            unsafe extern "C" fn(*mut ggml_log_callback, *mut *mut std::ffi::c_void),
        >(&libs, b"llama_log_get\0");

        Ok(Self {
            _libs: libs,
            symbols,
        })
    }

    /// Return whether a CUDA backend device is registered by ggml.
    ///
    /// This must be called after `ggml_backend_load_all[_from_path]`; before
    /// that call the dynamic plugins have not been registered yet.
    pub fn has_cuda_device(&self) -> bool {
        let (Some(count), Some(get), Some(device_type), Some(device_backend_reg), Some(reg_name)) = (
            self.symbols.device_count,
            self.symbols.device_get,
            self.symbols.device_type,
            self.symbols.device_backend_reg,
            self.symbols.backend_reg_name,
        ) else {
            return false;
        };
        let Some(device_name) = self.symbols.device_name else {
            return false;
        };

        let count = unsafe { count() };
        (0..count).any(|index| {
            let device = unsafe { get(index) };
            if device.is_null() || unsafe { device_type(device) } != GGML_BACKEND_DEVICE_TYPE_GPU {
                return false;
            }
            let registration = unsafe { device_backend_reg(device) };
            if registration.is_null() {
                return false;
            }
            let registration_name = unsafe { reg_name(registration) };
            let device_name = unsafe { device_name(device) };
            c_string_contains_ignore_ascii_case(registration_name, "cuda")
                || c_string_contains_ignore_ascii_case(device_name, "cuda")
                || c_string_contains_ignore_ascii_case(device_name, "nvidia")
        })
    }

    /// Return concise names for all registered backend devices. This is useful
    /// for diagnostics and lets callers report the actual runtime capabilities.
    pub fn device_names(&self) -> Vec<String> {
        let (Some(count), Some(get), Some(name)) = (
            self.symbols.device_count,
            self.symbols.device_get,
            self.symbols.device_name,
        ) else {
            return Vec::new();
        };
        let count = unsafe { count() };
        (0..count)
            .filter_map(|index| {
                let device = unsafe { get(index) };
                if device.is_null() {
                    return None;
                }
                let pointer = unsafe { name(device) };
                c_string_to_string(pointer)
            })
            .collect()
    }
}

pub struct LlamaSymbols {
    pub llama_backend_init: unsafe extern "C" fn(),
    pub llama_backend_free: unsafe extern "C" fn(),
    pub ggml_backend_load_all: unsafe extern "C" fn(),
    pub ggml_backend_load_all_from_path: unsafe extern "C" fn(*const std::ffi::c_char),

    /// Optional dynamic-backend registry APIs. Older llama.cpp builds may not
    /// export these; callers must handle `None` as "CUDA capability unknown".
    pub device_count: Option<unsafe extern "C" fn() -> usize>,
    pub device_get: Option<unsafe extern "C" fn(usize) -> *mut ggml_backend_device>,
    pub device_type: Option<unsafe extern "C" fn(*mut ggml_backend_device) -> i32>,
    pub device_name: Option<unsafe extern "C" fn(*mut ggml_backend_device) -> *const c_char>,
    pub device_backend_reg:
        Option<unsafe extern "C" fn(*mut ggml_backend_device) -> *mut ggml_backend_reg>,
    pub backend_reg_name: Option<unsafe extern "C" fn(*mut ggml_backend_reg) -> *const c_char>,

    /// Optional process-global logging hooks. They were added to the public
    /// llama.cpp API after Lime's original dynamic wrapper was written. The
    /// higher-level wrapper uses them opportunistically to capture the memory
    /// figures emitted during model/context initialization.
    pub llama_log_set: Option<unsafe extern "C" fn(ggml_log_callback, *mut std::ffi::c_void)>,
    pub llama_log_get:
        Option<unsafe extern "C" fn(*mut ggml_log_callback, *mut *mut std::ffi::c_void)>,

    pub llama_model_default_params: unsafe extern "C" fn() -> llama_model_params,
    pub llama_model_load_from_file:
        unsafe extern "C" fn(*const std::ffi::c_char, llama_model_params) -> *mut llama_model,
    pub llama_model_free: unsafe extern "C" fn(*mut llama_model),

    pub llama_context_default_params: unsafe extern "C" fn() -> llama_context_params,
    pub llama_init_from_model:
        unsafe extern "C" fn(*mut llama_model, llama_context_params) -> *mut llama_context,
    pub llama_free: unsafe extern "C" fn(*mut llama_context),

    pub llama_batch_get_one: unsafe extern "C" fn(*mut llama_token, i32) -> llama_batch,
    pub llama_batch_init: unsafe extern "C" fn(i32, i32, i32) -> llama_batch,
    pub llama_batch_free: unsafe extern "C" fn(llama_batch),

    pub llama_decode: unsafe extern "C" fn(*mut llama_context, llama_batch) -> i32,
    pub llama_get_memory: unsafe extern "C" fn(*const llama_context) -> *mut llama_memory,
    pub llama_memory_clear: unsafe extern "C" fn(*mut llama_memory, bool),
    pub llama_memory_seq_rm:
        unsafe extern "C" fn(*mut llama_memory, llama_seq_id, llama_pos, llama_pos) -> bool,

    pub llama_set_n_threads: unsafe extern "C" fn(*mut llama_context, u32, u32),
    pub llama_model_get_vocab: unsafe extern "C" fn(*const llama_model) -> *const llama_vocab,
    pub llama_vocab_n_tokens: unsafe extern "C" fn(*const llama_vocab) -> i32,
    pub llama_n_vocab: unsafe extern "C" fn(*const llama_vocab) -> i32,
    pub llama_n_ctx: unsafe extern "C" fn(*const llama_context) -> u32,

    pub llama_get_logits: unsafe extern "C" fn(*mut llama_context) -> *mut f32,
    pub llama_get_logits_ith: unsafe extern "C" fn(*mut llama_context, i32) -> *mut f32,

    pub llama_token_get_text:
        unsafe extern "C" fn(*const llama_vocab, llama_token) -> *const std::ffi::c_char,
    pub llama_tokenize: unsafe extern "C" fn(
        *const llama_vocab,
        *const std::ffi::c_char,
        i32,
        *mut llama_token,
        i32,
        bool,
        bool,
    ) -> i32,
    pub llama_token_to_piece: unsafe extern "C" fn(
        *const llama_vocab,
        llama_token,
        *mut std::ffi::c_char,
        i32,
        i32,
        bool,
    ) -> i32,

    pub llama_vocab_bos: unsafe extern "C" fn(*const llama_vocab) -> llama_token,
    pub llama_vocab_eos: unsafe extern "C" fn(*const llama_vocab) -> llama_token,
    pub llama_vocab_nl: unsafe extern "C" fn(*const llama_vocab) -> llama_token,
    pub llama_vocab_is_eog: unsafe extern "C" fn(*const llama_vocab, llama_token) -> bool,

    pub llama_print_system_info: unsafe extern "C" fn() -> *const std::ffi::c_char,

    pub llama_sampler_chain_init:
        unsafe extern "C" fn(llama_sampler_chain_params) -> *mut llama_sampler,
    pub llama_sampler_chain_default_params: unsafe extern "C" fn() -> llama_sampler_chain_params,
    pub llama_sampler_init:
        unsafe extern "C" fn(*mut llama_sampler_i, *mut std::ffi::c_void) -> *mut llama_sampler,
    pub llama_set_sampler:
        unsafe extern "C" fn(*mut llama_context, llama_seq_id, *mut llama_sampler) -> bool,
    pub llama_synchronize: unsafe extern "C" fn(*mut llama_context),
    pub llama_sampler_init_greedy: unsafe extern "C" fn() -> *mut llama_sampler,
    pub llama_sampler_free: unsafe extern "C" fn(*mut llama_sampler),
    pub llama_sampler_init_temp: unsafe extern "C" fn(f32) -> *mut llama_sampler,
    pub llama_sampler_init_top_k: unsafe extern "C" fn(i32) -> *mut llama_sampler,
    pub llama_sampler_init_top_p: unsafe extern "C" fn(f32, usize) -> *mut llama_sampler,
    pub llama_sampler_init_dist: unsafe extern "C" fn(u32) -> *mut llama_sampler,
    pub llama_sampler_init_min_p: unsafe extern "C" fn(f32, usize) -> *mut llama_sampler,
    pub llama_sampler_init_typical: unsafe extern "C" fn(f32, usize) -> *mut llama_sampler,
    pub llama_sampler_init_mirostat_v2: unsafe extern "C" fn(u32, f32, f32) -> *mut llama_sampler,
    pub llama_sampler_init_penalties:
        unsafe extern "C" fn(i32, f32, f32, f32) -> *mut llama_sampler,
    pub llama_sampler_chain_add: unsafe extern "C" fn(*mut llama_sampler, *mut llama_sampler),
    pub llama_sampler_sample:
        unsafe extern "C" fn(*mut llama_sampler, *mut llama_context, i32) -> llama_token,
    pub llama_sampler_accept: unsafe extern "C" fn(*mut llama_sampler, llama_token),
    pub llama_chat_apply_template: unsafe extern "C" fn(
        *const std::ffi::c_char,
        *const llama_chat_message,
        usize,
        bool,
        *mut std::ffi::c_char,
        i32,
    ) -> i32,
    pub llama_model_chat_template: unsafe extern "C" fn(
        *const llama_model,
        *const std::ffi::c_char,
        *mut std::ffi::c_char,
        usize,
    ) -> i32,

    pub ggml_reshape_1d:
        unsafe extern "C" fn(*mut ggml_context, *mut ggml_tensor, i64) -> *mut ggml_tensor,
    pub ggml_reshape_2d:
        unsafe extern "C" fn(*mut ggml_context, *mut ggml_tensor, i64, i64) -> *mut ggml_tensor,
    pub ggml_soft_max:
        unsafe extern "C" fn(*mut ggml_context, *mut ggml_tensor) -> *mut ggml_tensor,
    pub ggml_get_rows: unsafe extern "C" fn(
        *mut ggml_context,
        *mut ggml_tensor,
        *mut ggml_tensor,
    ) -> *mut ggml_tensor,
    pub ggml_log: unsafe extern "C" fn(*mut ggml_context, *mut ggml_tensor) -> *mut ggml_tensor,
    pub ggml_new_tensor_1d: unsafe extern "C" fn(*mut ggml_context, i32, i64) -> *mut ggml_tensor,
    pub ggml_set_input: unsafe extern "C" fn(*mut ggml_tensor),
    pub ggml_get_data: unsafe extern "C" fn(*const ggml_tensor) -> *mut std::ffi::c_void,
    pub ggml_nelements: unsafe extern "C" fn(*const ggml_tensor) -> i64,
    pub ggml_backend_tensor_get:
        unsafe extern "C" fn(*mut ggml_tensor, *mut std::ffi::c_void, usize, usize),
}
