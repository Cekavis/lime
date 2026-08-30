use lime_protocol::ErrorCode;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

/// Structured logger that never accepts input text, preedit, candidates or prompts.
#[derive(Debug)]
pub struct PrivacyLogger {
    path: Option<PathBuf>,
    debug: bool,
    lock: Mutex<()>,
}

impl PrivacyLogger {
    pub fn new(data_dir: Option<PathBuf>, debug: bool) -> Self {
        Self {
            path: data_dir.map(|dir| dir.join("lime.log")),
            debug,
            lock: Mutex::new(()),
        }
    }
    pub fn event(&self, event: &str, code: Option<ErrorCode>) {
        let Some(path) = &self.path else {
            return;
        };
        let _guard = self.lock.lock().ok();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if path
            .metadata()
            .map(|meta| meta.len() > 1_048_576)
            .unwrap_or(false)
        {
            let rotated = path.with_extension("log.1");
            let _ = fs::rename(path, rotated);
        }
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_secs())
            .unwrap_or_default();
        let record = serde_json::json!({ "ts": timestamp, "event": event, "error_code": code.map(|v| v.name()), "debug": self.debug });
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{record}");
        }
    }
}
