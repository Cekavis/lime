//! Runtime corpus loading, segmentation, pinyin extraction, and streaming case generation.
//!
//! A corpus is a directory whose immediate child directories are categories and whose
//! immediate `.txt` files are articles. The loader intentionally keeps source bytes out of the
//! application binary: callers provide the directory at runtime and can use the resulting
//! SHA-256 fingerprint to invalidate persisted benchmark results.

use crate::{
    is_chinese_character, validate_case, Case, Dataset, MAX_CONTEXT_CHARS, MAX_TARGET_CHARS,
};
use jieba_rs::{Jieba, TokenizeMode};
use pinyin::{Pinyin, ToPinyin};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The algorithm/data contract version included in corpus metadata and fingerprints.
pub const CORPUS_VERSION: u32 = 3;
// Keep this identity aligned with the processor behavior and the exact versions pinned in
// Cargo.toml. The phrase-dictionary marker invalidates old results when lookup behavior changes;
// the loaded Rime dictionary contents are intentionally not part of the corpus fingerprint.
const PROCESSOR_FINGERPRINT: &str =
    "jieba-rs=0.7.0/default-dict/hmm=true;pinyin=0.11.0/plain;phrase-pronunciation=1;han-runs=1;first-word=1";
const DEFAULT_CONTEXT_CHARS: usize = 128;
const DEFAULT_DATASET_ID: &str = "benchmark";
const DEFAULT_DATASET_NAME: &str = "语料";

/// Small corpus directory suitable for management IPC; contains no source text.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DatasetInfo {
    pub id: String,
    pub name: String,
    pub version: u32,
    /// Absolute directory containing category folders. Never contains source text.
    #[serde(default)]
    pub directory: String,
    /// Refresh errors are populated by the service while preserving directory metadata.
    #[serde(default)]
    pub error: Option<String>,
    /// SHA-256 identity of the complete runtime corpus directory.
    #[serde(default)]
    pub sha256: String,
    pub corpora: Vec<CorpusInfo>,
}

/// Chinese-character count, article count, and target-word count for a corpus category.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CorpusInfo {
    pub id: String,
    pub name: String,
    pub characters: usize,
    pub cases: usize,
    /// Number of direct `.txt` files in the category directory.
    #[serde(default)]
    pub articles: usize,
    /// Identity of this category, independent of unrelated category contents.
    #[serde(default)]
    pub sha256: String,
}

#[derive(Clone, Debug)]
struct Article {
    id: String,
    category: String,
    text: String,
    words: Vec<Word>,
}

#[derive(Clone, Debug)]
struct Word {
    start: usize,
    end: usize,
    syllables: Vec<String>,
}

/// Phrase-level pronunciations loaded from the bundled Rime dictionary.
///
/// The dictionary is only a pronunciation aid for corpus loading. Its contents are deliberately
/// not part of the corpus fingerprint; the versioned processor marker above invalidates results
/// when the lookup behavior changes, while an unavailable dictionary simply falls back to the
/// single-character readings.
#[derive(Clone, Debug, Default)]
pub struct PinyinDictionary {
    entries: HashMap<String, DictionaryEntry>,
}

#[derive(Clone, Debug)]
struct DictionaryEntry {
    syllables: Vec<String>,
    weight: i64,
}

impl PinyinDictionary {
    /// Load phrase pronunciations from the raw Rime-Ice `cn_dicts` directory when available.
    /// Missing or malformed files are ignored so benchmark loading remains usable with a
    /// compiled-only Rime resource package.
    pub fn from_rime_dir(rime_dir: Option<&Path>) -> Self {
        let mut dictionary = Self::default();
        let Some(rime_dir) = rime_dir else {
            return dictionary;
        };
        let dict_dir = rime_dir.join("cn_dicts");
        let Ok(entries) = fs::read_dir(dict_dir) else {
            return dictionary;
        };
        let mut paths = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file()
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.ends_with(".dict.yaml"))
            })
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            dictionary.load_file(&path);
        }
        dictionary
    }

    fn load_file(&mut self, path: &Path) {
        let Ok(text) = fs::read_to_string(path) else {
            return;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || !line.contains('\t') {
                continue;
            }
            let mut fields = line.split('\t');
            let Some(word) = fields.next().map(str::trim) else {
                continue;
            };
            let Some(pinyin) = fields.next().map(str::trim) else {
                continue;
            };
            if word.chars().count() < 2 || !word.chars().all(is_chinese_character) {
                continue;
            }
            let syllables = pinyin
                .split_whitespace()
                .map(normalize_dictionary_syllable)
                .collect::<Option<Vec<_>>>();
            let Some(syllables) = syllables else {
                continue;
            };
            if syllables.len() != word.chars().count() {
                continue;
            }
            let weight = fields
                .next()
                .and_then(|value| value.trim().parse::<i64>().ok())
                .unwrap_or_default();
            let replace = self
                .entries
                .get(word)
                .map_or(true, |entry| weight > entry.weight);
            if replace {
                self.entries
                    .insert(word.to_owned(), DictionaryEntry { syllables, weight });
            }
        }
    }

    fn lookup(&self, word: &str) -> Option<&[String]> {
        self.entries
            .get(word)
            .map(|entry| entry.syllables.as_slice())
    }
}

/// A loaded runtime corpus. Loading reads and validates all article bytes and performs
/// segmentation/pinyin conversion once; case generation remains streaming.
#[derive(Clone, Debug)]
pub struct Corpus {
    root: PathBuf,
    info: DatasetInfo,
    fingerprint: String,
    articles: Vec<Article>,
}

impl Corpus {
    /// Load all category folders and `.txt` articles below `root`.
    pub fn load(root: impl AsRef<Path>) -> Result<Self, String> {
        Self::load_with_pinyin_dictionary(root, &PinyinDictionary::default())
    }

    /// Load all category folders and `.txt` articles using phrase pronunciations when present.
    pub fn load_with_pinyin_dictionary(
        root: impl AsRef<Path>,
        pinyin_dictionary: &PinyinDictionary,
    ) -> Result<Self, String> {
        let root = if root.as_ref().is_absolute() {
            root.as_ref().to_path_buf()
        } else {
            env::current_dir()
                .map_err(|error| format!("failed to resolve corpus directory: {error}"))?
                .join(root)
        };
        // The default user directory is created on first access so opening the
        // benchmark page never fails solely because the user has not added a
        // corpus yet. The subsequent empty-directory validation still reports
        // that a category folder is required.
        fs::create_dir_all(&root).map_err(|error| {
            format!(
                "failed to create corpus directory {}: {error}",
                root.display()
            )
        })?;
        let metadata = fs::metadata(&root).map_err(|error| {
            format!(
                "failed to inspect corpus directory {}: {error}",
                root.display()
            )
        })?;
        if !metadata.is_dir() {
            return Err(format!(
                "corpus path is not a directory: {}",
                root.display()
            ));
        }

        let mut category_dirs = read_sorted_directories(&root)?;
        if category_dirs.is_empty() {
            return Err(format!(
                "corpus directory has no category folders: {}",
                root.display()
            ));
        }

        let jieba = tokenizer();
        let mut articles = Vec::new();
        let mut corpora = Vec::with_capacity(category_dirs.len());
        let mut hasher = fingerprint_hasher(PROCESSOR_FINGERPRINT);

        for category_dir in category_dirs.drain(..) {
            let category = file_name(&category_dir)?;
            if category.trim().is_empty() {
                return Err("corpus category name must not be empty".to_owned());
            }
            let files = read_sorted_text_files(&category_dir)?;
            if files.is_empty() {
                return Err(format!(
                    "corpus category has no .txt articles: {}",
                    category_dir.display()
                ));
            }
            hash_path_component(&mut hasher, &category);
            let mut category_hasher = fingerprint_hasher(PROCESSOR_FINGERPRINT);
            hash_path_component(&mut category_hasher, &category);
            let mut category_characters = 0usize;
            let mut category_cases = 0usize;
            let mut category_articles = 0usize;
            for path in files {
                let bytes = fs::read(&path).map_err(|error| {
                    format!("failed to read corpus article {}: {error}", path.display())
                })?;
                let text = std::str::from_utf8(&bytes).map_err(|error| {
                    format!(
                        "corpus article is not valid UTF-8 {}: {error}",
                        path.display()
                    )
                })?;
                if text.is_empty() {
                    return Err(format!("corpus article is empty: {}", path.display()));
                }
                let file = file_name(&path)?;
                let relative_path = format!("{category}/{file}");
                hash_path_component(&mut hasher, &relative_path);
                hash_bytes(&mut hasher, &bytes);
                hash_path_component(&mut category_hasher, &relative_path);
                hash_bytes(&mut category_hasher, &bytes);

                let article = segment_article(
                    &jieba,
                    category.clone(),
                    relative_path,
                    text.to_owned(),
                    pinyin_dictionary,
                )
                .map_err(|error| format!("invalid corpus article {}: {error}", path.display()))?;
                let characters = article
                    .text
                    .chars()
                    .filter(|character| is_chinese_character(*character))
                    .count();
                if article.words.is_empty() || characters == 0 {
                    return Err(format!(
                        "corpus article contains no Chinese target words: {}",
                        path.display()
                    ));
                }
                category_characters += characters;
                category_cases += article.words.len();
                category_articles += 1;
                articles.push(article);
            }
            corpora.push(CorpusInfo {
                id: category.clone(),
                name: category,
                characters: category_characters,
                cases: category_cases,
                articles: category_articles,
                sha256: format!("{:x}", category_hasher.finalize()),
            });
        }

        let fingerprint = format!("{:x}", hasher.finalize());
        let info = DatasetInfo {
            id: DEFAULT_DATASET_ID.to_owned(),
            name: DEFAULT_DATASET_NAME.to_owned(),
            version: CORPUS_VERSION,
            directory: root.to_string_lossy().into_owned(),
            error: None,
            sha256: fingerprint.clone(),
            corpora,
        };
        Ok(Self {
            root,
            info,
            fingerprint,
            articles,
        })
    }

    /// The root directory used for this load.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Return category/count metadata without source text.
    pub fn info(&self) -> &DatasetInfo {
        &self.info
    }

    /// Return the complete corpus SHA-256 fingerprint.
    pub fn sha256(&self) -> &str {
        &self.fingerprint
    }

    /// Build a materialized dataset using all categories and at most `context_limit` characters.
    pub fn dataset_with_context_limit(&self, context_limit: usize) -> Result<Dataset, String> {
        self.dataset_with_context_limit_for_categories(context_limit, None)
    }

    /// Build a materialized dataset for selected category IDs.
    pub fn dataset_with_context_limit_for_categories(
        &self,
        context_limit: usize,
        categories: Option<&[String]>,
    ) -> Result<Dataset, String> {
        let mut cases = Vec::new();
        self.visit_cases(context_limit, categories, |case| {
            cases.push(case);
            Ok(())
        })?;
        if cases.is_empty() {
            return Err("selected corpus categories contain no cases".to_owned());
        }
        Ok(Dataset {
            id: self.info.id.clone(),
            name: self.info.name.clone(),
            version: self.info.version,
            cases,
        })
    }

    /// Stream cases for all or selected category IDs without retaining repeated contexts.
    pub fn visit_cases(
        &self,
        context_limit: usize,
        categories: Option<&[String]>,
        mut visitor: impl FnMut(Case) -> Result<(), String>,
    ) -> Result<(), String> {
        validate_context_limit(context_limit)?;
        validate_categories(&self.info, categories)?;
        for article in &self.articles {
            if !category_selected(&article.category, categories) {
                continue;
            }
            visit_article(article, context_limit, &mut visitor)?;
        }
        Ok(())
    }

    /// Convenience form of [`Self::visit_cases`] for an explicit category slice.
    pub fn visit_selected_cases(
        &self,
        context_limit: usize,
        categories: &[String],
        visitor: impl FnMut(Case) -> Result<(), String>,
    ) -> Result<(), String> {
        self.visit_cases(context_limit, Some(categories), visitor)
    }

    /// Convenience form of [`Self::dataset_with_context_limit_for_categories`] for an explicit
    /// category slice.
    pub fn dataset_for_categories(
        &self,
        context_limit: usize,
        categories: &[String],
    ) -> Result<Dataset, String> {
        self.dataset_with_context_limit_for_categories(context_limit, Some(categories))
    }
}

/// Return the configured runtime corpus directory. `LIME_BENCHMARK_CORPUS_DIR` is useful for
/// development/tests; normal Windows installs use `%LOCALAPPDATA%\\Lime\\benchmark\\corpora`.
pub fn default_corpus_dir() -> PathBuf {
    if let Some(path) = env::var_os("LIME_BENCHMARK_CORPUS_DIR") {
        return PathBuf::from(path);
    }
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local_app_data)
            .join("Lime")
            .join("benchmark")
            .join("corpora");
    }
    if let Some(data_home) = env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(data_home)
            .join("Lime")
            .join("benchmark")
            .join("corpora");
    }
    PathBuf::from(".lime").join("benchmark").join("corpora")
}

/// Load a runtime corpus from `root`.
pub fn load_corpus(root: impl AsRef<Path>) -> Result<Corpus, String> {
    Corpus::load(root)
}

/// Load a runtime corpus using phrase pronunciations when available.
pub fn load_corpus_with_pinyin_dictionary(
    root: impl AsRef<Path>,
    pinyin_dictionary: &PinyinDictionary,
) -> Result<Corpus, String> {
    Corpus::load_with_pinyin_dictionary(root, pinyin_dictionary)
}

/// Load a corpus and return its directory metadata.
pub fn corpus_info(root: impl AsRef<Path>) -> Result<DatasetInfo, String> {
    Ok(Corpus::load(root)?.info().clone())
}

/// Load a corpus and return its complete SHA-256 fingerprint.
pub fn corpus_sha256(root: impl AsRef<Path>) -> Result<String, String> {
    Ok(Corpus::load(root)?.sha256().to_owned())
}

/// Load every runtime case with at most 128 preceding Unicode characters.
pub fn dataset_from_corpus_dir(root: impl AsRef<Path>) -> Result<Dataset, String> {
    dataset_from_corpus_dir_with_context_limit(root, DEFAULT_CONTEXT_CHARS)
}

/// Load runtime cases with an explicit context limit.
pub fn dataset_from_corpus_dir_with_context_limit(
    root: impl AsRef<Path>,
    context_limit: usize,
) -> Result<Dataset, String> {
    Corpus::load(root)?.dataset_with_context_limit(context_limit)
}

/// Load selected runtime categories with an explicit context limit.
pub fn dataset_from_corpus_dir_for_categories(
    root: impl AsRef<Path>,
    context_limit: usize,
    categories: &[String],
) -> Result<Dataset, String> {
    Corpus::load(root)?.dataset_for_categories(context_limit, categories)
}

/// Stream runtime cases for all or selected categories.
pub fn visit_corpus_cases(
    root: impl AsRef<Path>,
    context_limit: usize,
    categories: Option<&[String]>,
    visitor: impl FnMut(Case) -> Result<(), String>,
) -> Result<(), String> {
    Corpus::load(root)?.visit_cases(context_limit, categories, visitor)
}

/// Stream runtime cases for an explicit category slice.
pub fn visit_selected_corpus_cases(
    root: impl AsRef<Path>,
    context_limit: usize,
    categories: &[String],
    visitor: impl FnMut(Case) -> Result<(), String>,
) -> Result<(), String> {
    Corpus::load(root)?.visit_selected_cases(context_limit, categories, visitor)
}

/// Legacy compatibility wrapper. It now reads the configured runtime directory instead of
/// embedding generated source snapshots in the executable.
pub fn builtin_dataset() -> Result<Dataset, String> {
    dataset_from_corpus_dir(default_corpus_dir())
}

/// Legacy compatibility wrapper with an explicit context limit.
pub fn builtin_dataset_with_context_limit(context_limit: usize) -> Result<Dataset, String> {
    dataset_from_corpus_dir_with_context_limit(default_corpus_dir(), context_limit)
}

/// Legacy compatibility wrapper returning runtime corpus metadata.
pub fn builtin_dataset_info() -> Result<DatasetInfo, String> {
    corpus_info(default_corpus_dir())
}

/// Legacy compatibility wrapper returning runtime corpus fingerprint.
pub fn builtin_dataset_sha256() -> Result<String, String> {
    corpus_sha256(default_corpus_dir())
}

/// Legacy compatibility wrapper streaming all runtime cases.
pub fn visit_builtin_cases(
    context_limit: usize,
    visitor: impl FnMut(Case) -> Result<(), String>,
) -> Result<(), String> {
    visit_corpus_cases(default_corpus_dir(), context_limit, None, visitor)
}

fn validate_context_limit(context_limit: usize) -> Result<(), String> {
    if !(1..=MAX_CONTEXT_CHARS).contains(&context_limit) {
        return Err(format!(
            "benchmark context limit must be between 1 and {MAX_CONTEXT_CHARS} characters"
        ));
    }
    Ok(())
}

fn validate_categories(info: &DatasetInfo, categories: Option<&[String]>) -> Result<(), String> {
    let Some(categories) = categories else {
        return Ok(());
    };
    let available: HashSet<_> = info.corpora.iter().map(|item| item.id.as_str()).collect();
    let mut seen = HashSet::with_capacity(categories.len());
    for category in categories {
        if !available.contains(category.as_str()) {
            return Err(format!("unknown corpus category: {category}"));
        }
        if !seen.insert(category.as_str()) {
            return Err(format!("duplicate corpus category: {category}"));
        }
    }
    Ok(())
}

fn category_selected(category: &str, categories: Option<&[String]>) -> bool {
    categories.map_or(true, |selected| {
        selected.iter().any(|value| value == category)
    })
}

fn read_sorted_directories(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut entries = fs::read_dir(root)
        .map_err(|error| {
            format!(
                "failed to list corpus directory {}: {error}",
                root.display()
            )
        })?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|error| format!("failed to read corpus directory entry: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    entries.retain(|path| path.is_dir());
    entries.sort_by(|left, right| {
        file_name(left)
            .unwrap_or_default()
            .cmp(&file_name(right).unwrap_or_default())
    });
    Ok(entries)
}

fn read_sorted_text_files(category_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut entries = fs::read_dir(category_dir)
        .map_err(|error| {
            format!(
                "failed to list corpus category {}: {error}",
                category_dir.display()
            )
        })?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|error| format!("failed to read corpus category entry: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    entries.retain(|path| {
        path.is_file()
            && path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    });
    entries.sort_by(|left, right| {
        file_name(left)
            .unwrap_or_default()
            .cmp(&file_name(right).unwrap_or_default())
    });
    Ok(entries)
}

fn file_name(path: &Path) -> Result<String, String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("corpus path is not valid UTF-8: {}", path.display()))
}

fn segment_article(
    jieba: &Jieba,
    category: String,
    relative_path: String,
    text: String,
    pinyin_dictionary: &PinyinDictionary,
) -> Result<Article, String> {
    let id = relative_path.clone();
    let char_count = text.chars().count();
    let mut words = Vec::new();
    let mut previous_end = 0usize;
    for token in jieba.tokenize(&text, TokenizeMode::Default, true) {
        if token.start != previous_end || token.end <= token.start || token.end > char_count {
            return Err(format!(
                "invalid segmentation boundaries in corpus article {id}"
            ));
        }
        let token_chars: Vec<char> = token.word.chars().collect();
        let mut run_start = 0usize;
        let mut run_is_chinese = token_chars
            .first()
            .copied()
            .is_some_and(is_chinese_character);
        for (offset, character) in token_chars.iter().copied().enumerate().skip(1) {
            let character_is_chinese = is_chinese_character(character);
            if character_is_chinese != run_is_chinese {
                if run_is_chinese {
                    append_word(
                        &mut words,
                        &token_chars[run_start..offset],
                        token.start + run_start,
                        token.start + offset,
                        &id,
                        pinyin_dictionary,
                    )?;
                }
                run_start = offset;
                run_is_chinese = character_is_chinese;
            }
        }
        if run_is_chinese {
            append_word(
                &mut words,
                &token_chars[run_start..],
                token.start + run_start,
                token.end,
                &id,
                pinyin_dictionary,
            )?;
        }
        previous_end = token.end;
    }
    if previous_end != char_count {
        return Err(format!("segmentation did not cover corpus article {id}"));
    }
    Ok(Article {
        id,
        category,
        text,
        words,
    })
}

fn append_word(
    words: &mut Vec<Word>,
    characters: &[char],
    start: usize,
    end: usize,
    article_id: &str,
    pinyin_dictionary: &PinyinDictionary,
) -> Result<(), String> {
    if characters.is_empty() || !characters.iter().copied().all(is_chinese_character) {
        return Err(format!(
            "mixed token split produced invalid word in corpus article {article_id}"
        ));
    }
    if characters.len() > MAX_TARGET_CHARS {
        return Err(format!(
            "target word has {} characters; the maximum is {MAX_TARGET_CHARS} in corpus article {article_id}",
            characters.len()
        ));
    }
    let word: String = characters.iter().collect();
    let syllables = if let Some(syllables) = pinyin_dictionary.lookup(&word) {
        syllables.to_vec()
    } else {
        let mut syllables = Vec::with_capacity(characters.len());
        for character in characters {
            let pinyin = character.to_pinyin().ok_or_else(|| {
                format!("missing pinyin for {character} in corpus article {article_id}")
            })?;
            syllables.push(normalize_pinyin(pinyin));
        }
        syllables
    };
    // Validate target alignment and pinyin while the source file is still available to the
    // caller. The first-word context is filled in later; a one-character placeholder validates
    // the same target/syllable contract without making the article's first word special here.
    validate_case(&Case {
        id: article_id.to_owned(),
        category: "corpus".to_owned(),
        context: "前".to_owned(),
        expected: characters.iter().collect(),
        syllables: syllables.clone(),
        first_word: false,
    })?;
    words.push(Word {
        start,
        end,
        syllables,
    });
    Ok(())
}

fn normalize_pinyin(pinyin: Pinyin) -> String {
    pinyin.plain().replace('ü', "v")
}

fn normalize_dictionary_syllable(value: &str) -> Option<String> {
    let value = value.replace('ü', "v");
    (!value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_lowercase()))
    .then_some(value)
}

fn visit_article(
    article: &Article,
    context_limit: usize,
    visitor: &mut impl FnMut(Case) -> Result<(), String>,
) -> Result<(), String> {
    let chars: Vec<char> = article.text.chars().collect();
    for (index, word) in article.words.iter().enumerate() {
        let first_word = index == 0;
        let context = if first_word {
            String::new()
        } else {
            let start = word.start.saturating_sub(context_limit);
            chars[start..word.start].iter().collect()
        };
        let case = Case {
            id: format!("{}#{:06}", article.id, index + 1),
            category: article.category.clone(),
            context,
            expected: chars[word.start..word.end].iter().collect(),
            syllables: word.syllables.clone(),
            first_word,
        };
        validate_case(&case)?;
        visitor(case)?;
    }
    Ok(())
}

fn hash_path_component(hasher: &mut Sha256, value: &str) {
    let normalized = value.replace('\\', "/");
    hasher.update((normalized.len() as u64).to_le_bytes());
    hasher.update(normalized.as_bytes());
}

fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn fingerprint_hasher(scope: &str) -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(b"lime-benchmark-corpus\0");
    hasher.update(CORPUS_VERSION.to_le_bytes());
    hash_path_component(&mut hasher, scope);
    hasher
}

fn tokenizer() -> &'static Jieba {
    static TOKENIZER: OnceLock<Jieba> = OnceLock::new();
    TOKENIZER.get_or_init(Jieba::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempCorpus(PathBuf);

    impl TempCorpus {
        fn new() -> Self {
            let id = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("lime-benchmark-corpus-{id}"));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn write(&self, category: &str, file: &str, text: &str) {
            let path = self.0.join(category);
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join(file), text.as_bytes()).unwrap();
        }
    }

    impl Drop for TempCorpus {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn loads_sorted_categories_and_articles_and_counts_first_words() {
        let corpus = TempCorpus::new();
        corpus.write("zeta", "b.txt", "A股很好");
        corpus.write("zeta", "a.txt", "你好世界");
        corpus.write("alpha", "a.txt", "春风沉醉");
        let loaded = Corpus::load(&corpus.0).unwrap();
        assert_eq!(
            loaded
                .info()
                .corpora
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "zeta"]
        );
        assert_eq!(loaded.info().corpora[0].articles, 1);
        assert_eq!(loaded.info().corpora[0].cases, 2);
        let mut cases = Vec::new();
        loaded
            .visit_cases(128, None, |case| {
                cases.push(case);
                Ok(())
            })
            .unwrap();
        assert_eq!(cases[0].category, "alpha");
        assert!(cases[0].first_word);
        assert_eq!(cases[0].context, "");
        assert_eq!(cases[1].expected, "沉醉");
        assert!(!cases[1].first_word);
        assert_eq!(cases[2].category, "zeta");
        assert_eq!(cases[2].expected, "你好");
    }

    #[test]
    fn mixed_tokens_are_split_and_non_chinese_prefix_is_preserved_after_first_word() {
        let corpus = TempCorpus::new();
        corpus.write("mixed", "article.txt", "A股2026年好");
        let loaded = Corpus::load(&corpus.0).unwrap();
        let mut cases = Vec::new();
        loaded
            .visit_cases(128, None, |case| {
                cases.push(case);
                Ok(())
            })
            .unwrap();
        assert_eq!(
            cases
                .iter()
                .map(|case| case.expected.as_str())
                .collect::<Vec<_>>(),
            ["股", "年", "好"]
        );
        assert_eq!(cases[0].context, "");
        assert_eq!(cases[1].context, "A股2026");
        assert_eq!(cases[2].context, "A股2026年");
        assert_eq!(cases[0].syllables, ["gu"]);
    }

    #[test]
    fn rime_phrase_dictionary_resolves_polyphonic_words_and_keeps_fallback() {
        let corpus = TempCorpus::new();
        corpus.write("places", "article.txt", "西藏尼泊尔重庆银行甲");
        let rime_dir = std::env::temp_dir().join(format!(
            "lime-benchmark-rime-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let dict_dir = rime_dir.join("cn_dicts");
        fs::create_dir_all(&dict_dir).unwrap();
        fs::write(
            dict_dir.join("base.dict.yaml"),
            "# Rime dictionary\n---\n西藏\txi zang\t10\n尼泊尔\tni bo er\t20\n重庆\tchong qing\t30\n银行\tyin hang\t40\n",
        )
        .unwrap();

        let dictionary = PinyinDictionary::from_rime_dir(Some(&rime_dir));
        let loaded = Corpus::load_with_pinyin_dictionary(&corpus.0, &dictionary).unwrap();
        let loaded_without_dictionary = Corpus::load(&corpus.0).unwrap();
        assert_eq!(loaded.sha256(), loaded_without_dictionary.sha256());
        let mut cases = Vec::new();
        loaded
            .visit_cases(128, None, |case| {
                cases.push(case);
                Ok(())
            })
            .unwrap();
        assert_eq!(cases[0].expected, "西藏");
        assert_eq!(cases[0].syllables, ["xi", "zang"]);
        assert_eq!(cases[1].expected, "尼泊尔");
        assert_eq!(cases[1].syllables, ["ni", "bo", "er"]);
        assert_eq!(cases[2].expected, "重庆");
        assert_eq!(cases[2].syllables, ["chong", "qing"]);
        assert_eq!(cases[3].expected, "银行");
        assert_eq!(cases[3].syllables, ["yin", "hang"]);
        assert_eq!(cases[4].expected, "甲");
        assert_eq!(cases[4].syllables, ["jia"]);
        fs::remove_dir_all(rime_dir).unwrap();
    }

    #[test]
    fn selected_categories_and_fingerprint_are_stable() {
        let corpus = TempCorpus::new();
        corpus.write("b", "x.txt", "你好世界");
        corpus.write("a", "x.txt", "春风沉醉");
        let loaded = Corpus::load(&corpus.0).unwrap();
        let selected = vec!["b".to_owned()];
        let mut cases = Vec::new();
        loaded
            .visit_cases(1, Some(&selected), |case| {
                cases.push(case);
                Ok(())
            })
            .unwrap();
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[1].context, "好");
        let a_sha = loaded
            .info()
            .corpora
            .iter()
            .find(|item| item.id == "a")
            .unwrap()
            .sha256
            .clone();
        let first = loaded.sha256().to_owned();
        fs::write(corpus.0.join("b").join("x.txt"), "你好变化").unwrap();
        let reloaded = Corpus::load(&corpus.0).unwrap();
        let second = reloaded.sha256().to_owned();
        assert_ne!(first, second);
        assert_eq!(
            reloaded
                .info()
                .corpora
                .iter()
                .find(|item| item.id == "a")
                .unwrap()
                .sha256,
            a_sha
        );
        assert!(loaded
            .visit_cases(1, Some(&["missing".to_owned()]), |_| Ok(()))
            .is_err());
        assert!(loaded
            .visit_cases(1, Some(&["a".to_owned(), "a".to_owned()]), |_| Ok(()))
            .is_err());
    }

    #[test]
    fn rejects_invalid_utf8_and_articles_without_chinese_text() {
        let corpus = TempCorpus::new();
        corpus.write("bad", "bad.txt", "hello");
        assert!(Corpus::load(&corpus.0).is_err());
        let path = corpus.0.join("bad").join("bad.txt");
        fs::write(path, [0xff, 0xfe]).unwrap();
        assert!(Corpus::load(&corpus.0).is_err());
    }

    #[test]
    fn rejects_long_target_with_the_full_article_path() {
        let path = std::env::temp_dir().join("lime-benchmark-bad-long.txt");
        let characters: Vec<char> = "中".repeat(MAX_TARGET_CHARS + 1).chars().collect();
        let error = append_word(
            &mut Vec::new(),
            &characters,
            0,
            characters.len(),
            &path.display().to_string(),
            &PinyinDictionary::default(),
        )
        .unwrap_err();
        assert!(error.contains(&path.display().to_string()));
        assert!(error.contains("maximum is 32"));
    }

    #[test]
    fn visitor_errors_are_propagated_and_context_limit_is_checked() {
        let corpus = TempCorpus::new();
        corpus.write("test", "a.txt", "你好世界");
        let loaded = Corpus::load(&corpus.0).unwrap();
        assert!(loaded.visit_cases(0, None, |_| Ok(())).is_err());
        let error = loaded.visit_cases(3, None, |_| Err("cancelled".to_owned()));
        assert_eq!(error, Err("cancelled".to_owned()));
    }

    #[test]
    fn creates_missing_corpus_directory_before_reporting_empty_corpus() {
        let root = std::env::temp_dir().join(format!(
            "lime-benchmark-missing-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(!root.exists());
        let error = Corpus::load(&root).unwrap_err();
        assert!(root.is_dir());
        assert!(error.contains("no category folders"));
        fs::remove_dir_all(root).unwrap();
    }
}
