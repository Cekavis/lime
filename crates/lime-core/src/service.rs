use crate::{
    config::ConfigStore,
    engine::{CandidateEngine, RimeEngine},
    logging::PrivacyLogger,
    ranking::{rerank_candidates, GenerationTracker, LlamaRuntime},
};
use lime_protocol::{
    ConfigSnapshot, DictionaryEntry, ErrorCode, InputRequest, InputResponse, ModelInfo, Request,
    Response, ServiceState, ServiceStatus,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const CONFIG_FILE_VERSION: u32 = 1;

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum PersistedConfig {
    Versioned {
        version: u32,
        config: lime_protocol::Config,
    },
    Legacy(lime_protocol::Config),
}

#[derive(serde::Serialize)]
struct VersionedConfig<'a> {
    version: u32,
    config: &'a lime_protocol::Config,
}

#[derive(Clone)]
pub struct CoreService {
    config: Arc<Mutex<ConfigStore>>,
    engine: Arc<Mutex<RimeEngine>>,
    model: Arc<Mutex<Option<LlamaRuntime>>>,
    generation: Arc<GenerationTracker>,
    data_dir: Option<PathBuf>,
    logger: Arc<PrivacyLogger>,
}

impl Default for CoreService {
    fn default() -> Self {
        Self::new(None)
    }
}

impl CoreService {
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        let mut engine = RimeEngine::new();
        if let Some(dir) = &data_dir {
            let _ = fs::create_dir_all(dir);
            if let Ok(bytes) = fs::read(dir.join("dictionary.json")) {
                if let Ok(entries) = serde_json::from_slice::<Vec<DictionaryEntry>>(&bytes) {
                    let _ = engine.import_dictionary(&entries);
                }
            }
        }
        let config = data_dir
            .as_deref()
            .and_then(load_config)
            .and_then(|value| ConfigStore::from_config(value).ok())
            .unwrap_or_default();
        Self {
            config: Arc::new(Mutex::new(config)),
            engine: Arc::new(Mutex::new(engine)),
            model: Arc::new(Mutex::new(None)),
            generation: Arc::new(GenerationTracker::default()),
            data_dir: data_dir.clone(),
            logger: Arc::new(PrivacyLogger::new(data_dir.clone(), false)),
        }
    }

    pub fn config_snapshot(&self) -> ConfigSnapshot {
        self.config
            .lock()
            .expect("config mutex poisoned")
            .snapshot()
    }

    pub fn handle(&self, request: Request) -> Response {
        match request {
            Request::Handshake(handshake) => {
                if handshake.protocol_version == lime_protocol::PROTOCOL_VERSION {
                    Response::Handshake(lime_protocol::HandshakeResponse::accepted())
                } else {
                    Response::Handshake(lime_protocol::HandshakeResponse::rejected(
                        ErrorCode::ProtocolVersionMismatch,
                    ))
                }
            }
            Request::Input(input) => self
                .input(input)
                .map(Response::Input)
                .unwrap_or_else(|code| Response::Error { code }),
            Request::GetConfig => Response::Config(self.config_snapshot()),
            Request::SetConfig(config) => {
                let result = {
                    let mut store = self.config.lock().expect("config mutex poisoned");
                    let before = store.snapshot();
                    match store.replace(config) {
                        Ok(snapshot) => {
                            if self.persist_config(&snapshot.config).is_ok() {
                                Ok(snapshot)
                            } else {
                                store.restore(before);
                                Err(ErrorCode::Internal)
                            }
                        }
                        Err(_) => Err(ErrorCode::ConfigValidationFailed),
                    }
                };
                match result {
                    Ok(snapshot) => Response::Config(snapshot),
                    Err(ErrorCode::ConfigValidationFailed) => {
                        self.logger.event(
                            "config_validation_failed",
                            Some(ErrorCode::ConfigValidationFailed),
                        );
                        Response::Error {
                            code: ErrorCode::ConfigValidationFailed,
                        }
                    }
                    Err(code) => {
                        self.logger.event("config_persist_failed", Some(code));
                        Response::Error { code }
                    }
                }
            }
            Request::GetStatus => Response::Status(self.status()),
            Request::LoadModel { path } => self.load_model(Path::new(&path)),
            Request::UnloadModel => {
                *self.model.lock().expect("model mutex poisoned") = None;
                Response::Accepted
            }
            Request::Learn { pinyin, text } => self.learn(&pinyin, &text),
            Request::ExportDictionary => Response::Dictionary(
                self.engine
                    .lock()
                    .expect("engine mutex poisoned")
                    .export_dictionary(),
            ),
            Request::ImportDictionary { entries } => match self
                .engine
                .lock()
                .expect("engine mutex poisoned")
                .import_dictionary(&entries)
            {
                Ok(()) => {
                    let _ = self.persist_dictionary();
                    Response::Accepted
                }
                Err(_) => Response::Error {
                    code: ErrorCode::InvalidRequest,
                },
            },
            Request::ClearDictionary => {
                self.engine
                    .lock()
                    .expect("engine mutex poisoned")
                    .clear_dictionary();
                let _ = self.persist_dictionary();
                Response::Accepted
            }
        }
    }

    fn input(&self, request: InputRequest) -> Result<InputResponse, ErrorCode> {
        let snapshot = self.config_snapshot();
        if request.config_revision != snapshot.revision {
            return Err(ErrorCode::RequestCancelled);
        }
        let generation = self.generation.next();
        let mut candidates = self
            .engine
            .lock()
            .map_err(|_| ErrorCode::Internal)?
            .candidates(&request.preedit)
            .map_err(|_| ErrorCode::RimeInitializationFailed)?;
        let config = snapshot.config;
        let preceding_text = truncate_chars(
            &request.preceding_text,
            config.preceding_text_char_limit as usize,
        );
        let model = self.model.lock().map_err(|_| ErrorCode::Internal)?;
        let service_state = if model.is_some() {
            ServiceState::Ready
        } else {
            ServiceState::RimeOnly
        };
        if config.llm_enabled {
            candidates = rerank_candidates(
                &candidates,
                &preceding_text,
                model.as_ref(),
                config.llm_rerank_count as usize,
                config.llm_effective_count as usize,
            );
        }
        if !self.generation.is_current(generation) {
            return Err(ErrorCode::RequestCancelled);
        }
        Ok(InputResponse {
            request_id: request.request_id,
            candidates,
            context_used: request.context_available && !request.preceding_text.is_empty(),
            service_state,
        })
    }

    fn load_model(&self, path: &Path) -> Response {
        match LlamaRuntime::load(path.to_path_buf()) {
            Ok(model) => {
                *self.model.lock().expect("model mutex poisoned") = Some(model);
                Response::Accepted
            }
            Err(_) => {
                self.logger.event(
                    "model_load_failed",
                    Some(if path.exists() {
                        ErrorCode::ModelLoadFailed
                    } else {
                        ErrorCode::ModelNotFound
                    }),
                );
                Response::Error {
                    code: if path.exists() {
                        ErrorCode::ModelLoadFailed
                    } else {
                        ErrorCode::ModelNotFound
                    },
                }
            }
        }
    }
    fn learn(&self, pinyin: &str, text: &str) -> Response {
        match self
            .engine
            .lock()
            .expect("engine mutex poisoned")
            .learn(pinyin, text)
        {
            Ok(()) => {
                let _ = self.persist_dictionary();
                Response::Accepted
            }
            Err(error) => Response::Error { code: error.code },
        }
    }
    fn status(&self) -> ServiceStatus {
        let model = self.model.lock().expect("model mutex poisoned");
        ServiceStatus {
            state: if model.is_some() {
                ServiceState::Ready
            } else {
                ServiceState::RimeOnly
            },
            config: self.config_snapshot(),
            model: model
                .as_ref()
                .map(|item| ModelInfo {
                    path: Some(item.path.display().to_string()),
                    size_bytes: Some(item.size_bytes),
                    sha256: Some(item.sha256.clone()),
                    loaded: true,
                })
                .unwrap_or(ModelInfo {
                    path: None,
                    size_bytes: None,
                    sha256: None,
                    loaded: false,
                }),
        }
    }
    fn persist_dictionary(&self) -> Result<(), std::io::Error> {
        let Some(dir) = &self.data_dir else {
            return Ok(());
        };
        fs::create_dir_all(dir)?;
        let target = dir.join("dictionary.json");
        let temp = dir.join("dictionary.json.tmp");
        let bytes = serde_json::to_vec_pretty(
            &self
                .engine
                .lock()
                .expect("engine mutex poisoned")
                .export_dictionary(),
        )
        .map_err(std::io::Error::other)?;
        fs::write(&temp, bytes)?;
        fs::rename(temp, target)
    }

    fn persist_config(&self, config: &lime_protocol::Config) -> Result<(), std::io::Error> {
        let Some(dir) = &self.data_dir else {
            return Ok(());
        };
        fs::create_dir_all(dir)?;
        let target = dir.join("config.json");
        let temp = dir.join("config.json.tmp");
        let bytes = serde_json::to_vec_pretty(&VersionedConfig {
            version: CONFIG_FILE_VERSION,
            config,
        })
        .map_err(std::io::Error::other)?;
        fs::write(&temp, bytes)?;
        fs::rename(temp, target)
    }
}

fn load_config(path: &Path) -> Option<lime_protocol::Config> {
    let bytes = fs::read(path.join("config.json")).ok()?;
    match serde_json::from_slice::<PersistedConfig>(&bytes).ok()? {
        PersistedConfig::Versioned { version, config } if version == CONFIG_FILE_VERSION => {
            Some(config)
        }
        PersistedConfig::Versioned { .. } => None,
        PersistedConfig::Legacy(config) => Some(config),
    }
}

fn truncate_chars(value: &str, limit: usize) -> String {
    let count = value.chars().count();
    if count <= limit {
        return value.to_owned();
    }
    value.chars().skip(count - limit).collect()
}

pub mod framing {
    use std::io::{self, Read, Write};
    pub fn read_json<R: Read, T: serde::de::DeserializeOwned>(reader: &mut R) -> io::Result<T> {
        let mut len = [0; 4];
        reader.read_exact(&mut len)?;
        let length = u32::from_le_bytes(len) as usize;
        if length > 16 * 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "IPC frame too large",
            ));
        }
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes)?;
        serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
    pub fn write_json<W: Write, T: serde::Serialize>(writer: &mut W, value: &T) -> io::Result<()> {
        let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
        let length = u32::try_from(bytes.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "IPC frame too large"))?;
        writer.write_all(&length.to_le_bytes())?;
        writer.write_all(&bytes)?;
        writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lime_protocol::Config;

    #[test]
    fn service_starts_in_rime_only() {
        let service = CoreService::default();
        let response = service.handle(Request::Input(InputRequest {
            request_id: 1,
            preedit: "nihao".into(),
            preceding_text: String::new(),
            context_available: false,
            config_revision: 0,
        }));
        match response {
            Response::Input(value) => {
                assert_eq!(value.service_state, ServiceState::RimeOnly);
                assert!(!value.candidates.is_empty());
            }
            _ => panic!("unexpected response"),
        }
    }

    #[test]
    fn framing_round_trips_requests() {
        let request = Request::GetStatus;
        let mut bytes = Vec::new();
        framing::write_json(&mut bytes, &request).unwrap();
        let decoded: Request = framing::read_json(&mut bytes.as_slice()).unwrap();
        assert_eq!(decoded, request);
    }

    #[test]
    fn stale_config_revision_is_cancelled() {
        let service = CoreService::default();
        let config = Config {
            page_size: 10,
            ..Config::default()
        };
        assert!(matches!(
            service.handle(Request::SetConfig(config)),
            Response::Config(_)
        ));
        let response = service.handle(Request::Input(InputRequest {
            request_id: 2,
            preedit: "nihao".into(),
            preceding_text: String::new(),
            context_available: false,
            config_revision: 0,
        }));
        assert_eq!(
            response,
            Response::Error {
                code: ErrorCode::RequestCancelled
            }
        );
    }

    #[test]
    fn config_survives_restart_and_legacy_format_is_migrated() {
        let directory =
            std::env::temp_dir().join(format!("lime-core-config-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let service = CoreService::new(Some(directory.clone()));
        let config = Config {
            page_size: 12,
            ..Config::default()
        };
        assert!(matches!(
            service.handle(Request::SetConfig(config)),
            Response::Config(_)
        ));
        let restarted = CoreService::new(Some(directory.clone()));
        assert_eq!(restarted.config_snapshot().config.page_size, 12);

        fs::write(
            directory.join("config.json"),
            serde_json::to_vec(&Config {
                page_size: 7,
                ..Config::default()
            })
            .unwrap(),
        )
        .unwrap();
        let migrated = CoreService::new(Some(directory.clone()));
        assert_eq!(migrated.config_snapshot().config.page_size, 7);
        let _ = fs::remove_dir_all(directory);
    }
}
