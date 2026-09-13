use super::*;

pub(super) fn parse_initialization_memory(
    logs: &str,
    fallback_backend: &str,
) -> Option<InitializationMemory> {
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

pub(super) fn parse_mib_value(value: &str) -> Option<u64> {
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

pub(super) fn native_memory_backend(line: &str, marker_index: usize) -> Option<String> {
    let prefix = line.get(..marker_index)?;
    let token = prefix
        .rsplit(':')
        .next()
        .and_then(|segment| segment.split_whitespace().last())?
        .trim_matches(':');
    let lower = token.to_ascii_lowercase();
    (lower == "cpu" || lower.starts_with("cuda") || lower.starts_with("gpu")).then_some(lower)
}

pub(super) fn backend_name(backend: llama_cpp_v3::Backend) -> &'static str {
    match backend {
        llama_cpp_v3::Backend::Cuda => "cuda",
        llama_cpp_v3::Backend::Cpu => "cpu",
        llama_cpp_v3::Backend::Vulkan => "vulkan",
        llama_cpp_v3::Backend::Hip => "hip",
        llama_cpp_v3::Backend::Sycl => "sycl",
        llama_cpp_v3::Backend::OpenCl => "opencl",
    }
}

pub(super) fn platform_library_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["llama.dll", "llama-cpu.dll"]
    } else if cfg!(target_os = "macos") {
        &["libllama.dylib", "llama.dylib"]
    } else {
        &["libllama.so", "llama.so"]
    }
}

pub(super) fn resolve_runtime_library(path: &Path) -> Result<PathBuf, String> {
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
pub(super) fn resolve_runtime_libraries(
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

pub(super) fn preference_subdirectories(preference: BackendPreference) -> &'static [&'static str] {
    match preference {
        BackendPreference::Auto | BackendPreference::Cuda => &["cuda", "cpu"],
        BackendPreference::Cpu => &["cpu"],
    }
}

pub(super) fn discover_runtime_libraries(
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
