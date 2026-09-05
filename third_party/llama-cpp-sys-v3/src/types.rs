#![allow(non_camel_case_types)]

// No imports needed

pub type llama_pos = i32;
pub type llama_token = i32;
pub type llama_seq_id = i32;

/// The public llama.cpp logging callback uses the ggml log level enum.  C enums
/// have the platform `int` ABI, so keeping the level as `i32` avoids depending
/// on a private enum definition while preserving the callback calling
/// convention.
pub type ggml_log_level = i32;
pub type ggml_log_callback =
    Option<unsafe extern "C" fn(ggml_log_level, *const std::ffi::c_char, *mut std::ffi::c_void)>;

/// Opaque ggml backend registration/device handles.
///
/// The handles are owned by ggml and are only used for read-only capability
/// queries. Keeping them opaque avoids coupling Lime to backend-specific C++
/// layouts.
pub type ggml_backend_reg = std::ffi::c_void;
pub type ggml_backend_device = std::ffi::c_void;

/// Values from `enum ggml_backend_dev_type` in ggml-backend.h.
pub const GGML_BACKEND_DEVICE_TYPE_CPU: i32 = 0;
pub const GGML_BACKEND_DEVICE_TYPE_GPU: i32 = 1;
pub const GGML_BACKEND_DEVICE_TYPE_ACCEL: i32 = 2;
pub const GGML_BACKEND_DEVICE_TYPE_CPU_SCHED: i32 = 3;

pub const LLAMA_TOKEN_NULL: llama_token = -1;

// Opaque types
#[repr(C)]
pub struct llama_vocab {
    _data: [u8; 0],
    _marker: core::marker::PhantomData<(*mut u8, core::marker::PhantomPinned)>,
}

#[repr(C)]
pub struct llama_model {
    _data: [u8; 0],
    _marker: core::marker::PhantomData<(*mut u8, core::marker::PhantomPinned)>,
}

#[repr(C)]
pub struct llama_context {
    _data: [u8; 0],
    _marker: core::marker::PhantomData<(*mut u8, core::marker::PhantomPinned)>,
}

#[repr(C)]
pub struct llama_memory {
    _data: [u8; 0],
    _marker: core::marker::PhantomData<(*mut u8, core::marker::PhantomPinned)>,
}

#[repr(C)]
pub struct llama_sampler {
    _data: [u8; 0],
    _marker: core::marker::PhantomData<(*mut u8, core::marker::PhantomPinned)>,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct llama_chat_message {
    pub role: *const std::ffi::c_char,
    pub content: *const std::ffi::c_char,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct llama_batch {
    pub n_tokens: i32,
    pub token: *mut llama_token,
    pub embd: *mut f32,
    pub pos: *mut llama_pos,
    pub n_seq_id: *mut i32,
    pub seq_id: *mut *mut llama_seq_id,
    pub logits: *mut i8, // bool in C
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct llama_model_params {
    pub devices: *mut std::ffi::c_void,
    pub tensor_buft_overrides: *const std::ffi::c_void,
    pub n_gpu_layers: i32,
    pub split_mode: i32,
    pub load_mode: i32,
    pub tensor_read_lazy: i32,
    pub main_gpu: i32,
    pub tensor_split: *const f32,
    pub progress_callback: Option<unsafe extern "C" fn(f32, *mut std::ffi::c_void) -> bool>,
    pub progress_callback_user_data: *mut std::ffi::c_void,
    pub kv_overrides: *const std::ffi::c_void,
    pub vocab_only: bool,
    pub check_tensors: bool,
    pub use_extra_bufts: bool,
    pub no_host: bool,
    pub no_alloc: bool,
    pub load_mtp: bool,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct llama_context_params {
    pub n_ctx: u32,
    pub n_batch: u32,
    pub n_ubatch: u32,
    pub n_seq_max: u32,
    pub n_rs_seq: u32,
    pub n_outputs_max: u32,
    pub n_outputs_max_per_seq: u32,
    pub n_threads: i32,
    pub n_threads_batch: i32,
    pub ctx_type: i32,
    pub rope_scaling_type: i32,
    pub pooling_type: i32,
    pub attention_type: i32,
    pub flash_attn_type: i32,
    pub rope_freq_base: f32,
    pub rope_freq_scale: f32,
    pub yarn_ext_factor: f32,
    pub yarn_attn_factor: f32,
    pub yarn_beta_fast: f32,
    pub yarn_beta_slow: f32,
    pub yarn_orig_ctx: u32,
    pub defrag_thold: f32,
    pub cb_eval: Option<unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void) -> bool>,
    pub cb_eval_user_data: *mut std::ffi::c_void,
    pub type_k: i32,
    pub type_v: i32,
    pub abort_callback: Option<unsafe extern "C" fn(*mut std::ffi::c_void) -> bool>,
    pub abort_callback_data: *mut std::ffi::c_void,
    pub embeddings: bool,
    pub offload_kqv: bool,
    pub no_perf: bool,
    pub op_offload: bool,
    pub swa_full: bool,
    pub kv_unified: bool,
    pub samplers: *mut std::ffi::c_void,
    pub n_samplers: usize,
    pub ctx_other: *mut llama_context,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct llama_sampler_chain_params {
    pub no_perf: bool,
}
