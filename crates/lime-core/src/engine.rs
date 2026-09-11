use crate::CoreError;
use lime_protocol::{Candidate, DictionaryEntry, ErrorCode};
use std::path::{Path, PathBuf};

pub trait CandidateEngine: Send {
    fn candidates(&mut self, preedit: &str) -> Result<Vec<Candidate>, CoreError>;
    /// Process one native librime key event when the backend supports stateful input.
    /// Existing snapshot-based engines may keep the compatibility default until their
    /// platform adapter supplies key codes and modifier masks.
    fn process_key(&mut self, _keycode: i32, _mask: i32) -> Result<RimeKeyResult, CoreError> {
        Err(CoreError::new(
            ErrorCode::RimeInitializationFailed,
            "native key processing is unavailable",
        ))
    }
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

    pub fn from_dictionary_file(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let _ = path;
        Err(CoreError::new(
            ErrorCode::RimeInitializationFailed,
            "direct dictionary parsing is not supported; configure librime resources",
        ))
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

    /// Preserves the existing persistence-facing API boundary while routing migrated entries
    /// through librime's native user-dictionary API.
    pub fn import_persisted_dictionary(
        &mut self,
        entries: &[DictionaryEntry],
    ) -> Result<(), CoreError> {
        let migrated = entries
            .iter()
            .filter(|entry| !entry.pinyin.trim().is_empty() && !entry.text.trim().is_empty())
            .cloned()
            .collect::<Vec<_>>();
        self.import_dictionary(&migrated)
    }

    fn normalized(input: &str) -> String {
        input
            .chars()
            .filter(|c| !c.is_whitespace())
            .map(|c| c.to_ascii_lowercase())
            .collect()
    }
}

fn find_librime_dll(root: &Path) -> Option<PathBuf> {
    [
        root.join("rime.dll"),
        root.join("windows-x64").join("rime.dll"),
        root.join("bin").join("rime.dll"),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

fn default_user_dir() -> PathBuf {
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

fn parse_user_dictionary(text: &str) -> Result<Vec<DictionaryEntry>, String> {
    let mut entries = Vec::new();
    for (line_number, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim_end_matches('\r');
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 3 {
            return Err(format!(
                "Rime user dictionary line {} has {} tab-separated fields; expected 3",
                line_number + 1,
                fields.len()
            ));
        }
        let value = fields[0];
        let pinyin = fields[1].trim();
        let weight = fields[2].trim().parse().map_err(|_| {
            format!(
                "Rime user dictionary line {} has an invalid weight",
                line_number + 1
            )
        })?;
        if value.trim().is_empty() || pinyin.is_empty() {
            return Err(format!(
                "Rime user dictionary line {} contains an empty field",
                line_number + 1
            ));
        }
        entries.push(DictionaryEntry {
            pinyin: pinyin.to_owned(),
            text: value.to_owned(),
            weight,
        });
    }
    Ok(entries)
}

// Rime-Ice's main translator is the native user dictionary.  The bundled
// `custom_phrase.txt` is a published read-only table and must not be treated
// as Lime's mutable user database.
const RIME_ICE_USER_DICT: &str = "rime_ice";
pub const DEFAULT_RIME_SCHEMA: &str = "rime_ice";

fn configured_schema() -> String {
    std::env::var("LIME_RIME_SCHEMA")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_RIME_SCHEMA.to_owned())
}

fn validate_schema(schema: &str) -> Result<(), String> {
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

#[cfg(windows)]
mod native {
    use super::*;
    use std::{
        ffi::{c_char, c_int, c_void, CStr, CString},
        os::windows::ffi::OsStrExt,
        ptr,
    };

    type SessionId = usize;

    #[repr(C)]
    struct Traits {
        data_size: c_int,
        shared_data_dir: *const c_char,
        user_data_dir: *const c_char,
        distribution_name: *const c_char,
        distribution_code_name: *const c_char,
        distribution_version: *const c_char,
        app_name: *const c_char,
        modules: *const *const c_char,
        min_log_level: c_int,
        log_dir: *const c_char,
        prebuilt_data_dir: *const c_char,
        staging_dir: *const c_char,
    }

    #[repr(C)]
    struct Composition {
        length: c_int,
        cursor_pos: c_int,
        sel_start: c_int,
        sel_end: c_int,
        preedit: *mut c_char,
    }
    #[repr(C)]
    struct FfiCandidate {
        text: *mut c_char,
        comment: *mut c_char,
        reserved: *mut c_void,
    }
    #[repr(C)]
    struct Menu {
        page_size: c_int,
        page_no: c_int,
        is_last_page: c_int,
        highlighted_candidate_index: c_int,
        num_candidates: c_int,
        candidates: *mut FfiCandidate,
        select_keys: *mut c_char,
    }
    #[repr(C)]
    struct Context {
        data_size: c_int,
        composition: Composition,
        menu: Menu,
        commit_text_preview: *mut c_char,
        select_labels: *mut *mut c_char,
    }
    #[repr(C)]
    struct CandidateIterator {
        ptr: *mut c_void,
        index: c_int,
        candidate: FfiCandidate,
    }

    #[repr(C)]
    struct Commit {
        data_size: c_int,
        text: *mut c_char,
    }

    type Setup = unsafe extern "C" fn(*mut Traits);
    type Initialize = unsafe extern "C" fn(*mut Traits);
    type Finalize = unsafe extern "C" fn();
    type Maintenance = unsafe extern "C" fn(c_int) -> c_int;
    type JoinMaintenance = unsafe extern "C" fn();
    type CreateSession = unsafe extern "C" fn() -> SessionId;
    type DestroySession = unsafe extern "C" fn(SessionId) -> c_int;
    type ProcessKey = unsafe extern "C" fn(SessionId, c_int, c_int) -> c_int;
    type SelectSchema = unsafe extern "C" fn(SessionId, *const c_char) -> c_int;
    type SetInput = unsafe extern "C" fn(SessionId, *const c_char) -> c_int;
    type GetContext = unsafe extern "C" fn(SessionId, *mut Context) -> c_int;
    type FreeContext = unsafe extern "C" fn(*mut Context) -> c_int;
    type SelectCandidate = unsafe extern "C" fn(SessionId, usize) -> c_int;
    type HighlightCandidate = unsafe extern "C" fn(SessionId, usize) -> c_int;
    type CommitComposition = unsafe extern "C" fn(SessionId) -> c_int;
    type GetCommit = unsafe extern "C" fn(SessionId, *mut Commit) -> c_int;
    type FreeCommit = unsafe extern "C" fn(*mut Commit) -> c_int;
    type ListBegin = unsafe extern "C" fn(SessionId, *mut CandidateIterator) -> c_int;
    type ListNext = unsafe extern "C" fn(*mut CandidateIterator) -> c_int;
    type ListEnd = unsafe extern "C" fn(*mut CandidateIterator);
    type GetVersion = unsafe extern "C" fn() -> *const c_char;
    type FindModule = unsafe extern "C" fn(*const c_char) -> *mut Module;
    type GetModuleApi = unsafe extern "C" fn() -> *mut c_void;

    #[derive(Debug, Clone, Copy)]
    #[repr(C)]
    struct Api {
        data_size: c_int,
        setup: Setup,
        _notification: *mut c_void,
        initialize: Initialize,
        finalize: Finalize,
        start_maintenance: Maintenance,
        _is_maintenance_mode: *mut c_void,
        join_maintenance_thread: JoinMaintenance,
        _deployer_initialize: *mut c_void,
        _prebuild: *mut c_void,
        _deploy: *mut c_void,
        _deploy_schema: *mut c_void,
        _deploy_config_file: *mut c_void,
        _sync_user_data: *mut c_void,
        create_session: CreateSession,
        _find_session: *mut c_void,
        destroy_session: DestroySession,
        _cleanup_stale_sessions: *mut c_void,
        _cleanup_all_sessions: *mut c_void,
        process_key: ProcessKey,
        commit_composition: CommitComposition,
        _clear_composition: *mut c_void,
        get_commit: GetCommit,
        free_commit: FreeCommit,
        get_context: GetContext,
        free_context: FreeContext,
        _get_status: *mut c_void,
        _free_status: *mut c_void,
        _set_option: *mut c_void,
        _get_option: *mut c_void,
        _set_property: *mut c_void,
        _get_property: *mut c_void,
        _get_schema_list: *mut c_void,
        _free_schema_list: *mut c_void,
        _get_current_schema: *mut c_void,
        select_schema: SelectSchema,
        _config: [*mut c_void; 12],
        _testing: *mut c_void,
        _register_module: *mut c_void,
        find_module: FindModule,
        _run_task: *mut c_void,
        _dirs: [*mut c_void; 18],
        _get_input: *mut c_void,
        _get_caret_pos: *mut c_void,
        select_candidate: SelectCandidate,
        get_version: GetVersion,
        _set_caret_pos: *mut c_void,
        _select_candidate_on_current_page: *mut c_void,
        candidate_list_begin: ListBegin,
        candidate_list_next: ListNext,
        candidate_list_end: ListEnd,
        _post_candidate: [*mut c_void; 11],
        set_input: SetInput,
        _dir_string: [*mut c_void; 5],
        highlight_candidate: HighlightCandidate,
        _highlight_candidate_on_current_page: *mut c_void,
        _change_page: *mut c_void,
    }

    #[repr(C)]
    struct Module {
        data_size: c_int,
        module_name: *const c_char,
        _initialize: *mut c_void,
        _finalize: *mut c_void,
        get_api: GetModuleApi,
    }

    #[repr(C)]
    struct UserDictIterator {
        ptr: *mut c_void,
        i: usize,
    }

    #[derive(Debug, Clone, Copy)]
    #[repr(C)]
    struct LeversApi {
        data_size: c_int,
        _settings: [*mut c_void; 24],
        user_dict_iterator_init: unsafe extern "C" fn(*mut UserDictIterator) -> c_int,
        user_dict_iterator_destroy: unsafe extern "C" fn(*mut UserDictIterator),
        next_user_dict: unsafe extern "C" fn(*mut UserDictIterator) -> *const c_char,
        _backup: *mut c_void,
        _restore: *mut c_void,
        export_user_dict: unsafe extern "C" fn(*const c_char, *const c_char) -> c_int,
        import_user_dict: unsafe extern "C" fn(*const c_char, *const c_char) -> c_int,
        _customize_item: *mut c_void,
    }

    #[derive(Debug)]
    struct Library {
        handle: *mut c_void,
    }
    unsafe impl Send for Library {}
    unsafe impl Sync for Library {}

    #[derive(Debug)]
    pub(super) struct NativeBackend {
        _library: Library,
        api: Api,
        session: SessionId,
        user_dir: PathBuf,
        schema: String,
        levers: Option<LeversApi>,
    }
    unsafe impl Send for NativeBackend {}

    impl NativeBackend {
        pub(super) fn open(
            dll: &Path,
            shared: &Path,
            user: &Path,
            schema: &str,
        ) -> Result<Self, String> {
            validate_schema(schema)?;
            let dll = absolute(dll)?;
            let package_root = absolute(shared)?;
            let shared = if package_root.join("shared").is_dir() {
                package_root.join("shared")
            } else {
                package_root.clone()
            };
            // The published Rime-Ice `full_compiled.zip` uses the standard
            // `build/` directory. Keep `prebuilt/` as a compatibility path
            // for the local evaluator's older package layout.
            let prebuilt = if has_bin_files(&package_root.join("build")) {
                package_root.join("build")
            } else if has_bin_files(&package_root.join("prebuilt")) {
                package_root.join("prebuilt")
            } else if has_bin_files(&shared.join("build")) {
                shared.join("build")
            } else if has_bin_files(&shared.join("prebuilt")) {
                shared.join("prebuilt")
            } else {
                shared.clone()
            };
            if !prebuilt.join("rime_ice.table.bin").is_file() {
                return Err(format!(
                    "compiled Rime-Ice dictionary not found: {}",
                    prebuilt.join("rime_ice.table.bin").display()
                ));
            }
            let user = absolute(user)?;
            std::fs::create_dir_all(&user).map_err(|e| format!("create Rime user dir: {e}"))?;
            let log_dir = user.join("logs");
            std::fs::create_dir_all(&log_dir).map_err(|e| format!("create Rime log dir: {e}"))?;
            // Keep librime's generated deployment state under the conventional
            // `${user_data_dir}/build` directory.  The package resources remain
            // read-only; only this per-user staging directory is writable.
            let staging_dir = user.join("build");
            std::fs::create_dir_all(&staging_dir)
                .map_err(|e| format!("create Rime staging dir: {e}"))?;

            let library = Library::load(&dll)?;
            let api = unsafe { Api::load(library.handle)? };
            let shared_c = c_path(&shared)?;
            let user_c = c_path(&user)?;
            let prebuilt_c = c_path(&prebuilt)?;
            let log_c = c_path(&log_dir)?;
            let staging_c = c_path(&staging_dir)?;
            let distribution = CString::new("Lime").unwrap();
            let version = CString::new(env!("CARGO_PKG_VERSION")).unwrap();
            let app = CString::new("rime.lime").unwrap();
            let mut traits = Traits {
                data_size: (std::mem::size_of::<Traits>() - std::mem::size_of::<c_int>()) as c_int,
                shared_data_dir: shared_c.as_ptr(),
                user_data_dir: user_c.as_ptr(),
                distribution_name: distribution.as_ptr(),
                distribution_code_name: distribution.as_ptr(),
                distribution_version: version.as_ptr(),
                app_name: app.as_ptr(),
                modules: ptr::null(),
                min_log_level: 2,
                log_dir: log_c.as_ptr(),
                prebuilt_data_dir: prebuilt_c.as_ptr(),
                staging_dir: staging_c.as_ptr(),
            };
            unsafe {
                (api.setup)(&mut traits as *mut _);
                // Let librime deploy its published resources into the isolated
                // user data directory. Lime does not parse or rewrite them.
                (api.initialize)(&mut traits as *mut _);
                // `RimeStartMaintenance(false)` returns false when librime
                // detects that there is no pending deployment work.  That is
                // the normal path after the first successful startup, not an
                // initialization failure.  Only join when a maintenance
                // thread was actually started.
                if (api.start_maintenance)(0) != 0 {
                    // RimeInitialize/start_maintenance may schedule work on a
                    // background thread; wait before creating the first session.
                    (api.join_maintenance_thread)();
                }
            }
            let levers = match unsafe { load_levers(&api) } {
                Ok(levers) => levers,
                Err(error) => {
                    unsafe { (api.finalize)() };
                    return Err(error);
                }
            };
            let session = unsafe { (api.create_session)() };
            if session == 0 {
                unsafe { (api.finalize)() };
                return Err("librime could not create a session".to_owned());
            }
            let schema_c = CString::new(schema).map_err(|_| "schema id contains NUL".to_owned())?;
            if unsafe { (api.select_schema)(session, schema_c.as_ptr()) == 0 } {
                unsafe {
                    (api.destroy_session)(session);
                    (api.finalize)();
                }
                return Err(format!("librime could not select schema {schema}"));
            }
            Ok(Self {
                _library: library,
                api,
                session,
                user_dir: user,
                schema: schema.to_owned(),
                levers,
            })
        }

        pub(super) fn candidates_for_rerank(
            &mut self,
            input: &str,
            rerank_count: usize,
            candidate_limit: usize,
        ) -> Result<CandidateBatch, CoreError> {
            // The legacy candidate engine accepted pinyin case-insensitively and ignored
            // separators.  Keep that contract at the native boundary as well: TSF normally
            // supplies lowercase letters, while the management test page may receive pasted
            // uppercase/space-separated input.  Always submit the normalized snapshot (including
            // an empty one) so a cleared request also clears librime's previous composition.
            let normalized = RimeEngine::normalized(input);
            let input = CString::new(normalized)
                .map_err(|_| CoreError::new(ErrorCode::InvalidRequest, "preedit contains NUL"))?;
            if unsafe { (self.api.set_input)(self.session, input.as_ptr()) } == 0 {
                return Err(CoreError::new(
                    ErrorCode::RimeInitializationFailed,
                    "librime rejected input",
                ));
            }
            self.current_candidates(rerank_count, candidate_limit)
        }

        pub(super) fn process_key(
            &mut self,
            keycode: i32,
            mask: i32,
        ) -> Result<RimeKeyResult, CoreError> {
            if self.session == 0 {
                return Err(CoreError::new(
                    ErrorCode::RimeInitializationFailed,
                    "librime session is unavailable",
                ));
            }
            let handled = unsafe { (self.api.process_key)(self.session, keycode, mask) != 0 };
            let commit_text = self.take_commit()?;
            let candidates = self.current_candidates(0, 0)?.candidates;
            Ok(RimeKeyResult {
                handled,
                commit_text,
                candidates,
            })
        }

        pub(super) fn schema(&self) -> &str {
            &self.schema
        }

        fn take_commit(&self) -> Result<Option<String>, CoreError> {
            let mut commit = Commit {
                data_size: (std::mem::size_of::<Commit>() - std::mem::size_of::<c_int>()) as c_int,
                text: ptr::null_mut(),
            };
            if unsafe { (self.api.get_commit)(self.session, &mut commit) == 0 } {
                return Ok(None);
            }
            let text = c_string(commit.text);
            if unsafe { (self.api.free_commit)(&mut commit) == 0 } {
                return Err(CoreError::new(
                    ErrorCode::RimeInitializationFailed,
                    "librime failed to free commit data",
                ));
            }
            Ok((!text.is_empty()).then_some(text))
        }

        fn current_candidates(
            &self,
            rerank_count: usize,
            candidate_limit: usize,
        ) -> Result<CandidateBatch, CoreError> {
            let mut context = Context {
                data_size: (std::mem::size_of::<Context>() - std::mem::size_of::<c_int>()) as c_int,
                composition: Composition {
                    length: 0,
                    cursor_pos: 0,
                    sel_start: 0,
                    sel_end: 0,
                    preedit: ptr::null_mut(),
                },
                menu: Menu {
                    page_size: 0,
                    page_no: 0,
                    is_last_page: 0,
                    highlighted_candidate_index: 0,
                    num_candidates: 0,
                    candidates: ptr::null_mut(),
                    select_keys: ptr::null_mut(),
                },
                commit_text_preview: ptr::null_mut(),
                select_labels: ptr::null_mut(),
            };
            let mut out = Vec::new();
            let ok = unsafe { (self.api.get_context)(self.session, &mut context) != 0 };
            if !ok {
                return Err(CoreError::new(
                    ErrorCode::RimeInitializationFailed,
                    "librime failed to read session context",
                ));
            }
            let original_highlight = (context.menu.num_candidates > 0).then(|| {
                context.menu.page_no.max(0) as usize * context.menu.page_size.max(0) as usize
                    + context.menu.highlighted_candidate_index.max(0) as usize
            });
            unsafe { (self.api.free_context)(&mut context) };
            let mut iterator = CandidateIterator {
                ptr: ptr::null_mut(),
                index: 0,
                candidate: FfiCandidate {
                    text: ptr::null_mut(),
                    comment: ptr::null_mut(),
                    reserved: ptr::null_mut(),
                },
            };
            unsafe {
                if (self.api.candidate_list_begin)(self.session, &mut iterator) != 0 {
                    loop {
                        let source_index = iterator.index.max(0) as usize;
                        if candidate_limit > 0 && source_index >= candidate_limit.max(rerank_count)
                        {
                            break;
                        }
                        let text = c_string(iterator.candidate.text);
                        if !text.is_empty() {
                            out.push((
                                Candidate {
                                    display_text: text.clone(),
                                    commit_text: text,
                                },
                                source_index,
                            ));
                        }
                        if (self.api.candidate_list_next)(&mut iterator) == 0 {
                            break;
                        }
                    }
                    (self.api.candidate_list_end)(&mut iterator);
                }
            }
            let mut complete_candidate_indices = Vec::new();
            let mut candidate_remainders = Vec::with_capacity(out.len());
            for (candidate_index, (candidate, source_index)) in out.iter().enumerate() {
                let remainder = self.candidate_remainder(*source_index, candidate);
                if candidate_index < rerank_count && remainder.as_deref() == Some("") {
                    complete_candidate_indices.push(candidate_index);
                }
                candidate_remainders.push(remainder);
            }
            if let Some(index) = original_highlight {
                unsafe {
                    (self.api.highlight_candidate)(self.session, index);
                }
            }
            Ok(CandidateBatch {
                candidates: out.into_iter().map(|(candidate, _)| candidate).collect(),
                complete_candidate_indices,
                candidate_remainders,
            })
        }

        fn candidate_remainder(
            &self,
            candidate_index: usize,
            candidate: &Candidate,
        ) -> Option<String> {
            // librime returns false when the requested candidate is already highlighted. Read
            // the context in either case and verify the resulting global highlight index before
            // trusting the preview.
            unsafe {
                (self.api.highlight_candidate)(self.session, candidate_index);
            }
            let mut context = Context {
                data_size: (std::mem::size_of::<Context>() - std::mem::size_of::<c_int>()) as c_int,
                composition: Composition {
                    length: 0,
                    cursor_pos: 0,
                    sel_start: 0,
                    sel_end: 0,
                    preedit: ptr::null_mut(),
                },
                menu: Menu {
                    page_size: 0,
                    page_no: 0,
                    is_last_page: 0,
                    highlighted_candidate_index: 0,
                    num_candidates: 0,
                    candidates: ptr::null_mut(),
                    select_keys: ptr::null_mut(),
                },
                commit_text_preview: ptr::null_mut(),
                select_labels: ptr::null_mut(),
            };
            if unsafe { (self.api.get_context)(self.session, &mut context) } == 0 {
                return None;
            }
            let highlighted_index = context.menu.page_no.max(0) as usize
                * context.menu.page_size.max(0) as usize
                + context.menu.highlighted_candidate_index.max(0) as usize;
            let preview = c_string(context.commit_text_preview);
            unsafe {
                (self.api.free_context)(&mut context);
            }
            if highlighted_index != candidate_index {
                return None;
            }
            preview
                .strip_prefix(&candidate.commit_text)
                .map(str::to_owned)
        }

        pub(super) fn learn(&mut self, pinyin: &str, text: &str) -> Result<(), CoreError> {
            self.import_dictionary(&[DictionaryEntry {
                pinyin: pinyin.to_owned(),
                text: text.to_owned(),
                weight: 1,
            }])
        }

        pub(super) fn export_dictionary(&mut self) -> Result<Vec<DictionaryEntry>, CoreError> {
            let temp = self
                .user_dir
                .join(format!(".lime-levers-export-{}.txt", std::process::id()));
            let result = self.with_session_stopped(|levers| {
                let name = CString::new(RIME_ICE_USER_DICT).unwrap();
                let temp_c = c_path(&temp)
                    .map_err(|e| CoreError::new(ErrorCode::RimeInitializationFailed, e))?;
                let count = unsafe { (levers.export_user_dict)(name.as_ptr(), temp_c.as_ptr()) };
                if count < 0 {
                    return Err(CoreError::new(
                        ErrorCode::RimeInitializationFailed,
                        "librime failed to export rime_ice user dictionary",
                    ));
                }
                std::fs::read(&temp)
                    .map_err(|e| CoreError::new(ErrorCode::RimeInitializationFailed, e.to_string()))
            });
            let _ = std::fs::remove_file(&temp);
            let bytes = result?;
            let text = std::str::from_utf8(&bytes).map_err(|e| {
                CoreError::new(
                    ErrorCode::RimeInitializationFailed,
                    format!("invalid UTF-8 in exported user dictionary: {e}"),
                )
            })?;
            parse_user_dictionary(text).map_err(|error| {
                CoreError::new(
                    ErrorCode::RimeInitializationFailed,
                    format!("invalid exported Rime user dictionary: {error}"),
                )
            })
        }

        pub(super) fn import_dictionary(
            &mut self,
            entries: &[DictionaryEntry],
        ) -> Result<(), CoreError> {
            if entries.is_empty() {
                return Err(CoreError::new(
                    ErrorCode::InvalidRequest,
                    "dictionary import must contain at least one entry",
                ));
            }
            let temp = self
                .user_dir
                .join(format!(".lime-levers-import-{}.txt", std::process::id()));
            let mut output = String::from("# Rime table\n# coding: utf-8\n");
            for entry in entries {
                if entry.pinyin.trim().is_empty()
                    || entry.text.trim().is_empty()
                    || entry.pinyin.contains('\t')
                    || entry.text.contains('\t')
                    || entry.pinyin.contains('\r')
                    || entry.pinyin.contains('\n')
                    || entry.text.contains('\r')
                    || entry.text.contains('\n')
                {
                    return Err(CoreError::new(
                        ErrorCode::InvalidRequest,
                        "Rime user dictionary entry contains invalid fields",
                    ));
                }
                output.push_str(&format!(
                    "{}\t{}\t{}\n",
                    entry.text, entry.pinyin, entry.weight
                ));
            }
            std::fs::write(&temp, output.as_bytes())
                .map_err(|e| CoreError::new(ErrorCode::RimeInitializationFailed, e.to_string()))?;
            let result = self.with_session_stopped(|levers| {
                let name = CString::new(RIME_ICE_USER_DICT).unwrap();
                let temp_c = c_path(&temp)
                    .map_err(|e| CoreError::new(ErrorCode::RimeInitializationFailed, e))?;
                let count = unsafe { (levers.import_user_dict)(name.as_ptr(), temp_c.as_ptr()) };
                if count <= 0 {
                    return Err(CoreError::new(
                        ErrorCode::RimeInitializationFailed,
                        "librime failed to import rime_ice user dictionary",
                    ));
                }
                Ok(())
            });
            let _ = std::fs::remove_file(&temp);
            result
        }

        pub(super) fn clear_dictionary(&mut self) -> Result<(), CoreError> {
            if self.levers.is_none() {
                return Err(CoreError::new(
                    ErrorCode::RimeInitializationFailed,
                    "librime levers module is unavailable",
                ));
            }
            self.stop_session();
            let user_dict = self.user_dir.join(format!("{RIME_ICE_USER_DICT}.userdb"));
            let result = match std::fs::metadata(&user_dict) {
                Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(&user_dict),
                Ok(_) => std::fs::remove_file(&user_dict),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            }
            .map_err(|error| {
                CoreError::new(
                    ErrorCode::RimeInitializationFailed,
                    format!(
                        "remove Rime user dictionary {}: {error}",
                        user_dict.display()
                    ),
                )
            });
            let reopen = self.recreate_session();
            result.and(reopen)
        }

        pub(super) fn select_schema(&mut self, schema: &str) -> Result<(), CoreError> {
            validate_schema(schema)
                .map_err(|error| CoreError::new(ErrorCode::InvalidRequest, error))?;
            if self.schema == schema {
                return Ok(());
            }
            self.stop_session();
            let session = match self.create_session_for_schema(schema) {
                Ok(session) => session,
                Err(error) => {
                    let _ = self.recreate_session();
                    return Err(error);
                }
            };
            self.session = session;
            self.schema = schema.to_owned();
            Ok(())
        }

        fn recreate_session(&mut self) -> Result<(), CoreError> {
            self.stop_session();
            let session = self.create_session_for_schema(&self.schema.clone())?;
            self.session = session;
            Ok(())
        }

        fn create_session_for_schema(&self, schema: &str) -> Result<SessionId, CoreError> {
            let session = unsafe { (self.api.create_session)() };
            if session == 0 {
                return Err(CoreError::new(
                    ErrorCode::RimeInitializationFailed,
                    "librime could not create a session",
                ));
            }
            let schema_c = match CString::new(schema) {
                Ok(value) => value,
                Err(_) => {
                    unsafe {
                        (self.api.destroy_session)(session);
                    }
                    return Err(CoreError::new(
                        ErrorCode::InvalidRequest,
                        "Rime schema id contains NUL",
                    ));
                }
            };
            if unsafe { (self.api.select_schema)(session, schema_c.as_ptr()) } == 0 {
                unsafe {
                    (self.api.destroy_session)(session);
                }
                return Err(CoreError::new(
                    ErrorCode::RimeInitializationFailed,
                    format!("librime could not select schema {schema}"),
                ));
            }
            Ok(session)
        }

        fn stop_session(&mut self) {
            if self.session != 0 {
                unsafe {
                    (self.api.destroy_session)(self.session);
                }
                self.session = 0;
            }
        }

        fn with_session_stopped<T, F>(&mut self, operation: F) -> Result<T, CoreError>
        where
            F: FnOnce(&LeversApi) -> Result<T, CoreError>,
        {
            let levers = self.levers.ok_or_else(|| {
                CoreError::new(
                    ErrorCode::RimeInitializationFailed,
                    "librime levers module is unavailable",
                )
            })?;
            self.stop_session();
            let result = operation(&levers);
            let reopen = self.recreate_session();
            match (result, reopen) {
                (Ok(value), Ok(())) => Ok(value),
                (Err(error), _) => Err(error),
                (Ok(_), Err(error)) => Err(error),
            }
        }

        #[allow(dead_code)]
        pub(super) fn version(&self) -> Option<String> {
            if unsafe { !api_has(&self.api, &self.api.get_version) }
                || self.api.get_version as usize == 0
            {
                return None;
            }
            let value = unsafe { (self.api.get_version)() };
            (!value.is_null()).then(|| c_string(value))
        }
    }

    impl Drop for NativeBackend {
        fn drop(&mut self) {
            self.stop_session();
            unsafe {
                (self.api.finalize)();
            }
        }
    }

    impl Library {
        fn load(path: &Path) -> Result<Self, String> {
            let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
            let handle = unsafe { LoadLibraryW(wide.as_ptr()) };
            if handle.is_null() {
                Err(format!(
                    "LoadLibraryW failed for {}: {}",
                    path.display(),
                    std::io::Error::last_os_error()
                ))
            } else {
                Ok(Self { handle })
            }
        }
    }

    impl Drop for Library {
        fn drop(&mut self) {
            if !self.handle.is_null() {
                unsafe { FreeLibrary(self.handle) };
            }
        }
    }

    impl Api {
        unsafe fn load(module: *mut c_void) -> Result<Self, String> {
            type GetApi = unsafe extern "C" fn() -> *const Api;
            let get_api: GetApi = transmute_symbol(module, b"rime_get_api\0")?;
            let api_ptr = get_api();
            if api_ptr.is_null() {
                return Err("rime_get_api returned null".to_owned());
            }
            let data_size = ptr::read(api_ptr as *const c_int);
            let required_size =
                (std::mem::size_of::<Api>() - std::mem::size_of::<c_int>()) as c_int;
            if data_size < required_size {
                return Err(format!(
                    "librime API is too old (data_size={data_size}, required={required_size})"
                ));
            }
            let api = ptr::read(api_ptr);
            for (name, present) in [
                ("setup", api_has(&api, &api.setup)),
                ("initialize", api_has(&api, &api.initialize)),
                ("finalize", api_has(&api, &api.finalize)),
                ("start_maintenance", api_has(&api, &api.start_maintenance)),
                (
                    "join_maintenance_thread",
                    api_has(&api, &api.join_maintenance_thread),
                ),
                ("create_session", api_has(&api, &api.create_session)),
                ("destroy_session", api_has(&api, &api.destroy_session)),
                ("process_key", api_has(&api, &api.process_key)),
                ("select_schema", api_has(&api, &api.select_schema)),
                ("set_input", api_has(&api, &api.set_input)),
                ("get_context", api_has(&api, &api.get_context)),
                ("free_context", api_has(&api, &api.free_context)),
                ("select_candidate", api_has(&api, &api.select_candidate)),
                ("commit_composition", api_has(&api, &api.commit_composition)),
                ("get_commit", api_has(&api, &api.get_commit)),
                ("free_commit", api_has(&api, &api.free_commit)),
                (
                    "candidate_list_begin",
                    api_has(&api, &api.candidate_list_begin),
                ),
                (
                    "candidate_list_next",
                    api_has(&api, &api.candidate_list_next),
                ),
                ("candidate_list_end", api_has(&api, &api.candidate_list_end)),
                ("find_module", api_has(&api, &api.find_module)),
                (
                    "highlight_candidate",
                    api_has(&api, &api.highlight_candidate),
                ),
            ] {
                if !present {
                    return Err(format!("librime API field {name} is unavailable"));
                }
            }
            Ok(api)
        }
    }

    unsafe fn load_levers(api: &Api) -> Result<Option<LeversApi>, String> {
        let name = CString::new("levers").unwrap();
        let module = (api.find_module)(name.as_ptr());
        if module.is_null() {
            return Ok(None);
        }
        let module_size = ptr::read(module as *const c_int);
        let module_required =
            (std::mem::size_of::<Module>() - std::mem::size_of::<c_int>()) as c_int;
        if module_size < module_required {
            return Err(format!(
                "librime levers module is too old (data_size={module_size})"
            ));
        }
        let module = &*module;
        if module.get_api as usize == 0 {
            return Ok(None);
        }
        let levers = (module.get_api)();
        if levers.is_null() {
            return Ok(None);
        }
        let levers_size = ptr::read(levers as *const c_int);
        let required_size =
            (std::mem::size_of::<LeversApi>() - std::mem::size_of::<c_int>()) as c_int;
        if levers_size < required_size {
            return Err(format!(
                "librime levers API is too old (data_size={levers_size})"
            ));
        }
        Ok(Some(ptr::read(levers as *const LeversApi)))
    }

    unsafe fn api_has<T>(api: &Api, field: &T) -> bool {
        let end = field as *const T as usize + std::mem::size_of::<T>();
        end.saturating_sub(api as *const Api as usize)
            <= std::mem::size_of::<c_int>() + api.data_size.max(0) as usize
    }

    unsafe fn transmute_symbol<T>(module: *mut c_void, name: &[u8]) -> Result<T, String> {
        let ptr = GetProcAddress(module, name.as_ptr() as *const c_char);
        if ptr.is_null() {
            Err(format!(
                "missing librime symbol {}",
                String::from_utf8_lossy(name)
            ))
        } else {
            Ok(std::mem::transmute_copy(&ptr))
        }
    }

    fn c_path(path: &Path) -> Result<CString, String> {
        CString::new(path.to_string_lossy().as_bytes()).map_err(|_| "path contains NUL".to_owned())
    }
    fn absolute(path: &Path) -> Result<PathBuf, String> {
        if path.is_absolute() {
            Ok(path.to_owned())
        } else {
            std::env::current_dir()
                .map_err(|e| e.to_string())
                .map(|cwd| cwd.join(path))
        }
    }
    fn c_string(value: *const c_char) -> String {
        if value.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(value).to_string_lossy().into_owned() }
        }
    }

    fn has_bin_files(path: &Path) -> bool {
        path.is_dir()
            && std::fs::read_dir(path)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .any(|entry| {
                    entry
                        .file_type()
                        .map(|kind| kind.is_file())
                        .unwrap_or(false)
                        && entry.path().extension().and_then(|value| value.to_str()) == Some("bin")
                })
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
        fn FreeLibrary(module: *mut c_void) -> i32;
    }
}

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

    fn process_key(&mut self, keycode: i32, mask: i32) -> Result<RimeKeyResult, CoreError> {
        RimeEngine::process_key(self, keycode, mask)
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
