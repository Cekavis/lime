use super::*;

impl CoreService {
    pub(super) fn load_model(&self, path: &Path) -> Response {
        // Loading a native runtime mutates process-global llama.cpp state and
        // the Windows DLL search path. Serialize the whole load/replace
        // operation while leaving inference protected by the model mutex.
        let _load_guard = self.model_load.lock().expect("model load mutex poisoned");
        match self.load_runtime(path) {
            Ok(model) => {
                let loaded_path = model.path.clone();
                *self.model.lock().expect("model mutex poisoned") = Some(model);
                self.mark_loaded_preset(Some(&loaded_path));
                self.set_active_model_path(Some(&loaded_path));
                self.persist_model_state_best_effort("model_state_persist_failed");
                Response::Accepted
            }
            Err(code) => {
                self.logger.event("model_load_failed", Some(code));
                Response::Error { code }
            }
        }
    }

    pub(super) fn unload_model(&self) -> Response {
        let _load_guard = self.model_load.lock().expect("model load mutex poisoned");
        *self.model.lock().expect("model mutex poisoned") = None;
        self.mark_loaded_preset(None);
        self.set_active_model_path(None);
        self.persist_model_state_best_effort("model_state_persist_failed");
        Response::Accepted
    }

    fn load_runtime(&self, path: &Path) -> Result<LlamaRuntime, ErrorCode> {
        let (context_tokens, sequence_count, backend_preference) = self
            .config
            .lock()
            .map(|config| {
                let snapshot = config.snapshot();
                (
                    snapshot.config.llm_context_token_limit as usize,
                    snapshot.config.llm_rerank_count as usize,
                    backend_preference_for(&snapshot.config.llm_backend),
                )
            })
            .unwrap_or((
                crate::llama::DEFAULT_CONTEXT_TOKENS,
                crate::llama::DEFAULT_SEQUENCE_COUNT,
                BackendPreference::Cuda,
            ));
        LlamaRuntime::load_with_backend_preference_and_sequence_count(
            path.to_path_buf(),
            context_tokens,
            backend_preference,
            sequence_count,
        )
        .map_err(|error| {
            if error.contains("unsupported model for Lime ranking:") {
                return ErrorCode::ModelUnsupported;
            }
            if path.exists() {
                ErrorCode::ModelLoadFailed
            } else {
                ErrorCode::ModelNotFound
            }
        })
    }

    pub(super) fn restore_model_at_startup(&self, path: &Path) {
        // Startup restoration is deliberately best-effort.  Keep this operation
        // serialized with explicit model requests, but never let a failed load
        // prevent the service (and its Rime path) from coming up.
        let _load_guard = self.model_load.lock().expect("model load mutex poisoned");
        if !path.is_file() {
            self.logger
                .event("model_autoload_failed", Some(ErrorCode::ModelNotFound));
            return;
        }
        match self.load_runtime(path) {
            Ok(model) => {
                let loaded_path = model.path.clone();
                *self.model.lock().expect("model mutex poisoned") = Some(model);
                self.mark_loaded_preset(Some(&loaded_path));
                self.set_active_model_path(Some(&loaded_path));
            }
            Err(code) => {
                self.logger.event("model_autoload_failed", Some(code));
                eprintln!(
                    "Lime model auto-load failed for {}: {}",
                    path.display(),
                    code
                );
            }
        }
    }

    fn set_active_model_path(&self, path: Option<&Path>) {
        let value = path.map(normalize_path_string);
        *self
            .active_model_path
            .lock()
            .expect("active model mutex poisoned") = value;
    }

    fn persist_model_state_best_effort(&self, event: &'static str) {
        if let Err(error) = self.persist_model_state() {
            self.logger.event(event, Some(ErrorCode::Internal));
            eprintln!("Lime model state persistence failed: {error}");
        }
    }

    fn persist_model_state(&self) -> Result<(), std::io::Error> {
        let presets = self
            .model_presets
            .lock()
            .map_err(|_| std::io::Error::other("model presets mutex poisoned"))?;
        self.persist_model_presets_locked(&presets)
    }

    pub(super) fn model_presets(&self) -> Vec<ModelPreset> {
        let loaded_path = self
            .model
            .lock()
            .expect("model mutex poisoned")
            .as_ref()
            .map(|model| normalize_path_string(&model.path));
        let mut presets = self
            .model_presets
            .lock()
            .expect("model presets mutex poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for preset in &mut presets {
            preset.loaded = loaded_path
                .as_deref()
                .is_some_and(|path| path == preset.path);
        }
        presets
    }

    fn mark_loaded_preset(&self, path: Option<&Path>) {
        let normalized = path.map(normalize_path_string);
        let mut presets = self
            .model_presets
            .lock()
            .expect("model presets mutex poisoned");
        for preset in presets.values_mut() {
            preset.loaded = normalized
                .as_deref()
                .is_some_and(|value| value == preset.path);
        }
    }

    pub(super) fn save_model_preset(&self, name: &str, path: &Path) -> Response {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 128 || name.contains('\0') {
            return Response::Error {
                code: ErrorCode::InvalidRequest,
            };
        }
        let metadata = match LlamaRuntime::inspect_gguf(path.to_path_buf()) {
            Ok(metadata) => metadata,
            Err(_) => {
                let code = if path.exists() {
                    ErrorCode::ModelLoadFailed
                } else {
                    ErrorCode::ModelNotFound
                };
                self.logger.event("model_preset_save_failed", Some(code));
                return Response::Error { code };
            }
        };
        let normalized_path = normalize_path_string(path);
        let loaded = self
            .model
            .lock()
            .expect("model mutex poisoned")
            .as_ref()
            .map(|current| normalize_path_string(&current.path))
            .is_some_and(|current| current == normalized_path);
        let preset = ModelPreset {
            name: name.to_owned(),
            path: normalized_path,
            size_bytes: Some(metadata.size_bytes),
            sha256: Some(metadata.sha256),
            loaded,
        };
        let mut presets = self
            .model_presets
            .lock()
            .expect("model presets mutex poisoned");
        let previous = presets.insert(name.to_owned(), preset.clone());
        if self.persist_model_presets_locked(&presets).is_err() {
            if let Some(previous) = previous {
                presets.insert(name.to_owned(), previous);
            } else {
                presets.remove(name);
            }
            return Response::Error {
                code: ErrorCode::Internal,
            };
        }
        Response::ModelPreset(preset)
    }

    pub(super) fn delete_model_preset(&self, name: &str) -> Response {
        let name = name.trim();
        if name.is_empty() {
            return Response::Error {
                code: ErrorCode::InvalidRequest,
            };
        }
        let mut presets = self
            .model_presets
            .lock()
            .expect("model presets mutex poisoned");
        let Some(removed) = presets.remove(name) else {
            return Response::Error {
                code: ErrorCode::ModelNotFound,
            };
        };
        if self.persist_model_presets_locked(&presets).is_err() {
            presets.insert(name.to_owned(), removed);
            return Response::Error {
                code: ErrorCode::Internal,
            };
        }
        Response::Accepted
    }

    pub(super) fn rename_model_preset(&self, name: &str, new_name: &str) -> Response {
        let name = name.trim();
        let new_name = new_name.trim();
        if name.is_empty()
            || new_name.is_empty()
            || name.chars().count() > 128
            || new_name.chars().count() > 128
            || name.contains('\0')
            || new_name.contains('\0')
        {
            return Response::Error {
                code: ErrorCode::InvalidRequest,
            };
        }
        let mut presets = self
            .model_presets
            .lock()
            .expect("model presets mutex poisoned");
        let Some(preset) = presets.get(name).cloned() else {
            return Response::Error {
                code: ErrorCode::ModelNotFound,
            };
        };
        if name == new_name {
            return Response::ModelPreset(preset);
        }
        if presets.contains_key(new_name) {
            return Response::Error {
                code: ErrorCode::InvalidRequest,
            };
        }

        let mut renamed = preset.clone();
        renamed.name = new_name.to_owned();
        presets.remove(name);
        presets.insert(new_name.to_owned(), renamed.clone());
        if self.persist_model_presets_locked(&presets).is_err() {
            presets.remove(new_name);
            presets.insert(name.to_owned(), preset);
            return Response::Error {
                code: ErrorCode::Internal,
            };
        }
        Response::ModelPreset(renamed)
    }

    pub(super) fn select_model_preset(&self, name: &str) -> Response {
        let _load_guard = self.model_load.lock().expect("model load mutex poisoned");
        let name = name.trim();
        if name.is_empty() {
            return Response::Error {
                code: ErrorCode::InvalidRequest,
            };
        }
        let preset = {
            let presets = self
                .model_presets
                .lock()
                .expect("model presets mutex poisoned");
            presets.get(name).cloned()
        };
        let Some(preset) = preset else {
            return Response::Error {
                code: ErrorCode::ModelNotFound,
            };
        };
        match self.load_runtime(Path::new(&preset.path)) {
            Ok(model) => {
                let loaded_path = model.path.clone();
                *self.model.lock().expect("model mutex poisoned") = Some(model);
                self.mark_loaded_preset(Some(&loaded_path));
                self.set_active_model_path(Some(&loaded_path));
                self.persist_model_state_best_effort("model_state_persist_failed");
                Response::ModelPreset(
                    self.model_presets()
                        .into_iter()
                        .find(|item| item.name == name)
                        .unwrap_or(preset),
                )
            }
            Err(code) => {
                self.logger.event("model_preset_select_failed", Some(code));
                Response::Error { code }
            }
        }
    }
}
