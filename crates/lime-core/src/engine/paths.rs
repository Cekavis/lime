use std::path::{Path, PathBuf};

pub const DEFAULT_RIME_SCHEMA: &str = "rime_ice";

pub(super) fn find_librime_dll(root: &Path) -> Option<PathBuf> {
    [
        root.join("rime.dll"),
        root.join("windows-x64").join("rime.dll"),
        root.join("bin").join("rime.dll"),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

pub(super) fn default_user_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("LIME_RIME_USER_DIR") {
        return PathBuf::from(path);
    }
    #[cfg(windows)]
    if let Some(path) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(path).join("Lime").join("rime-user");
    }
    #[cfg(not(windows))]
    if let Some(path) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(path).join("lime").join("rime-user");
    }
    std::env::temp_dir().join("lime-rime-user")
}

pub(super) fn configured_schema() -> String {
    std::env::var("LIME_RIME_SCHEMA")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_RIME_SCHEMA.to_owned())
}

pub(super) fn validate_schema(schema: &str) -> Result<(), String> {
    if schema.trim().is_empty() {
        return Err("Rime schema id must not be empty".to_owned());
    }
    if schema.contains('\0') {
        return Err("Rime schema id contains NUL".to_owned());
    }
    if schema.chars().count() > 128 {
        return Err("Rime schema id is too long".to_owned());
    }
    Ok(())
}
