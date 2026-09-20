//! Offline benchmark primitives for comparing Lime input configurations.
//!
//! The crate deliberately contains no input method or model dependency. A caller supplies
//! one [`Prediction`] for each case and input mode, and this crate validates, scores, and
//! serializes the resulting report.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

const MAX_CASES: usize = 5_000;
const MAX_DATASET_BYTES: usize = 8 * 1024 * 1024;
const MAX_CONTEXT_CHARS: usize = 4_096;
const MAX_TARGET_CHARS: usize = 32;

/// A versioned collection of benchmark cases.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Dataset {
    pub id: String,
    pub name: String,
    pub version: u32,
    pub cases: Vec<Case>,
}

/// One expected Chinese continuation and its hand-reviewed pinyin syllables.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub category: String,
    pub context: String,
    pub expected: String,
    pub syllables: Vec<String>,
}

/// The two preedit forms measured by the benchmark.
#[derive(Clone, Copy, Debug, Deserialize, Hash, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputMode {
    Full,
    Initials,
}

/// The top-ranked commit returned by an adapter.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Prediction {
    pub top1: Option<String>,
    pub error: Option<String>,
    pub elapsed_ms: Option<u64>,
}

/// A normalized observation retained in a report.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Observation {
    pub case_id: String,
    pub category: String,
    pub context: String,
    pub expected: String,
    pub preedit: String,
    pub mode: InputMode,
    pub top1: Option<String>,
    pub correct: bool,
    pub error: Option<String>,
    pub elapsed_ms: Option<u64>,
}

/// Counts for one category and input mode.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Summary {
    pub category: Option<String>,
    pub mode: InputMode,
    pub total: usize,
    pub completed: usize,
    pub correct: usize,
    pub no_prediction: usize,
    pub errors: usize,
    pub accuracy: Option<f64>,
}

/// A reproducible benchmark result, including the dataset fingerprint.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Report {
    pub dataset_id: String,
    pub dataset_name: String,
    pub dataset_version: u32,
    pub dataset_sha256: String,
    pub modes: Vec<InputMode>,
    pub summaries: Vec<Summary>,
    pub observations: Vec<Observation>,
    pub complete: bool,
}

/// Load the bundled 48-case daily-language dataset.
pub fn builtin_dataset() -> Result<Dataset, String> {
    parse_dataset(include_str!("../data/builtin.json"))
}

/// Parse and validate a JSON dataset. Invalid cases are returned as errors; none are dropped.
pub fn parse_dataset(json: &str) -> Result<Dataset, String> {
    let dataset: Dataset = serde_json::from_str(json)
        .map_err(|error| format!("invalid benchmark dataset JSON: {error}"))?;
    validate_dataset(&dataset)?;
    Ok(dataset)
}

/// Validate dataset identity, case uniqueness, pinyin, and size limits.
pub fn validate_dataset(dataset: &Dataset) -> Result<(), String> {
    if dataset.id.trim().is_empty() {
        return Err("dataset id must not be empty".to_owned());
    }
    if dataset.name.trim().is_empty() {
        return Err("dataset name must not be empty".to_owned());
    }
    if dataset.cases.is_empty() {
        return Err("dataset must contain at least one case".to_owned());
    }
    if dataset.cases.len() > MAX_CASES {
        return Err(format!(
            "dataset contains {} cases; the maximum is {MAX_CASES}",
            dataset.cases.len()
        ));
    }

    let mut ids = HashSet::with_capacity(dataset.cases.len());
    for case in &dataset.cases {
        validate_case(case)?;
        if !ids.insert(case.id.as_str()) {
            return Err(format!("duplicate case id: {}", case.id));
        }
    }

    let bytes = serde_json::to_vec(dataset)
        .map_err(|error| format!("failed to serialize dataset for size validation: {error}"))?;
    if bytes.len() > MAX_DATASET_BYTES {
        return Err(format!(
            "serialized dataset is {} bytes; the maximum is {MAX_DATASET_BYTES}",
            bytes.len()
        ));
    }
    Ok(())
}

/// Validate one case, including the target-to-syllable alignment contract.
pub fn validate_case(case: &Case) -> Result<(), String> {
    if case.id.trim().is_empty() {
        return Err("case id must not be empty".to_owned());
    }
    if case.category.trim().is_empty() {
        return Err(format!("case {} category must not be empty", case.id));
    }
    let context_chars = case.context.chars().count();
    if context_chars == 0 || case.context.trim().is_empty() {
        return Err(format!("case {} context must not be empty", case.id));
    }
    if context_chars > MAX_CONTEXT_CHARS {
        return Err(format!(
            "case {} context has {context_chars} characters; the maximum is {MAX_CONTEXT_CHARS}",
            case.id
        ));
    }

    let target_chars = case.expected.chars().count();
    if target_chars == 0 {
        return Err(format!("case {} expected text must not be empty", case.id));
    }
    if target_chars > MAX_TARGET_CHARS {
        return Err(format!(
            "case {} expected text has {target_chars} characters; the maximum is {MAX_TARGET_CHARS}",
            case.id
        ));
    }
    if !case.expected.chars().all(is_chinese_character) {
        return Err(format!(
            "case {} expected text must contain only Chinese characters",
            case.id
        ));
    }
    if case.syllables.len() != target_chars {
        return Err(format!(
            "case {} has {} syllables for {target_chars} target characters",
            case.id,
            case.syllables.len()
        ));
    }
    for (index, syllable) in case.syllables.iter().enumerate() {
        if syllable.is_empty() || !syllable.chars().all(|ch| ch.is_ascii_lowercase()) {
            return Err(format!(
                "case {} syllable {} must use lowercase a-z (v represents ü)",
                case.id,
                index + 1
            ));
        }
    }
    Ok(())
}

/// Convert a case to a full-pinyin or initial-letter preedit.
///
/// Full pinyin concatenates syllables. An apostrophe is inserted before a later
/// syllable beginning with `a`, `e`, or `o`, which keeps vowel-initial boundaries
/// unambiguous (for example, `fang'an` and `wen'an`).
pub fn preedit(case: &Case, mode: InputMode) -> String {
    match mode {
        InputMode::Full => {
            let mut result = String::new();
            for (index, syllable) in case.syllables.iter().enumerate() {
                if index > 0
                    && syllable
                        .chars()
                        .next()
                        .is_some_and(|first| matches!(first, 'a' | 'e' | 'o'))
                {
                    result.push('\'');
                }
                result.push_str(syllable);
            }
            result
        }
        InputMode::Initials => case
            .syllables
            .iter()
            .filter_map(|syllable| syllable.chars().next())
            .collect(),
    }
}

/// Turn one adapter result into a report observation.
///
/// Accuracy uses strict literal equality with `expected`; an adapter error is always
/// a miss even if it happens to include a matching `top1` value.
pub fn observe(case: &Case, mode: InputMode, prediction: Prediction) -> Observation {
    let correct =
        prediction.error.is_none() && prediction.top1.as_deref() == Some(case.expected.as_str());
    Observation {
        case_id: case.id.clone(),
        category: case.category.clone(),
        context: case.context.clone(),
        expected: case.expected.clone(),
        preedit: preedit(case, mode),
        mode,
        top1: prediction.top1,
        correct,
        error: prediction.error,
        elapsed_ms: prediction.elapsed_ms,
    }
}

/// Validate observations, recompute strict correctness, and aggregate a report.
pub fn build_report(
    dataset: &Dataset,
    modes: &[InputMode],
    mut observations: Vec<Observation>,
) -> Result<Report, String> {
    validate_dataset(dataset)?;
    if modes.is_empty() {
        return Err("at least one input mode is required".to_owned());
    }

    let mut mode_set = HashSet::with_capacity(modes.len());
    for mode in modes {
        if !mode_set.insert(*mode) {
            return Err(format!("duplicate input mode: {mode:?}"));
        }
    }

    let cases_by_id: HashMap<&str, &Case> = dataset
        .cases
        .iter()
        .map(|case| (case.id.as_str(), case))
        .collect();
    let mut seen = HashSet::with_capacity(observations.len());
    for observation in &mut observations {
        let case = cases_by_id
            .get(observation.case_id.as_str())
            .copied()
            .ok_or_else(|| {
                format!(
                    "observation references unknown case: {}",
                    observation.case_id
                )
            })?;
        if !mode_set.contains(&observation.mode) {
            return Err(format!(
                "observation {} uses a mode that is not in the report",
                observation.case_id
            ));
        }
        let key = (observation.case_id.clone(), observation.mode);
        if !seen.insert(key) {
            return Err(format!(
                "duplicate observation for case {} and mode {:?}",
                observation.case_id, observation.mode
            ));
        }
        if observation.category != case.category {
            return Err(format!(
                "observation {} category does not match dataset",
                observation.case_id
            ));
        }
        if observation.context != case.context {
            return Err(format!(
                "observation {} context does not match dataset",
                observation.case_id
            ));
        }
        if observation.expected != case.expected {
            return Err(format!(
                "observation {} expected text does not match dataset",
                observation.case_id
            ));
        }
        let expected_preedit = preedit(case, observation.mode);
        if observation.preedit != expected_preedit {
            return Err(format!(
                "observation {} preedit does not match dataset",
                observation.case_id
            ));
        }
        observation.correct = observation.error.is_none()
            && observation.top1.as_deref() == Some(case.expected.as_str());
    }

    let complete = seen.len() == dataset.cases.len() * modes.len();
    let categories = dataset
        .cases
        .iter()
        .map(|case| case.category.as_str())
        .fold(Vec::<String>::new(), |mut categories, category| {
            if categories.iter().all(|known| known != category) {
                categories.push(category.to_owned());
            }
            categories
        });

    let mut summaries = Vec::with_capacity((categories.len() + 1) * modes.len());
    for mode in modes {
        for category in &categories {
            summaries.push(make_summary(
                Some(category.clone()),
                *mode,
                dataset
                    .cases
                    .iter()
                    .filter(|case| case.category == *category)
                    .count(),
                observations.iter().filter(|observation| {
                    observation.mode == *mode && observation.category == *category
                }),
                complete,
            ));
        }
        summaries.push(make_summary(
            None,
            *mode,
            dataset.cases.len(),
            observations
                .iter()
                .filter(|observation| observation.mode == *mode),
            complete,
        ));
    }

    Ok(Report {
        dataset_id: dataset.id.clone(),
        dataset_name: dataset.name.clone(),
        dataset_version: dataset.version,
        dataset_sha256: fingerprint(dataset)?,
        modes: modes.to_vec(),
        summaries,
        observations,
        complete,
    })
}

fn make_summary<'a, I>(
    category: Option<String>,
    mode: InputMode,
    total: usize,
    observations: I,
    complete: bool,
) -> Summary
where
    I: IntoIterator<Item = &'a Observation>,
{
    let mut completed = 0;
    let mut correct = 0;
    let mut no_prediction = 0;
    let mut errors = 0;
    for observation in observations {
        completed += 1;
        if observation.correct {
            correct += 1;
        }
        if observation.error.is_some() {
            errors += 1;
        } else if observation.top1.is_none() {
            no_prediction += 1;
        }
    }
    let accuracy = complete.then(|| correct as f64 / total as f64);
    Summary {
        category,
        mode,
        total,
        completed,
        correct,
        no_prediction,
        errors,
        accuracy,
    }
}

fn fingerprint(dataset: &Dataset) -> Result<String, String> {
    let bytes = serde_json::to_vec(dataset)
        .map_err(|error| format!("failed to serialize dataset for fingerprint: {error}"))?;
    let digest = Sha256::digest(bytes);
    Ok(format!("{digest:x}"))
}

fn is_chinese_character(character: char) -> bool {
    matches!(
        character as u32,
        0x3007
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0x20000..=0x323AF
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(id: &str, category: &str, context: &str, expected: &str, syllables: &[&str]) -> Case {
        Case {
            id: id.to_owned(),
            category: category.to_owned(),
            context: context.to_owned(),
            expected: expected.to_owned(),
            syllables: syllables.iter().map(|value| (*value).to_owned()).collect(),
        }
    }

    fn tiny_dataset() -> Dataset {
        Dataset {
            id: "tiny".to_owned(),
            name: "Tiny".to_owned(),
            version: 1,
            cases: vec![
                case("a", "chat", "我想吃", "方案", &["fang", "an"]),
                case("b", "search", "北京适合", "旅游", &["lv", "you"]),
            ],
        }
    }

    #[test]
    fn bundled_dataset_is_valid_and_has_four_categories() {
        let dataset = builtin_dataset().expect("bundled dataset should validate");
        assert_eq!(dataset.cases.len(), 48);
        assert_eq!(preedit(&dataset.cases[29], InputMode::Full), "fang'an");
        assert_eq!(preedit(&dataset.cases[30], InputMode::Initials), "wt");
        let categories: HashSet<_> = dataset
            .cases
            .iter()
            .map(|case| case.category.as_str())
            .collect();
        assert_eq!(
            categories,
            HashSet::from(["chat", "search", "prompt", "article"])
        );
    }

    #[test]
    fn validation_rejects_bad_context_target_alignment_and_syllables() {
        let mut value = tiny_dataset();
        value.cases[0].context = "   ".to_owned();
        assert!(validate_dataset(&value).is_err());
        value = tiny_dataset();
        value.cases[0].context = "我想吃".to_owned();
        value.cases[0].expected = "abc".to_owned();
        assert!(validate_dataset(&value).is_err());
        value = tiny_dataset();
        value.cases[0].syllables = vec!["fang".to_owned()];
        assert!(validate_dataset(&value).is_err());
        value = tiny_dataset();
        value.cases[1].syllables[0] = "LV".to_owned();
        assert!(validate_dataset(&value).is_err());
        value.cases[1].id = value.cases[0].id.clone();
        assert!(validate_dataset(&value).is_err());
    }

    #[test]
    fn parsing_does_not_silently_drop_unknown_dataset_fields() {
        let json = r#"{
            "id":"tiny",
            "name":"Tiny",
            "version":1,
            "cases":[],
            "ignored":"this must fail"
        }"#;
        assert!(parse_dataset(json).is_err());
    }

    #[test]
    fn observe_uses_strict_literal_top1_and_errors_are_misses() {
        let test_case = case("a", "chat", "我想吃", "方案", &["fang", "an"]);
        let exact = observe(
            &test_case,
            InputMode::Full,
            Prediction {
                top1: Some("方案".to_owned()),
                error: None,
                elapsed_ms: Some(3),
            },
        );
        assert!(exact.correct);
        assert_eq!(exact.preedit, "fang'an");
        let extra_space = observe(
            &test_case,
            InputMode::Full,
            Prediction {
                top1: Some("方案 ".to_owned()),
                error: None,
                elapsed_ms: None,
            },
        );
        assert!(!extra_space.correct);
        let error = observe(
            &test_case,
            InputMode::Full,
            Prediction {
                top1: Some("方案".to_owned()),
                error: Some("timeout".to_owned()),
                elapsed_ms: None,
            },
        );
        assert!(!error.correct);
    }

    #[test]
    fn report_counts_empty_predictions_and_errors_in_denominator() {
        let dataset = tiny_dataset();
        let first = observe(
            &dataset.cases[0],
            InputMode::Full,
            Prediction {
                top1: Some("方案".to_owned()),
                error: None,
                elapsed_ms: None,
            },
        );
        let second = observe(
            &dataset.cases[1],
            InputMode::Full,
            Prediction {
                top1: None,
                error: None,
                elapsed_ms: None,
            },
        );
        let report = build_report(&dataset, &[InputMode::Full], vec![first, second]).unwrap();
        assert!(report.complete);
        let overall = report
            .summaries
            .iter()
            .find(|summary| summary.category.is_none())
            .unwrap();
        assert_eq!(
            (overall.total, overall.completed, overall.correct),
            (2, 2, 1)
        );
        assert_eq!(overall.no_prediction, 1);
        assert_eq!(overall.errors, 0);
        assert_eq!(overall.accuracy, Some(0.5));

        let report = build_report(
            &dataset,
            &[InputMode::Full],
            vec![observe(
                &dataset.cases[0],
                InputMode::Full,
                Prediction {
                    top1: None,
                    error: Some("service unavailable".to_owned()),
                    elapsed_ms: None,
                },
            )],
        )
        .unwrap();
        assert!(!report.complete);
        let overall = report
            .summaries
            .iter()
            .find(|summary| summary.category.is_none())
            .unwrap();
        assert_eq!(overall.errors, 1);
        assert_eq!(overall.accuracy, None);
    }

    #[test]
    fn report_rejects_unknown_duplicate_and_inconsistent_observations() {
        let dataset = tiny_dataset();
        let valid = observe(
            &dataset.cases[0],
            InputMode::Full,
            Prediction {
                top1: None,
                error: None,
                elapsed_ms: None,
            },
        );
        let mut unknown = valid.clone();
        unknown.case_id = "missing".to_owned();
        assert!(build_report(&dataset, &[InputMode::Full], vec![unknown]).is_err());
        assert!(build_report(
            &dataset,
            &[InputMode::Full],
            vec![valid.clone(), valid.clone()]
        )
        .is_err());
        let mut inconsistent = valid;
        inconsistent.preedit = "wrong".to_owned();
        assert!(build_report(&dataset, &[InputMode::Full], vec![inconsistent]).is_err());
    }

    #[test]
    fn fingerprint_changes_when_content_changes_even_if_identity_matches() {
        let first = tiny_dataset();
        let mut second = first.clone();
        second.cases[0].context = "我今天想吃".to_owned();
        let first_report = build_report(&first, &[InputMode::Full], Vec::new()).unwrap();
        let second_report = build_report(&second, &[InputMode::Full], Vec::new()).unwrap();
        assert_ne!(first_report.dataset_sha256, second_report.dataset_sha256);
    }

    #[test]
    fn serde_uses_snake_case_input_modes() {
        assert_eq!(serde_json::to_string(&InputMode::Full).unwrap(), "\"full\"");
        assert_eq!(
            serde_json::to_string(&InputMode::Initials).unwrap(),
            "\"initials\""
        );
    }
}
