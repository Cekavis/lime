use crate::CoreError;
use lime_protocol::{Candidate, DictionaryEntry, ErrorCode};
use std::path::{Path, PathBuf};

mod dictionary;
mod paths;
use dictionary::{parse_user_dictionary, RIME_ICE_USER_DICT};
pub use paths::DEFAULT_RIME_SCHEMA;
use paths::{configured_schema, default_user_dir, find_librime_dll, validate_schema};

pub trait CandidateEngine: Send {
    fn candidates(&mut self, preedit: &str) -> Result<Vec<Candidate>, CoreError>;
    fn learn(&mut self, pinyin: &str, text: &str) -> Result<(), CoreError>;
    fn export_dictionary(&mut self) -> Result<Vec<DictionaryEntry>, CoreError>;
    fn import_dictionary(&mut self, entries: &[DictionaryEntry]) -> Result<(), CoreError>;
    fn clear_dictionary(&mut self) -> Result<(), CoreError>;
}

/// Native candidates together with the subset that consumes the complete composition.
///
/// The coverage metadata stays inside the Rust core. Platform clients continue to receive the
/// stable public [`Candidate`] shape containing only display and commit text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CandidateBatch {
    pub candidates: Vec<Candidate>,
    pub complete_candidate_indices: Vec<usize>,
    /// The raw input suffix left after selecting each candidate. `None` means librime could not
    /// expose a reliable preview for that row; `Some("")` means the candidate consumes all input.
    pub candidate_remainders: Vec<Option<String>>,
}

/// Result returned after librime processes one native key event.
///
/// `handled` is librime's `RimeProcessKey` result. Any unread commit is returned together
/// with the current native candidate list so a stateful platform adapter can use Rime's
/// processors, recognizer, and key binder without constructing candidates itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RimeKeyResult {
    pub handled: bool,
    pub commit_text: Option<String>,
    pub candidates: Vec<Candidate>,
}

#[derive(Debug)]
pub struct RimeEngine {
    native: Option<NativeBackend>,
}

impl Default for RimeEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl RimeEngine {
    pub fn new() -> Self {
        Self { native: None }
    }

    /// Returns whether an initialized native librime session is available.
    pub fn is_available(&self) -> bool {
        self.native.is_some()
    }

    /// Returns the schema selected by the active native session.
    pub fn active_schema(&self) -> Option<&str> {
        self.native.as_ref().map(NativeBackend::schema)
    }

    /// Loads read-only Rime/雾凇 dictionaries as the base candidate source. These entries are
    /// intentionally kept separate from the user dictionary exposed by import/export APIs.
    pub fn with_resource_dir(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        Self::with_resource_dir_and_schema(path, &configured_schema())
    }

    pub fn with_resource_dir_and_schema(
        path: impl AsRef<Path>,
        schema: &str,
    ) -> Result<Self, CoreError> {
        Self::with_resource_dir_and_user_dir_and_schema(path, default_user_dir(), schema)
    }

    pub fn with_resource_dir_and_user_dir(
        path: impl AsRef<Path>,
        user_dir: impl AsRef<Path>,
    ) -> Result<Self, CoreError> {
        Self::with_resource_dir_and_user_dir_and_schema(path, user_dir, &configured_schema())
    }

    /// Loads the official Rime/雾凇 package and selects one of its published schemas.
    ///
    /// The schema file and compiled data are still read by librime; Lime only passes the
    /// requested schema id to the native session.  `rime_ice` remains the default, while
    /// `LIME_RIME_SCHEMA` or this constructor can select another schema shipped by the
    /// release package.
    pub fn with_resource_dir_and_user_dir_and_schema(
        path: impl AsRef<Path>,
        user_dir: impl AsRef<Path>,
        schema: &str,
    ) -> Result<Self, CoreError> {
        let mut engine = Self::new();
        let root = path.as_ref();

        // A packaged installation uses the official librime DLL together with
        // its shared/prebuilt data.  Do not parse those dictionaries or create
        // a parallel candidate implementation when the native runtime exists.
        if let Some(dll) = find_librime_dll(root) {
            let native = NativeBackend::open(&dll, root, user_dir.as_ref(), schema)
                .map_err(|error| CoreError::new(ErrorCode::RimeInitializationFailed, error))?;
            engine.native = Some(native);
            return Ok(engine);
        }

        Err(CoreError::new(
            ErrorCode::RimeInitializationFailed,
            format!("librime DLL not found under {}", root.display()),
        ))
    }

    /// Changes the active native schema without rewriting any published resource files.
    pub fn select_schema(&mut self, schema: &str) -> Result<(), CoreError> {
        self.native
            .as_mut()
            .ok_or_else(|| {
                CoreError::new(
                    ErrorCode::RimeInitializationFailed,
                    "librime backend is unavailable",
                )
            })?
            .select_schema(schema)
    }

    /// Processes a single librime key event without synthesizing or parsing candidates.
    ///
    /// Key codes and masks follow librime's public `RimeProcessKey` ABI. The existing IPC
    /// input contract carries a complete preedit snapshot rather than native key events, so
    /// callers that need key-binder/recognizer semantics must use this stateful interface.
    pub fn process_key(&mut self, keycode: i32, mask: i32) -> Result<RimeKeyResult, CoreError> {
        self.native
            .as_mut()
            .ok_or_else(|| {
                CoreError::new(
                    ErrorCode::RimeInitializationFailed,
                    "librime backend is unavailable",
                )
            })?
            .process_key(keycode, mask)
    }

    /// Returns a bounded Rime candidate prefix plus the complete-input subset of the first
    /// `rerank_count` rows. A zero `candidate_limit` keeps the direct engine API unbounded.
    /// librime determines coverage through its commit preview, so the result also works for
    /// double-pinyin schemas, emoji conversions, and other candidates whose displayed character
    /// count does not match the number of pinyin syllables.
    pub(crate) fn candidates_for_rerank(
        &mut self,
        preedit: &str,
        rerank_count: usize,
        candidate_limit: usize,
    ) -> Result<CandidateBatch, CoreError> {
        self.native
            .as_mut()
            .ok_or_else(|| {
                CoreError::new(
                    ErrorCode::RimeInitializationFailed,
                    "librime backend is unavailable on this platform or was not initialized",
                )
            })?
            .candidates_for_rerank(preedit, rerank_count, candidate_limit)
    }

    fn normalized(input: &str) -> String {
        input
            .chars()
            .filter(|c| !c.is_whitespace())
            .map(|c| c.to_ascii_lowercase())
            .collect()
    }
}

#[cfg(windows)]
mod native;
#[cfg(windows)]
use native::NativeBackend;

#[cfg(not(windows))]
struct NativeBackend;

#[cfg(not(windows))]
impl NativeBackend {
    fn open(_: &Path, _: &Path, _: &Path, _: &str) -> Result<Self, String> {
        Err("bundled librime supports Windows x64 only".to_owned())
    }
    fn select_schema(&mut self, _: &str) -> Result<(), CoreError> {
        Err(CoreError::new(
            ErrorCode::RimeInitializationFailed,
            "bundled librime supports Windows x64 only",
        ))
    }
    fn process_key(&mut self, _: i32, _: i32) -> Result<RimeKeyResult, CoreError> {
        Err(CoreError::new(
            ErrorCode::RimeInitializationFailed,
            "bundled librime supports Windows x64 only",
        ))
    }

    fn candidates_for_rerank(
        &mut self,
        _: &str,
        _: usize,
        _: usize,
    ) -> Result<CandidateBatch, CoreError> {
        Err(CoreError::new(
            ErrorCode::RimeInitializationFailed,
            "bundled librime supports Windows x64 only",
        ))
    }
    fn schema(&self) -> &str {
        DEFAULT_RIME_SCHEMA
    }
    fn candidates(&mut self, _: &str) -> Result<Vec<Candidate>, CoreError> {
        Err(CoreError::new(
            ErrorCode::RimeInitializationFailed,
            "bundled librime supports Windows x64 only",
        ))
    }
    fn learn(&mut self, _: &str, _: &str) -> Result<(), CoreError> {
        Err(CoreError::new(
            ErrorCode::RimeInitializationFailed,
            "bundled librime supports Windows x64 only",
        ))
    }
    fn export_dictionary(&mut self) -> Result<Vec<DictionaryEntry>, CoreError> {
        Err(CoreError::new(
            ErrorCode::RimeInitializationFailed,
            "bundled librime supports Windows x64 only",
        ))
    }
    fn import_dictionary(&mut self, _: &[DictionaryEntry]) -> Result<(), CoreError> {
        Err(CoreError::new(
            ErrorCode::RimeInitializationFailed,
            "bundled librime supports Windows x64 only",
        ))
    }
    fn clear_dictionary(&mut self) -> Result<(), CoreError> {
        Err(CoreError::new(
            ErrorCode::RimeInitializationFailed,
            "bundled librime supports Windows x64 only",
        ))
    }
}

impl CandidateEngine for RimeEngine {
    fn candidates(&mut self, preedit: &str) -> Result<Vec<Candidate>, CoreError> {
        self.candidates_for_rerank(preedit, 0, 0)
            .map(|batch| batch.candidates)
    }

    fn learn(&mut self, pinyin: &str, text: &str) -> Result<(), CoreError> {
        self.native
            .as_mut()
            .ok_or_else(|| {
                CoreError::new(
                    ErrorCode::RimeInitializationFailed,
                    "librime user dictionary API is unavailable",
                )
            })?
            .learn(pinyin, text)
    }

    fn export_dictionary(&mut self) -> Result<Vec<DictionaryEntry>, CoreError> {
        self.native
            .as_mut()
            .ok_or_else(|| {
                CoreError::new(
                    ErrorCode::RimeInitializationFailed,
                    "librime user dictionary API is unavailable",
                )
            })?
            .export_dictionary()
    }

    fn import_dictionary(&mut self, entries: &[DictionaryEntry]) -> Result<(), CoreError> {
        for entry in entries {
            if Self::normalized(&entry.pinyin).is_empty() || entry.text.trim().is_empty() {
                return Err(CoreError::new(
                    ErrorCode::InvalidRequest,
                    "dictionary entry contains empty fields",
                ));
            }
        }
        self.native
            .as_mut()
            .ok_or_else(|| {
                CoreError::new(
                    ErrorCode::RimeInitializationFailed,
                    "librime user dictionary API is unavailable",
                )
            })?
            .import_dictionary(entries)
    }

    fn clear_dictionary(&mut self) -> Result<(), CoreError> {
        self.native
            .as_mut()
            .ok_or_else(|| {
                CoreError::new(
                    ErrorCode::RimeInitializationFailed,
                    "librime user dictionary API is unavailable",
                )
            })?
            .clear_dictionary()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn normalized_preedit_matches_legacy_case_and_separator_contract() {
        assert_eq!(RimeEngine::normalized(" Ni Hao\t"), "nihao");
        assert_eq!(RimeEngine::normalized("ＮＩＨＡＯ"), "ＮＩＨＡＯ");
        assert_eq!(RimeEngine::normalized("NH"), "nh");
        assert_eq!(RimeEngine::normalized("你 好"), "你好");
    }

    #[test]
    fn default_engine_is_explicitly_unavailable() {
        let mut engine = RimeEngine::new();
        assert!(engine.export_dictionary().is_err());
        assert!(engine.candidates("nihao").is_err());
        assert!(engine.process_key('n' as i32, 0).is_err());
    }

    #[test]
    fn resource_dir_without_librime_dll_is_not_parsed() {
        let root = std::env::temp_dir().join(format!("lime-rime-no-dll-{}", std::process::id()));
        fs::create_dir_all(root.join("cn_dicts")).unwrap();
        fs::write(
            root.join("cn_dicts").join("base.dict.yaml"),
            "你好 nihao 100\n",
        )
        .unwrap();
        let result = RimeEngine::with_resource_dir(&root);
        assert!(result.is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn packaged_resources_load_when_requested() {
        let Ok(path) = std::env::var("LIME_TEST_RIME_DIR") else {
            return;
        };
        let mut engine = RimeEngine::with_resource_dir(path).expect("load packaged Rime resources");
        assert!(!engine.candidates("nihao").unwrap().is_empty());
        if std::env::var_os("LIME_TEST_RIME_LEVERS").is_some() {
            assert!(engine
                .export_dictionary()
                .expect("export Rime user dictionary")
                .is_empty());
            engine
                .import_dictionary(&[DictionaryEntry {
                    pinyin: "nihao".into(),
                    text: "拟好".into(),
                    weight: 7,
                }])
                .expect("import Rime user dictionary");
            let exported = engine
                .export_dictionary()
                .expect("re-export Rime user dictionary");
            assert!(exported.iter().any(|entry| {
                entry.pinyin == "nihao" && entry.text == "拟好" && entry.weight == 7
            }));
            assert!(engine
                .candidates("nihao")
                .expect("query imported Rime user dictionary")
                .iter()
                .any(|candidate| candidate.commit_text == "拟好"));
            engine
                .clear_dictionary()
                .expect("clear Rime user dictionary");
            assert!(engine
                .export_dictionary()
                .expect("export cleared Rime user dictionary")
                .is_empty());
        }
        if std::env::var_os("LIME_TEST_RIME_LEARN").is_some() {
            engine.learn("nihao", "你好").expect("learn through Rime");
        }
    }

    #[test]
    fn packaged_resources_can_reopen_after_deployment() {
        let Ok(path) = std::env::var("LIME_TEST_RIME_DIR") else {
            return;
        };
        let user_dir =
            std::env::temp_dir().join(format!("lime-rime-reopen-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&user_dir);

        {
            let mut engine = RimeEngine::with_resource_dir_and_user_dir_and_schema(
                &path,
                &user_dir,
                DEFAULT_RIME_SCHEMA,
            )
            .expect("load packaged Rime resources on first startup");
            assert!(!engine
                .candidates("nihao")
                .expect("query candidates after first deployment")
                .is_empty());
        }

        // The second startup normally has no pending deployment work, so
        // RimeStartMaintenance(false) returns false. It must still produce a
        // usable native session rather than LIME-0005.
        let mut reopened = RimeEngine::with_resource_dir_and_user_dir_and_schema(
            &path,
            &user_dir,
            DEFAULT_RIME_SCHEMA,
        )
        .expect("reopen packaged Rime resources without pending maintenance");
        assert!(!reopened
            .candidates("nihao")
            .expect("query candidates after reopening")
            .is_empty());

        let _ = fs::remove_dir_all(user_dir);
    }

    #[test]
    fn packaged_candidates_follow_the_latest_normalized_input() {
        let Ok(path) = std::env::var("LIME_TEST_RIME_DIR") else {
            return;
        };
        let user_dir = std::env::temp_dir().join(format!(
            "lime-rime-candidate-snapshot-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&user_dir);
        let mut engine = RimeEngine::with_resource_dir_and_user_dir_and_schema(
            path,
            &user_dir,
            DEFAULT_RIME_SCHEMA,
        )
        .expect("load packaged Rime resources");

        let nihao = engine
            .candidates("NI HAO")
            .expect("query normalized full-pinyin input");
        assert_eq!(
            nihao
                .first()
                .map(|candidate| candidate.commit_text.as_str()),
            Some("你好")
        );

        let wo = engine
            .candidates("WO")
            .expect("query the next input snapshot");
        assert_eq!(
            wo.first().map(|candidate| candidate.commit_text.as_str()),
            Some("我")
        );
        assert!(!wo.iter().any(|candidate| candidate.commit_text == "你好"));

        // An empty normalized snapshot must clear the previous menu rather than returning stale
        // candidates from the preceding request.
        let empty = engine
            .candidates(" \t")
            .expect("clear the native composition");
        assert!(empty.is_empty());

        let xiexie = engine
            .candidates("xie xie")
            .expect("query after clearing the composition");
        assert_eq!(
            xiexie
                .first()
                .map(|candidate| candidate.commit_text.as_str()),
            Some("谢谢")
        );
        let _ = fs::remove_dir_all(user_dir);
    }

    #[test]
    fn packaged_candidate_coverage_excludes_unconsumed_input() {
        let Ok(path) = std::env::var("LIME_TEST_RIME_DIR") else {
            return;
        };
        let user_dir =
            std::env::temp_dir().join(format!("lime-rime-coverage-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&user_dir);
        let mut engine = RimeEngine::with_resource_dir_and_user_dir_and_schema(
            path,
            &user_dir,
            DEFAULT_RIME_SCHEMA,
        )
        .expect("load packaged Rime resources");

        let batch = engine
            .candidates_for_rerank("nihao", 32, 32)
            .expect("read candidate coverage");
        assert!(batch.candidates.len() <= 32);
        let complete = batch
            .complete_candidate_indices
            .iter()
            .map(|index| batch.candidates[*index].commit_text.as_str())
            .collect::<Vec<_>>();

        assert!(complete.contains(&"你好"));
        assert!(complete.contains(&"👋"));
        assert!(!complete.contains(&"你"));
        assert_eq!(batch.candidate_remainders.len(), batch.candidates.len());
        let partial_index = batch
            .candidates
            .iter()
            .position(|candidate| candidate.commit_text == "你")
            .expect("one-character candidate should be present");
        assert_eq!(
            batch.candidate_remainders[partial_index].as_deref(),
            Some("hao")
        );
        for index in &batch.complete_candidate_indices {
            assert_eq!(batch.candidate_remainders[*index].as_deref(), Some(""));
        }
        let _ = fs::remove_dir_all(user_dir);
    }

    #[test]
    fn packaged_process_key_uses_native_rime_when_requested() {
        if std::env::var_os("LIME_TEST_RIME_PROCESS_KEY").is_none() {
            return;
        }
        let Ok(path) = std::env::var("LIME_TEST_RIME_DIR") else {
            return;
        };
        let user_dir =
            std::env::temp_dir().join(format!("lime-rime-process-key-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&user_dir);
        let mut engine = RimeEngine::with_resource_dir_and_user_dir_and_schema(
            path,
            &user_dir,
            DEFAULT_RIME_SCHEMA,
        )
        .expect("load packaged Rime resources");
        let mut result = None;
        for key in "nihao".bytes() {
            result = Some(
                engine
                    .process_key(i32::from(key), 0)
                    .expect("process native Rime key"),
            );
        }
        let result = result.expect("at least one key result");
        assert!(result.handled);
        assert!(result.commit_text.is_none());
        assert!(result
            .candidates
            .iter()
            .any(|candidate| candidate.commit_text == "你好"));

        let committed = engine
            .process_key(i32::from(b' '), 0)
            .expect("commit through native Rime key handling");
        assert!(committed.handled);
        assert_eq!(committed.commit_text.as_deref(), Some("你好"));
        let _ = fs::remove_dir_all(user_dir);
    }

    #[test]
    fn packaged_schema_switch_uses_native_librime_when_requested() {
        if std::env::var_os("LIME_TEST_RIME_SCHEMA_SWITCH").is_none() {
            return;
        }
        let Ok(path) = std::env::var("LIME_TEST_RIME_DIR") else {
            return;
        };
        let user_dir = std::env::var_os("LIME_RIME_USER_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(default_user_dir);
        let mut engine = RimeEngine::with_resource_dir_and_user_dir_and_schema(
            path,
            user_dir,
            DEFAULT_RIME_SCHEMA,
        )
        .expect("load packaged Rime resources");
        engine
            .select_schema("double_pinyin_flypy")
            .expect("select published double-pinyin schema");
        assert!(!engine
            .candidates("nh")
            .expect("query double-pinyin candidates")
            .is_empty());
        engine
            .import_dictionary(&[DictionaryEntry {
                pinyin: "nh".into(),
                text: "拟好".into(),
                weight: 7,
            }])
            .expect("import user dictionary for selected schema");
        assert!(engine
            .export_dictionary()
            .expect("export user dictionary for selected schema")
            .iter()
            .any(|entry| entry.pinyin == "nh" && entry.text == "拟好"));
        engine
            .select_schema(DEFAULT_RIME_SCHEMA)
            .expect("restore published full-pinyin schema");
        assert!(!engine
            .candidates("nihao")
            .expect("query full-pinyin candidates")
            .is_empty());
    }

    #[test]
    fn user_dictionary_api_reports_native_boundary() {
        let mut engine = RimeEngine::new();
        assert!(engine
            .import_dictionary(&[DictionaryEntry {
                pinyin: "nihao".into(),
                text: "你好".into(),
                weight: 1,
            }])
            .is_err());
    }

    #[test]
    fn exported_dictionary_parser_keeps_utf8_codes() {
        let entries = parse_user_dictionary("# Rime table\n短语\t自定义码\t3\n").unwrap();
        assert_eq!(
            entries,
            vec![DictionaryEntry {
                pinyin: "自定义码".into(),
                text: "短语".into(),
                weight: 3,
            }]
        );
    }

    #[test]
    fn exported_dictionary_parser_rejects_malformed_rows() {
        assert!(parse_user_dictionary("短语\t自定义码\n").is_err());
        assert!(parse_user_dictionary("短语\t自定义码\tnot-a-number\n").is_err());
    }
}
