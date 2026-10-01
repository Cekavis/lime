use std::{collections::BTreeMap, fs, io, io::Write, path::Path};

use lime_protocol::{Config, ModelPreset};

pub(crate) const CONFIG_FILE_VERSION: u32 = 1;
pub(crate) const MODEL_PRESETS_FILE_VERSION: u32 = 1;

#[derive(serde::Serialize)]
pub(crate) struct VersionedConfig<'a> {
    pub(crate) version: u32,
    pub(crate) config: &'a Config,
}

#[derive(serde::Deserialize)]
pub(crate) struct VersionedConfigOwned {
    pub(crate) version: u32,
    pub(crate) config: Config,
}

#[derive(serde::Deserialize)]
pub(crate) struct PersistedModelPresets {
    pub(crate) version: u32,
    pub(crate) presets: Vec<ModelPreset>,
    pub(crate) active_model_path: Option<String>,
}

#[derive(serde::Serialize)]
pub(crate) struct VersionedModelPresets<'a> {
    pub(crate) version: u32,
    pub(crate) presets: &'a [ModelPreset],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) active_model_path: Option<&'a str>,
}

#[derive(Default)]
pub(crate) struct LoadedModelPresets {
    pub(crate) presets: Vec<ModelPreset>,
    pub(crate) active_model_path: Option<String>,
}

pub(crate) fn persist_config(data_dir: Option<&Path>, config: &Config) -> Result<(), io::Error> {
    let Some(dir) = data_dir else {
        return Ok(());
    };
    let bytes = serde_json::to_vec_pretty(&VersionedConfig {
        version: CONFIG_FILE_VERSION,
        config,
    })
    .map_err(io::Error::other)?;
    atomic_write_file(dir, "config.json", bytes)
}

pub(crate) fn persist_model_state(
    data_dir: Option<&Path>,
    presets: &BTreeMap<String, ModelPreset>,
    active_model_path: Option<&str>,
) -> Result<(), io::Error> {
    let Some(dir) = data_dir else {
        return Ok(());
    };
    let values = presets.values().cloned().collect::<Vec<_>>();
    let bytes = serde_json::to_vec_pretty(&VersionedModelPresets {
        version: MODEL_PRESETS_FILE_VERSION,
        presets: &values,
        active_model_path,
    })
    .map_err(io::Error::other)?;
    atomic_write_file(dir, "model-presets.json", bytes)
}

pub(crate) fn atomic_write_file(
    dir: &Path,
    file_name: &str,
    bytes: Vec<u8>,
) -> Result<(), io::Error> {
    fs::create_dir_all(dir)?;
    let target = dir.join(file_name);
    let temp = dir.join(format!("{file_name}.tmp"));
    let mut file = fs::File::create(&temp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp, &target)
}

pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(crate) fn load_config(path: &Path) -> Option<Config> {
    let bytes = fs::read(path.join("config.json")).ok()?;
    let persisted: VersionedConfigOwned = serde_json::from_slice(&bytes).ok()?;
    (persisted.version == CONFIG_FILE_VERSION).then_some(persisted.config)
}

pub(crate) fn load_model_state(path: &Path) -> Option<LoadedModelPresets> {
    let bytes = fs::read(path.join("model-presets.json")).ok()?;
    let persisted: PersistedModelPresets = serde_json::from_slice(&bytes).ok()?;
    if persisted.version != MODEL_PRESETS_FILE_VERSION {
        return None;
    }
    Some(LoadedModelPresets {
        active_model_path: persisted.active_model_path.filter(|path| !path.is_empty()),
        presets: persisted.presets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn atomic_write_replaces_existing_file_without_leftover_temporary_file() {
        let root = std::env::temp_dir().join(format!(
            "lime-atomic-write-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        atomic_write_file(&root, "index.json", b"first".to_vec()).unwrap();
        atomic_write_file(&root, "index.json", b"second".to_vec()).unwrap();
        assert_eq!(fs::read(root.join("index.json")).unwrap(), b"second");
        assert!(!root.join("index.json.tmp").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
