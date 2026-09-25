mod policy;
use policy::*;

use lime_protocol::{Candidate, CandidateDiagnostic, LlmPerformance};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

pub use crate::llama::{CandidateScore, LlamaRuntime, ModelMetadata, TokenInfo};

const DEFAULT_INFERENCE_COUNT_LIMIT: usize = 1;

#[derive(Clone, Copy, Debug)]
pub(crate) struct RerankOptions {
    pub(crate) rerank_count: usize,
    pub(crate) effective_count: usize,
    pub(crate) inference_count_limit: usize,
    pub(crate) ignore_emoji: bool,
}

/// Result of candidate ranking together with the rows consumed by the management UI.
#[derive(Clone, Debug, PartialEq)]
pub struct RerankResult {
    pub candidates: Vec<Candidate>,
    pub diagnostics: Vec<CandidateDiagnostic>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RankingOutcome {
    pub result: RerankResult,
    /// Original Rime index for each candidate in `result.candidates`.
    pub(crate) candidate_indices: Vec<usize>,
    pub llm_performance: Option<LlmPerformance>,
}

#[derive(Clone, Debug)]
struct ScoringOutput {
    scores: Vec<CandidateScore>,
    scored_indices: Vec<usize>,
    performance: LlmPerformance,
}

trait CandidateScorer {
    fn score_candidates(
        &self,
        preceding_text: &str,
        candidates: &[Candidate],
    ) -> Result<Vec<CandidateScore>, String>;

    fn score_candidates_with_performance(
        &self,
        preceding_text: &str,
        candidates: &[Candidate],
    ) -> Result<ScoringOutput, String> {
        let started = Instant::now();
        let scores = self.score_candidates(preceding_text, candidates)?;
        let target_token_count = scores
            .iter()
            .map(|score| score.token_logprobs.len())
            .sum::<usize>();
        Ok(ScoringOutput {
            performance: LlmPerformance {
                total_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                candidate_count: candidates.len().min(u32::MAX as usize) as u32,
                scored_count: scores.len().min(u32::MAX as usize) as u32,
                target_token_count: target_token_count.min(u32::MAX as usize) as u32,
                ..LlmPerformance::default()
            },
            scores,
            scored_indices: (0..candidates.len()).collect(),
        })
    }

    fn score_candidates_with_performance_limit(
        &self,
        preceding_text: &str,
        candidates: &[Candidate],
        _inference_count_limit: usize,
    ) -> Result<ScoringOutput, String> {
        self.score_candidates_with_performance(preceding_text, candidates)
    }
}

impl CandidateScorer for LlamaRuntime {
    fn score_candidates(
        &self,
        preceding_text: &str,
        candidates: &[Candidate],
    ) -> Result<Vec<CandidateScore>, String> {
        LlamaRuntime::score_candidates(self, preceding_text, candidates)
    }

    fn score_candidates_with_performance(
        &self,
        preceding_text: &str,
        candidates: &[Candidate],
    ) -> Result<ScoringOutput, String> {
        let result =
            LlamaRuntime::score_candidates_with_performance(self, preceding_text, candidates)?;
        Ok(ScoringOutput {
            scores: result.scores,
            scored_indices: result.scored_indices,
            performance: result.performance,
        })
    }

    fn score_candidates_with_performance_limit(
        &self,
        preceding_text: &str,
        candidates: &[Candidate],
        inference_count_limit: usize,
    ) -> Result<ScoringOutput, String> {
        let result = LlamaRuntime::score_candidates_with_inference_limit(
            self,
            preceding_text,
            candidates,
            inference_count_limit,
        )?;
        Ok(ScoringOutput {
            scores: result.scores,
            scored_indices: result.scored_indices,
            performance: result.performance,
        })
    }
}

/// Rank candidates without hiding model failures.
///
/// Existing callers that use the infallible Phase-1 API continue to compile. If a loaded model
/// cannot tokenize/decode a request, this function panics with the native error; the production
/// service uses [`try_rerank_candidates_with_diagnostics`] so it can return an IPC error instead.
pub fn rerank_candidates_with_diagnostics(
    candidates: &[Candidate],
    preceding_text: &str,
    runtime: Option<&LlamaRuntime>,
    rerank_count: usize,
    effective_count: usize,
) -> RerankResult {
    try_rerank_candidates_with_diagnostics(
        candidates,
        preceding_text,
        runtime,
        rerank_count,
        effective_count,
    )
    .unwrap_or_else(|error| panic!("llama.cpp candidate rerank failed: {error}"))
}

/// Fallible production entry point for model-backed ranking.
pub fn try_rerank_candidates_with_diagnostics(
    candidates: &[Candidate],
    preceding_text: &str,
    runtime: Option<&LlamaRuntime>,
    rerank_count: usize,
    effective_count: usize,
) -> Result<RerankResult, String> {
    let candidate_indices = (0..candidates.len()).collect::<Vec<_>>();
    try_rerank_selected_candidates_with_diagnostics(
        candidates,
        &candidate_indices,
        preceding_text,
        runtime,
        rerank_count,
        effective_count,
        DEFAULT_INFERENCE_COUNT_LIMIT,
    )
}

/// Rerank only the candidates selected by the Rime engine as consuming the complete input.
///
/// `candidate_indices` retain the original Rime positions. Invalid, repeated, or positions
/// outside the first `rerank_count` Rime candidates are ignored.
pub(crate) fn try_rerank_selected_candidates_with_diagnostics(
    candidates: &[Candidate],
    candidate_indices: &[usize],
    preceding_text: &str,
    runtime: Option<&LlamaRuntime>,
    rerank_count: usize,
    effective_count: usize,
    inference_count_limit: usize,
) -> Result<RerankResult, String> {
    try_rerank_selected_candidates_with_scorer_and_preedit_and_limit(
        candidates,
        candidate_indices,
        None,
        preceding_text,
        runtime,
        RerankOptions {
            rerank_count,
            effective_count,
            inference_count_limit,
            ignore_emoji: false,
        },
    )
    .map(|outcome| outcome.result)
}

/// Production entry point that applies text, English-candidate, and Emoji policies.
///
/// English candidates are useful only when Rime returns the exact pinyin text the user typed.
/// Excluded candidates remain available in the final Rime order, but are not sent to the model.
pub(crate) fn try_rerank_selected_candidates_with_preedit_and_limit(
    candidates: &[Candidate],
    candidate_indices: &[usize],
    preedit: &str,
    preceding_text: &str,
    runtime: Option<&LlamaRuntime>,
    options: RerankOptions,
) -> Result<RankingOutcome, String> {
    try_rerank_selected_candidates_with_scorer_and_preedit_and_limit(
        candidates,
        candidate_indices,
        Some(preedit),
        preceding_text,
        runtime,
        options,
    )
}

#[allow(dead_code)]
fn try_rerank_selected_candidates_with_scorer<S: CandidateScorer + ?Sized>(
    candidates: &[Candidate],
    candidate_indices: &[usize],
    preceding_text: &str,
    runtime: Option<&S>,
    rerank_count: usize,
    effective_count: usize,
) -> Result<RerankResult, String> {
    try_rerank_selected_candidates_with_scorer_and_preedit_and_limit(
        candidates,
        candidate_indices,
        None,
        preceding_text,
        runtime,
        RerankOptions {
            rerank_count,
            effective_count,
            inference_count_limit: DEFAULT_INFERENCE_COUNT_LIMIT,
            ignore_emoji: false,
        },
    )
    .map(|outcome| outcome.result)
}

#[allow(dead_code)]
fn try_rerank_selected_candidates_with_scorer_and_preedit<S: CandidateScorer + ?Sized>(
    candidates: &[Candidate],
    candidate_indices: &[usize],
    preedit: Option<&str>,
    preceding_text: &str,
    runtime: Option<&S>,
    rerank_count: usize,
    effective_count: usize,
) -> Result<RankingOutcome, String> {
    try_rerank_selected_candidates_with_scorer_and_preedit_and_limit(
        candidates,
        candidate_indices,
        preedit,
        preceding_text,
        runtime,
        RerankOptions {
            rerank_count,
            effective_count,
            inference_count_limit: DEFAULT_INFERENCE_COUNT_LIMIT,
            ignore_emoji: false,
        },
    )
}

fn try_rerank_selected_candidates_with_scorer_and_preedit_and_limit<S: CandidateScorer + ?Sized>(
    candidates: &[Candidate],
    candidate_indices: &[usize],
    preedit: Option<&str>,
    preceding_text: &str,
    runtime: Option<&S>,
    options: RerankOptions,
) -> Result<RankingOutcome, String> {
    let pool_indices =
        selected_pool_indices(candidate_indices, candidates.len(), options.rerank_count)
            .into_iter()
            .filter(|index| {
                candidate_allowed_for_llm(&candidates[*index], preedit, options.ignore_emoji)
            })
            .collect::<Vec<_>>();
    let pool_candidates = pool_indices
        .iter()
        .map(|index| candidates[*index].clone())
        .collect::<Vec<_>>();

    let active_runtime = if preceding_text.is_empty() {
        None
    } else {
        runtime
    };
    let model_active = active_runtime.is_some() && !pool_candidates.is_empty();
    let (score_rows, llm_performance) = if model_active {
        let runtime = active_runtime.expect("active runtime must exist when scoring");
        let scored = runtime.score_candidates_with_performance_limit(
            preceding_text,
            &pool_candidates,
            options.inference_count_limit,
        )?;
        if scored.scores.len() != scored.scored_indices.len()
            || scored
                .scored_indices
                .iter()
                .any(|index| *index >= pool_candidates.len())
        {
            return Err(format!(
                "llama.cpp returned an invalid score mapping: {} scores for {} candidates",
                scored.scores.len(),
                pool_candidates.len(),
            ));
        }

        let mut ranked = scored
            .scored_indices
            .into_iter()
            .zip(scored.scores)
            .map(|(pool_index, score)| {
                (
                    pool_indices[pool_index],
                    pool_candidates[pool_index].clone(),
                    score,
                )
            })
            .collect::<Vec<_>>();
        // Larger log probabilities are better. Keep original Rime order as a stable tie breaker.
        ranked.sort_by(|(left_index, _, left), (right_index, _, right)| {
            right
                .logprob
                .total_cmp(&left.logprob)
                .then_with(|| left_index.cmp(right_index))
        });

        (ranked, Some(scored.performance))
    } else {
        (Vec::new(), None)
    };

    let (result, candidate_indices) = build_rerank_result_with_indices(
        candidates,
        model_active,
        score_rows,
        options.effective_count,
    );
    Ok(RankingOutcome {
        result,
        candidate_indices,
        llm_performance,
    })
}

pub fn rerank_candidates(
    candidates: &[Candidate],
    preceding_text: &str,
    runtime: Option<&LlamaRuntime>,
    rerank_count: usize,
    effective_count: usize,
) -> Vec<Candidate> {
    rerank_candidates_with_diagnostics(
        candidates,
        preceding_text,
        runtime,
        rerank_count,
        effective_count,
    )
    .candidates
}

#[derive(Default, Debug)]
pub struct GenerationTracker(AtomicU64);

impl GenerationTracker {
    pub fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::AcqRel).saturating_add(1)
    }

    pub fn current(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }

    pub fn is_current(&self, generation: u64) -> bool {
        self.current() == generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(text: &str) -> Candidate {
        Candidate {
            display_text: text.into(),
            commit_text: text.into(),
        }
    }

    #[test]
    fn no_model_preserves_rime_order() {
        let input = vec![c("a"), c("b")];
        assert_eq!(rerank_candidates(&input, "", None, 2, 1), input);
    }

    #[test]
    fn generation_invalidates_old_requests() {
        let tracker = GenerationTracker::default();
        let first = tracker.next();
        let _ = tracker.next();
        assert!(!tracker.is_current(first));
    }

    #[test]
    fn diagnostics_preserve_rime_and_final_order_without_model() {
        let input = vec![c("甲"), c("乙")];
        let result = rerank_candidates_with_diagnostics(&input, "前文", None, 2, 1);
        assert_eq!(result.candidates, input);
        assert_eq!(result.diagnostics.len(), 2);
        assert_eq!(
            result.diagnostics[0]
                .rime_candidate
                .as_ref()
                .unwrap()
                .commit_text,
            "甲"
        );
        assert!(result.diagnostics[0].llm_candidate.is_none());
        assert_eq!(
            result.diagnostics[0]
                .display_candidate
                .as_ref()
                .unwrap()
                .commit_text,
            "甲"
        );
        assert!(result.diagnostics[0].logprobs.is_empty());
    }

    #[test]
    fn fallible_api_keeps_no_model_path_explicit() {
        let input = vec![c("a")];
        let result = try_rerank_candidates_with_diagnostics(&input, "", None, 1, 1).unwrap();
        assert_eq!(result.candidates, input);
    }

    #[test]
    fn empty_preceding_text_skips_available_runtime_and_preserves_rime_order() {
        struct PanickingScorer;

        impl CandidateScorer for PanickingScorer {
            fn score_candidates(
                &self,
                _: &str,
                _: &[Candidate],
            ) -> Result<Vec<CandidateScore>, String> {
                panic!("empty preceding text must not invoke the LLM scorer");
            }
        }

        let input = vec![c("甲"), c("乙"), c("丙")];
        let scorer = PanickingScorer;
        let result =
            try_rerank_selected_candidates_with_scorer(&input, &[0, 1, 2], "", Some(&scorer), 3, 2)
                .unwrap();

        assert_eq!(result.candidates, input);
        assert!(result
            .diagnostics
            .iter()
            .all(|row| row.llm_candidate.is_none() && row.logprobs.is_empty()));
    }

    #[test]
    fn english_candidates_are_scored_only_when_they_equal_the_preedit_ignoring_ascii_case() {
        use std::cell::RefCell;

        struct RecordingScorer {
            seen: RefCell<Vec<String>>,
        }

        impl CandidateScorer for RecordingScorer {
            fn score_candidates(
                &self,
                _: &str,
                candidates: &[Candidate],
            ) -> Result<Vec<CandidateScore>, String> {
                self.seen.borrow_mut().extend(
                    candidates
                        .iter()
                        .map(|candidate| candidate.commit_text.clone()),
                );
                Ok(candidates
                    .iter()
                    .enumerate()
                    .map(|(index, _)| CandidateScore {
                        token_ids: Vec::new(),
                        token_logprobs: vec![-(index as f64)],
                        logprob: -(index as f64),
                        mismatch: false,
                    })
                    .collect())
            }
        }

        let candidates = vec![
            c("你好"),
            c("mac"),
            c("macos"),
            c("macOS"),
            c("MACOS"),
            c("3D打印"),
            c("mac-os"),
            c("macos2"),
        ];
        let scorer = RecordingScorer {
            seen: RefCell::new(Vec::new()),
        };
        let result = try_rerank_selected_candidates_with_scorer_and_preedit(
            &candidates,
            &[0, 1, 2, 3, 4, 5, 6, 7],
            Some("macos"),
            "前文",
            Some(&scorer),
            8,
            2,
        )
        .unwrap();

        assert_eq!(
            scorer.seen.into_inner(),
            vec!["你好", "macos", "macOS", "MACOS", "3D打印"]
        );
        assert_eq!(
            result
                .result
                .diagnostics
                .iter()
                .filter_map(|row| row.llm_candidate.as_ref())
                .map(|candidate| candidate.commit_text.as_str())
                .collect::<Vec<_>>(),
            vec!["你好", "macos", "macOS", "MACOS", "3D打印"]
        );
        assert!(result
            .result
            .candidates
            .iter()
            .any(|candidate| candidate.commit_text == "mac"));
        assert!(result
            .result
            .candidates
            .iter()
            .any(|candidate| candidate.commit_text == "mac-os"));
        let performance = result
            .llm_performance
            .as_ref()
            .expect("scored candidates should have performance");
        assert_eq!(performance.candidate_count, 5);
        assert_eq!(performance.scored_count, 5);
        assert_eq!(performance.target_token_count, 5);
    }

    #[test]
    fn text_filter_preserves_chinese_mixed_words_and_uses_commit_text() {
        for text in [
            "要", "的", "〇", "㐀", "𠮷", "﨑", "3D打印", "第1名", "δ函数",
        ] {
            for preedit in [Some("yao"), None] {
                assert!(
                    candidate_allowed_for_llm(&c(text), preedit, true),
                    "Chinese candidate {text:?} must remain eligible"
                );
            }
        }

        assert!(!candidate_allowed_for_llm(
            &Candidate {
                display_text: "要".into(),
                commit_text: "1".into(),
            },
            Some("yao"),
            true,
        ));
        assert!(candidate_allowed_for_llm(
            &Candidate {
                display_text: "1".into(),
                commit_text: "要".into(),
            },
            Some("yao"),
            true,
        ));
    }

    #[test]
    fn numeric_and_non_english_symbol_candidates_are_not_scored_or_promoted() {
        struct AssertingScorer {
            expected: Vec<Candidate>,
        }

        impl CandidateScorer for AssertingScorer {
            fn score_candidates(
                &self,
                _: &str,
                candidates: &[Candidate],
            ) -> Result<Vec<CandidateScore>, String> {
                assert_eq!(candidates, self.expected);
                Ok(candidates
                    .iter()
                    .enumerate()
                    .map(|(index, _)| {
                        let logprob = if index == 1 { -1.0 } else { -2.0 };
                        CandidateScore {
                            token_ids: Vec::new(),
                            token_logprobs: vec![logprob],
                            logprob,
                            mismatch: false,
                        }
                    })
                    .collect())
            }
        }

        for (preedit, first, second, excluded) in [
            ("yao", "要", "幺", ["1", "１２", "①", "3.14", "-42"]),
            ("de", "的", "得", ["δ", "Δ", "Ж", "∑", "。"]),
        ] {
            let mut candidates = vec![c(first), c(excluded[0]), c(second), c(preedit)];
            candidates.extend(excluded[1..].iter().map(|text| c(text)));
            candidates.push(c("范围外"));
            let selected = (0..candidates.len()).collect::<Vec<_>>();
            let scorer = AssertingScorer {
                expected: vec![c(first), c(second), c(preedit)],
            };

            for input in [Some(preedit), None] {
                let outcome = try_rerank_selected_candidates_with_scorer_and_preedit(
                    &candidates,
                    &selected,
                    input,
                    "前文",
                    Some(&scorer),
                    candidates.len() - 1,
                    2,
                )
                .unwrap();

                let expected_indices = vec![2, 0, 1, 3, 4, 5, 6, 7, 8];
                assert_eq!(outcome.candidate_indices, expected_indices);
                assert_eq!(
                    outcome.result.candidates,
                    expected_indices
                        .iter()
                        .map(|index| candidates[*index].clone())
                        .collect::<Vec<_>>()
                );
                assert_eq!(
                    outcome
                        .result
                        .diagnostics
                        .iter()
                        .filter_map(|row| row.llm_candidate.clone())
                        .collect::<Vec<_>>(),
                    vec![c(second), c(first), c(preedit)]
                );
                assert_eq!(
                    outcome
                        .result
                        .diagnostics
                        .iter()
                        .filter_map(|row| row.rime_candidate.clone())
                        .collect::<Vec<_>>(),
                    candidates
                );
                let performance = outcome.llm_performance.unwrap();
                assert_eq!(performance.candidate_count, 3);
                assert_eq!(performance.scored_count, 3);
            }
        }
    }

    #[test]
    fn all_numeric_or_symbol_candidates_skip_the_scorer_and_keep_rime_order() {
        struct PanickingScorer;

        impl CandidateScorer for PanickingScorer {
            fn score_candidates(
                &self,
                _: &str,
                _: &[Candidate],
            ) -> Result<Vec<CandidateScore>, String> {
                panic!("an empty text-filtered pool must not invoke the scorer");
            }
        }

        let candidates = vec![c("1"), c("１２"), c("δ"), c("∑"), c(""), c(" "), c("1")];
        let indices = (0..candidates.len()).collect::<Vec<_>>();
        for preedit in [Some("yao"), Some("de"), Some("1"), Some("δ"), None] {
            let outcome = try_rerank_selected_candidates_with_scorer_and_preedit(
                &candidates,
                &indices,
                preedit,
                "前文",
                Some(&PanickingScorer),
                candidates.len(),
                3,
            )
            .unwrap();

            assert_eq!(outcome.result.candidates, candidates);
            assert_eq!(outcome.candidate_indices, indices);
            assert!(outcome.llm_performance.is_none());
            assert!(outcome
                .result
                .diagnostics
                .iter()
                .all(|row| row.llm_candidate.is_none() && row.logprobs.is_empty()));
        }
    }

    #[test]
    fn emoji_candidates_can_be_excluded_without_changing_rime_order() {
        use std::cell::RefCell;

        struct RecordingScorer {
            seen: RefCell<Vec<String>>,
        }

        impl CandidateScorer for RecordingScorer {
            fn score_candidates(
                &self,
                _: &str,
                candidates: &[Candidate],
            ) -> Result<Vec<CandidateScore>, String> {
                self.seen.borrow_mut().extend(
                    candidates
                        .iter()
                        .map(|candidate| candidate.commit_text.clone()),
                );
                Ok(candidates
                    .iter()
                    .enumerate()
                    .map(|(index, _)| CandidateScore {
                        token_ids: Vec::new(),
                        token_logprobs: vec![-(index as f64)],
                        logprob: -(index as f64),
                        mismatch: false,
                    })
                    .collect())
            }
        }

        let candidates = vec![c("你好"), c("😀"), c("中文😀"), c("hello")];
        let scorer = RecordingScorer {
            seen: RefCell::new(Vec::new()),
        };
        let result = try_rerank_selected_candidates_with_scorer_and_preedit_and_limit(
            &candidates,
            &[0, 1, 2, 3],
            Some("nihao"),
            "前文",
            Some(&scorer),
            RerankOptions {
                rerank_count: 4,
                effective_count: 3,
                inference_count_limit: DEFAULT_INFERENCE_COUNT_LIMIT,
                ignore_emoji: true,
            },
        )
        .unwrap();

        assert_eq!(scorer.seen.borrow().as_slice(), ["你好"]);
        assert_eq!(result.result.candidates, candidates);
        assert_eq!(result.result.diagnostics[0].llm_candidate, Some(c("你好")));

        scorer.seen.borrow_mut().clear();
        let result_without_filter =
            try_rerank_selected_candidates_with_scorer_and_preedit_and_limit(
                &candidates,
                &[0, 1, 2, 3],
                Some("nihao"),
                "前文",
                Some(&scorer),
                RerankOptions {
                    rerank_count: 4,
                    effective_count: 3,
                    inference_count_limit: DEFAULT_INFERENCE_COUNT_LIMIT,
                    ignore_emoji: false,
                },
            )
            .unwrap();

        assert_eq!(scorer.seen.borrow().as_slice(), ["你好", "😀", "中文😀"]);
        assert_eq!(result_without_filter.result.candidates, candidates);
    }

    #[test]
    fn all_emoji_candidates_keep_rime_only_behavior() {
        struct PanickingScorer;

        impl CandidateScorer for PanickingScorer {
            fn score_candidates(
                &self,
                _: &str,
                _: &[Candidate],
            ) -> Result<Vec<CandidateScore>, String> {
                panic!("an empty Emoji-filtered pool must not invoke the scorer");
            }
        }

        let candidates = vec![c("😀"), c("中文😀")];
        let result = try_rerank_selected_candidates_with_scorer_and_preedit_and_limit(
            &candidates,
            &[0, 1],
            Some("nihao"),
            "前文",
            Some(&PanickingScorer),
            RerankOptions {
                rerank_count: 2,
                effective_count: 1,
                inference_count_limit: DEFAULT_INFERENCE_COUNT_LIMIT,
                ignore_emoji: true,
            },
        )
        .unwrap();

        assert_eq!(result.result.candidates, candidates);
        assert!(result.llm_performance.is_none());
        assert!(result
            .result
            .diagnostics
            .iter()
            .all(|row| row.llm_candidate.is_none() && row.logprobs.is_empty()));
    }

    #[test]
    fn all_excluded_english_candidates_do_not_call_the_scorer() {
        use std::cell::Cell;

        struct CountingScorer {
            calls: Cell<u32>,
        }

        impl CandidateScorer for CountingScorer {
            fn score_candidates(
                &self,
                _: &str,
                _: &[Candidate],
            ) -> Result<Vec<CandidateScore>, String> {
                self.calls.set(self.calls.get() + 1);
                Ok(Vec::new())
            }
        }

        let candidates = vec![c("hello"), c("world")];
        let scorer = CountingScorer {
            calls: Cell::new(0),
        };
        let result = try_rerank_selected_candidates_with_scorer_and_preedit(
            &candidates,
            &[0, 1],
            Some("nihao"),
            "前文",
            Some(&scorer),
            2,
            1,
        )
        .unwrap();

        assert_eq!(scorer.calls.get(), 0);
        assert_eq!(result.result.candidates, candidates);
        assert!(result.llm_performance.is_none());
        assert!(result
            .result
            .diagnostics
            .iter()
            .all(|row| row.llm_candidate.is_none() && row.logprobs.is_empty()));
    }

    #[test]
    fn selected_pool_is_limited_to_the_configured_rime_prefix() {
        assert_eq!(selected_pool_indices(&[1, 3, 4, 6], 6, 3), vec![1]);
        assert_eq!(selected_pool_indices(&[1, 1, 9, 2], 4, 3), vec![1, 2]);
    }

    #[test]
    fn only_scored_complete_candidates_can_be_promoted() {
        let input = vec![c("不完整"), c("完整甲"), c("完整乙"), c("完整丙")];
        let score = |value| CandidateScore {
            token_ids: Vec::new(),
            token_logprobs: vec![value],
            logprob: value,
            mismatch: false,
        };
        let (result, _) = build_rerank_result_with_indices(
            &input,
            true,
            vec![
                (3, input[3].clone(), score(-1.0)),
                (1, input[1].clone(), score(-2.0)),
                (2, input[2].clone(), score(-3.0)),
            ],
            2,
        );
        let (_, indices) = build_rerank_result_with_indices(
            &input,
            true,
            vec![
                (3, input[3].clone(), score(-1.0)),
                (1, input[1].clone(), score(-2.0)),
                (2, input[2].clone(), score(-3.0)),
            ],
            2,
        );
        assert_eq!(indices, vec![3, 1, 0, 2]);

        assert_eq!(
            result
                .candidates
                .iter()
                .map(|candidate| candidate.commit_text.as_str())
                .collect::<Vec<_>>(),
            vec!["完整丙", "完整甲", "不完整", "完整乙"]
        );
        assert_eq!(
            result
                .diagnostics
                .iter()
                .filter_map(|row| row.llm_candidate.as_ref())
                .map(|candidate| candidate.commit_text.as_str())
                .collect::<Vec<_>>(),
            vec!["完整丙", "完整甲", "完整乙"]
        );
    }
}
