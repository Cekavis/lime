#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Cpu,
    Cuda,
    Vulkan,
    Hip,
    Sycl,
    OpenCl,
}

/// Backend selection policy used when opening a llama.cpp runtime.
///
/// `Auto` selects CUDA when available and otherwise CPU. `Cuda` is a strict
/// CUDA attempt at the wrapper boundary; Lime's higher-level loader retries
/// the dedicated CPU runtime when that attempt is unavailable. `Cpu` never
/// attempts GPU code. The low-level wrapper keeps `Auto` as its `Default` so
/// direct library callers remain CPU-capable; Lime's persisted service
/// configuration explicitly defaults to `cuda`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendPreference {
    #[default]
    Auto,
    Cuda,
    Cpu,
}

impl BackendPreference {
    /// Parse the optional `LIME_LLAMA_BACKEND` override.
    ///
    /// An unset variable means the default `Auto` policy. `cuda` requests a
    /// CUDA-first load and Lime will retry its CPU runtime if necessary.
    /// Invalid values are rejected so a typo cannot silently disable
    /// acceleration.
    pub fn from_env() -> Result<Self, String> {
        match std::env::var("LIME_LLAMA_BACKEND") {
            Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
                "" | "auto" | "default" => Ok(Self::Auto),
                "cuda" | "gpu" => Ok(Self::Cuda),
                "cpu" => Ok(Self::Cpu),
                other => Err(format!(
                    "invalid LIME_LLAMA_BACKEND={other:?}; expected auto, cuda, or cpu"
                )),
            },
            Err(std::env::VarError::NotPresent) => Ok(Self::Auto),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err("LIME_LLAMA_BACKEND is not valid UTF-8".to_owned())
            }
        }
    }

    pub const fn attempts(self) -> &'static [Backend] {
        match self {
            Self::Auto | Self::Cuda => &[Backend::Cuda, Backend::Cpu],
            Self::Cpu => &[Backend::Cpu],
        }
    }
}

impl Backend {
    /// The string used in the GitHub release filename (e.g. `llama-bXXXX-bin-win-vulkan-x64.zip`)
    pub fn release_name_component(&self) -> &'static str {
        match self {
            Backend::Cpu => "cpu",
            // Lime's pinned Windows default is the CUDA 13.3 b10743 asset. The release
            // preparation script still exposes an explicit CUDA-version override when needed.
            // The upstream archive uses `cuda-13.3` (without the historical `cu` infix).
            Backend::Cuda => "cuda-13.3",
            Backend::Vulkan => "vulkan",
            Backend::Hip => "sycl", // Needs to be mapped correctly, rocblas typically
            Backend::Sycl => "sycl",
            Backend::OpenCl => "opencl", // Not pre-built in standard releases but for completeness
        }
    }

    /// The name of the main DLL file
    pub fn dll_name(&self) -> &'static str {
        if cfg!(windows) {
            "llama.dll"
        } else if cfg!(target_os = "macos") {
            "libllama.dylib"
        } else {
            "libllama.so"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Backend;

    #[test]
    fn pinned_cuda_release_component_matches_upstream_asset() {
        assert_eq!(Backend::Cuda.release_name_component(), "cuda-13.3");
    }
}
