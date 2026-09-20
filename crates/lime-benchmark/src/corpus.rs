//! Frozen source text, segmentation and pinyin. Runtime evaluation needs no Python.

use crate::{is_chinese_character, validate_case, Case, Dataset, MAX_CONTEXT_CHARS};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::sync::OnceLock;

const CORPUS_JSON: &str = include_str!("../data/corpus.json");
const DEFAULT_CONTEXT_CHARS: usize = 128;

/// Small corpus directory suitable for management IPC; contains no source text.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DatasetInfo {
    pub id: String,
    pub name: String,
    pub version: u32,
    pub corpora: Vec<CorpusInfo>,
}

/// Chinese-character count and eligible word count for a corpus category.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CorpusInfo {
    pub id: String,
    pub name: String,
    pub characters: usize,
    pub cases: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenCorpus {
    id: String,
    name: String,
    version: u32,
    generator: serde_json::Value,
    corpora: Vec<CorpusInfo>,
    documents: Vec<Document>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    id: String,
    category: String,
    source_id: String,
    source_start: usize,
    source_end: usize,
    text: String,
    // Unicode scalar offsets and pinyin; non-Chinese tokens have no syllables.
    words: Vec<(usize, usize, Vec<String>)>,
}

/// Load every bundled case with at most 128 preceding Unicode characters.
///
/// Prefer [`visit_builtin_cases`] for large runs, to avoid retaining repeated prefixes.
pub fn builtin_dataset() -> Result<Dataset, String> {
    builtin_dataset_with_context_limit(DEFAULT_CONTEXT_CHARS)
}

/// Load all bundled cases with the requested amount of actual preceding text.
pub fn builtin_dataset_with_context_limit(context_limit: usize) -> Result<Dataset, String> {
    let info = builtin_dataset_info()?;
    let mut cases = Vec::with_capacity(info.corpora.iter().map(|corpus| corpus.cases).sum());
    visit_builtin_cases(context_limit, |case| {
        cases.push(case);
        Ok(())
    })?;
    Ok(Dataset {
        id: info.id,
        name: info.name,
        version: info.version,
        cases,
    })
}

/// Return the small directory without expanding per-word context strings.
pub fn builtin_dataset_info() -> Result<DatasetInfo, String> {
    let corpus = frozen_corpus()?;
    Ok(DatasetInfo {
        id: corpus.id.clone(),
        name: corpus.name.clone(),
        version: corpus.version,
        corpora: corpus.corpora.clone(),
    })
}

/// Fingerprint the frozen text, word boundaries, pinyin and source-manifest digest.
///
/// This identity is independent of the context limit selected for a benchmark row.
pub fn builtin_dataset_sha256() -> Result<String, String> {
    frozen_corpus()?;
    Ok(format!("{:x}", Sha256::digest(CORPUS_JSON.as_bytes())))
}

/// Visit every eligible Chinese word once, using only text preceding that word.
///
/// Each article/question block starts a new context. The first Chinese word is
/// skipped. Punctuation, letters, numbers and whitespace remain in later prefixes.
/// The visitor error is propagated immediately, allowing a caller to cancel a run.
pub fn visit_builtin_cases(
    context_limit: usize,
    mut visitor: impl FnMut(Case) -> Result<(), String>,
) -> Result<(), String> {
    if !(1..=MAX_CONTEXT_CHARS).contains(&context_limit) {
        return Err(format!(
            "benchmark context limit must be between 1 and {MAX_CONTEXT_CHARS} characters"
        ));
    }
    for document in &frozen_corpus()?.documents {
        visit_document(document, context_limit, &mut visitor)?;
    }
    Ok(())
}

fn visit_document(
    document: &Document,
    context_limit: usize,
    visitor: &mut impl FnMut(Case) -> Result<(), String>,
) -> Result<(), String> {
    let offsets: Vec<_> = document
        .text
        .char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(document.text.len()))
        .collect();
    let mut first_chinese = true;
    for (index, (start, end, syllables)) in document.words.iter().enumerate() {
        if syllables.is_empty() {
            continue;
        }
        if first_chinese {
            first_chinese = false;
            continue;
        }
        let context_start = start.saturating_sub(context_limit);
        visitor(Case {
            id: format!("{}-{:06}", document.id, index + 1),
            category: document.category.clone(),
            context: document.text[offsets[context_start]..offsets[*start]].to_owned(),
            expected: document.text[offsets[*start]..offsets[*end]].to_owned(),
            syllables: syllables.clone(),
        })?;
    }
    Ok(())
}

fn frozen_corpus() -> Result<&'static FrozenCorpus, String> {
    static CORPUS: OnceLock<Result<FrozenCorpus, String>> = OnceLock::new();
    CORPUS
        .get_or_init(|| {
            let corpus: FrozenCorpus = serde_json::from_str(CORPUS_JSON)
                .map_err(|error| format!("invalid bundled benchmark corpus: {error}"))?;
            validate_corpus(&corpus)?;
            Ok(corpus)
        })
        .as_ref()
        .map_err(Clone::clone)
}

fn validate_corpus(corpus: &FrozenCorpus) -> Result<(), String> {
    if corpus.id.is_empty() || corpus.name.is_empty() || corpus.documents.is_empty() {
        return Err("bundled corpus identity and documents must not be empty".to_owned());
    }
    if corpus.generator["offset_unit"] != "unicode_scalar" {
        return Err("unsupported bundled corpus offset unit".to_owned());
    }
    let categories: HashSet<_> = corpus
        .corpora
        .iter()
        .map(|value| value.id.as_str())
        .collect();
    if categories != HashSet::from(["zhihu", "classics"]) || corpus.corpora.len() != 2 {
        return Err("bundled corpus must contain exactly zhihu and classics".to_owned());
    }
    let mut ids = HashSet::new();
    for document in &corpus.documents {
        if !ids.insert(document.id.as_str()) || document.id.is_empty() {
            return Err(format!(
                "duplicate/empty corpus document id: {}",
                document.id
            ));
        }
        if !categories.contains(document.category.as_str()) || document.source_id.is_empty() {
            return Err(format!(
                "invalid corpus document source/category: {}",
                document.id
            ));
        }
        let characters: Vec<_> = document.text.chars().collect();
        if document.source_end.checked_sub(document.source_start) != Some(characters.len()) {
            return Err(format!("source range mismatch: {}", document.id));
        }
        let mut previous_end = 0;
        let mut chinese_words = 0;
        for (start, end, syllables) in &document.words {
            if *start != previous_end || end <= start || *end > characters.len() {
                return Err(format!("invalid/gapped token boundaries: {}", document.id));
            }
            let text: String = characters[*start..*end].iter().collect();
            let chinese = text.chars().all(is_chinese_character);
            if !chinese && (text.chars().any(is_chinese_character) || !syllables.is_empty()) {
                return Err(format!(
                    "mixed/non-Chinese prediction token: {}",
                    document.id
                ));
            }
            if chinese {
                validate_case(&Case {
                    id: document.id.clone(),
                    category: document.category.clone(),
                    // Validate the syllables even for the excluded first word.
                    context: "前".to_owned(),
                    expected: text,
                    syllables: syllables.clone(),
                })?;
                chinese_words += 1;
            }
            previous_end = *end;
        }
        if previous_end != characters.len() || chinese_words < 2 {
            return Err(format!("incomplete/empty corpus document: {}", document.id));
        }
    }
    for info in &corpus.corpora {
        let documents = || {
            corpus
                .documents
                .iter()
                .filter(|doc| doc.category == info.id)
        };
        let characters: usize = documents()
            .map(|document| {
                document
                    .text
                    .chars()
                    .filter(|ch| is_chinese_character(*ch))
                    .count()
            })
            .sum();
        let cases: usize = documents()
            .map(|document| {
                document
                    .words
                    .iter()
                    .filter(|word| !word.2.is_empty())
                    .count()
                    - 1
            })
            .sum();
        if (info.characters, info.cases) != (characters, cases) {
            return Err(format!("corpus directory counts disagree: {}", info.id));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mixed_document() -> Document {
        Document {
            id: "mixed".to_owned(),
            category: "zhihu".to_owned(),
            source_id: "mixed".to_owned(),
            source_start: 0,
            source_end: 15,
            text: "Hi!春天，A股2026年好。".to_owned(),
            words: vec![
                (0, 3, vec![]),
                (3, 5, vec!["chun".to_owned(), "tian".to_owned()]),
                (5, 7, vec![]),
                (7, 8, vec!["gu".to_owned()]),
                (8, 12, vec![]),
                (12, 13, vec!["nian".to_owned()]),
                (13, 14, vec!["hao".to_owned()]),
                (14, 15, vec![]),
            ],
        }
    }

    #[test]
    fn visits_every_chinese_word_except_first_and_preserves_non_chinese_prefix() {
        let mut cases = Vec::new();
        visit_document(&mixed_document(), 4096, &mut |case| {
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
        assert_eq!(cases[0].context, "Hi!春天，A");
        assert_eq!(cases[1].context, "Hi!春天，A股2026");
        assert_eq!(cases[2].context, "Hi!春天，A股2026年");
    }

    #[test]
    fn context_limit_counts_unicode_characters_and_propagates_visitor_error() {
        let mut cases = Vec::new();
        visit_document(&mixed_document(), 3, &mut |case| {
            cases.push(case);
            Ok(())
        })
        .unwrap();
        assert_eq!(cases[0].context, "天，A");
        assert_eq!(cases[1].context, "026");
        assert_eq!(cases[2].context, "26年");
        let error = visit_document(&mixed_document(), 10, &mut |_| Err("cancelled".to_owned()));
        assert_eq!(error, Err("cancelled".to_owned()));
        assert!(visit_builtin_cases(0, |_| Ok(())).is_err());
        assert!(visit_builtin_cases(4097, |_| Ok(())).is_err());
    }

    #[test]
    fn real_corpus_covers_all_targets_with_exact_preceding_text() {
        let corpus = frozen_corpus().unwrap();
        let mut total = 0;
        for document in &corpus.documents {
            let chars: Vec<_> = document.text.chars().collect();
            let targets: Vec<_> = document
                .words
                .iter()
                .filter(|word| !word.2.is_empty())
                .skip(1)
                .collect();
            let mut observed = 0;
            visit_document(document, 128, &mut |case| {
                let (start, end, syllables) = targets[observed];
                assert_eq!(
                    case.context,
                    chars[start.saturating_sub(128)..*start]
                        .iter()
                        .collect::<String>()
                );
                assert_eq!(
                    case.expected,
                    chars[*start..*end].iter().collect::<String>()
                );
                assert_eq!(&case.syllables, syllables);
                validate_case(&case).unwrap();
                observed += 1;
                Ok(())
            })
            .unwrap();
            assert_eq!(observed, targets.len());
            total += observed;
        }
        assert_eq!(
            total,
            corpus
                .corpora
                .iter()
                .map(|corpus| corpus.cases)
                .sum::<usize>()
        );
    }

    #[test]
    fn one_two_and_three_character_contexts_keep_every_case_including_whitespace() {
        let total: usize = builtin_dataset_info()
            .unwrap()
            .corpora
            .iter()
            .map(|corpus| corpus.cases)
            .sum();
        for (limit, expected_whitespace_cases) in [(1, 2318), (2, 672), (3, 24)] {
            let mut visited = 0;
            let mut whitespace_cases = 0;
            visit_builtin_cases(limit, |case| {
                validate_case(&case)?;
                assert!(case.context.chars().count() <= limit);
                whitespace_cases += usize::from(case.context.trim().is_empty());
                visited += 1;
                Ok(())
            })
            .unwrap();
            assert_eq!(visited, total);
            assert_eq!(whitespace_cases, expected_whitespace_cases);
        }
    }

    #[test]
    fn sources_are_pinned_and_classics_have_multiple_authors_and_comparable_volume() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../data/sources/manifest.json")).unwrap();
        let corpus = frozen_corpus().unwrap();
        let manifest_digest = format!(
            "{:x}",
            Sha256::digest(include_bytes!("../data/sources/manifest.json"))
        );
        assert_eq!(corpus.generator["source_manifest_sha256"], manifest_digest);
        let sources = manifest["sources"].as_array().unwrap();
        let authors: HashSet<_> = sources
            .iter()
            .filter(|source| source["category"] == "classics")
            .map(|source| source["author"].as_str().unwrap())
            .collect();
        assert_eq!(authors, HashSet::from(["鲁迅", "朱自清", "郁达夫"]));
        for source in sources {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("data/sources")
                .join(source["file"].as_str().unwrap());
            let raw = std::fs::read(path).unwrap();
            assert_eq!(source["sha256"], format!("{:x}", Sha256::digest(&raw)));
            assert!(source["url"].as_str().unwrap().starts_with("https://"));
            let text: Vec<_> = std::str::from_utf8(&raw).unwrap().chars().collect();
            for document in corpus
                .documents
                .iter()
                .filter(|doc| doc.source_id == source["id"].as_str().unwrap())
            {
                assert_eq!(
                    document.text,
                    text[document.source_start..document.source_end]
                        .iter()
                        .collect::<String>()
                );
            }
            if source["category"] == "classics" {
                assert!(source["published"].as_u64().unwrap() < 1930);
                assert!(source["revision"].as_u64().unwrap() > 0);
            } else {
                assert_eq!(source["commit"].as_str().unwrap().len(), 40);
            }
        }
        let info = builtin_dataset_info().unwrap();
        assert_eq!(info.corpora[0].characters, 61_921);
        assert!(info.corpora[1].characters >= info.corpora[0].characters);
        assert_eq!(info.corpora[0].cases, 35_767);
        assert_eq!(info.corpora[1].cases, 49_798);
        assert_eq!(builtin_dataset_sha256().unwrap().len(), 64);
    }
}
