use lime_protocol::{Candidate, CandidateDiagnostic, LlmPerformance};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

pub use crate::llama::{CandidateScore, LlamaRuntime, ModelMetadata, TokenInfo};

/// Result of candidate ranking together with the rows consumed by the management UI.
#[derive(Clone, Debug, PartialEq)]
pub struct RerankResult {
    pub candidates: Vec<Candidate>,
    pub diagnostics: Vec<CandidateDiagnostic>,
    pub llm_performance: Option<LlmPerformance>,
}

#[derive(Clone, Debug)]
struct ScoringOutput {
    scores: Vec<CandidateScore>,
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
        })
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
) -> Result<RerankResult, String> {
    try_rerank_selected_candidates_with_scorer(
        candidates,
        candidate_indices,
        preceding_text,
        runtime,
        rerank_count,
        effective_count,
    )
}

/// Production entry point that also applies the input-aware English-candidate policy.
///
/// English candidates are useful only when Rime returns the exact pinyin text the user typed.
/// Other English words remain available in the final Rime order, but are not sent to the model.
pub(crate) fn try_rerank_selected_candidates_with_preedit(
    candidates: &[Candidate],
    candidate_indices: &[usize],
    preedit: &str,
    preceding_text: &str,
    runtime: Option<&LlamaRuntime>,
    rerank_count: usize,
    effective_count: usize,
) -> Result<RerankResult, String> {
    try_rerank_selected_candidates_with_scorer_and_preedit(
        candidates,
        candidate_indices,
        Some(preedit),
        preceding_text,
        runtime,
        rerank_count,
        effective_count,
    )
}

fn try_rerank_selected_candidates_with_scorer<S: CandidateScorer + ?Sized>(
    candidates: &[Candidate],
    candidate_indices: &[usize],
    preceding_text: &str,
    runtime: Option<&S>,
    rerank_count: usize,
    effective_count: usize,
) -> Result<RerankResult, String> {
    try_rerank_selected_candidates_with_scorer_and_preedit(
        candidates,
        candidate_indices,
        None,
        preceding_text,
        runtime,
        rerank_count,
        effective_count,
    )
}

fn try_rerank_selected_candidates_with_scorer_and_preedit<S: CandidateScorer + ?Sized>(
    candidates: &[Candidate],
    candidate_indices: &[usize],
    preedit: Option<&str>,
    preceding_text: &str,
    runtime: Option<&S>,
    rerank_count: usize,
    effective_count: usize,
) -> Result<RerankResult, String> {
    let pool_indices = selected_pool_indices(candidate_indices, candidates.len(), rerank_count);
    let pool_indices = match preedit {
        Some(preedit) => pool_indices
            .into_iter()
            .filter(|index| candidate_allowed_for_llm(&candidates[*index], preedit))
            .collect::<Vec<_>>(),
        None => pool_indices,
    };
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
        let scored = runtime.score_candidates_with_performance(preceding_text, &pool_candidates)?;
        if scored.scores.len() != pool_candidates.len() {
            return Err(format!(
                "llama.cpp returned {} scores for {} candidates",
                scored.scores.len(),
                pool_candidates.len(),
            ));
        }

        let mut ranked = pool_indices
            .iter()
            .copied()
            .zip(pool_candidates)
            .zip(scored.scores)
            .map(|((index, candidate), score)| (index, candidate, score))
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

    let mut result = build_rerank_result(candidates, model_active, score_rows, effective_count);
    result.llm_performance = llm_performance;
    Ok(result)
}

fn candidate_allowed_for_llm(candidate: &Candidate, preedit: &str) -> bool {
    !is_english_candidate(candidate) || candidate.commit_text == preedit
}

fn is_english_candidate(candidate: &Candidate) -> bool {
    let text = candidate.commit_text.as_str();
    !text.is_empty() && text.is_ascii() && text.bytes().any(|byte| byte.is_ascii_alphabetic())
}

fn selected_pool_indices(
    candidate_indices: &[usize],
    candidate_count: usize,
    rerank_count: usize,
) -> Vec<usize> {
    let mut seen = HashSet::new();
    let prefix_len = candidate_count.min(rerank_count);
    candidate_indices
        .iter()
        .copied()
        .filter(|index| *index < prefix_len && seen.insert(*index))
        .collect()
}

fn build_rerank_result(
    candidates: &[Candidate],
    model_active: bool,
    score_rows: Vec<(usize, Candidate, CandidateScore)>,
    effective_count: usize,
) -> RerankResult {
    let llm_order = score_rows
        .iter()
        .map(|(_, candidate, _)| candidate.clone())
        .collect::<Vec<_>>();

    let mut final_order = if model_active {
        score_rows
            .iter()
            .take(effective_count.min(score_rows.len()))
            .map(|(_, candidate, _)| candidate.clone())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let promoted_indices = score_rows
        .iter()
        .take(effective_count.min(score_rows.len()))
        .map(|(index, _, _)| *index)
        .collect::<HashSet<_>>();
    for (index, candidate) in candidates.iter().enumerate() {
        if !promoted_indices.contains(&index) {
            final_order.push(candidate.clone());
        }
    }
    if !model_active {
        // The no-model path must preserve the exact Rime order, including duplicate entries.
        final_order = candidates.to_vec();
    }

    let row_count = candidates.len().max(llm_order.len()).max(final_order.len());
    let diagnostics = (0..row_count)
        .map(|index| {
            let rime_candidate = candidates.get(index).cloned();
            let llm_candidate = model_active
                .then(|| llm_order.get(index).cloned())
                .flatten();
            let score = if model_active {
                score_rows.get(index).map(|(_, _, score)| score)
            } else {
                None
            };
            CandidateDiagnostic {
                rank: index.saturating_add(1) as u32,
                rime_candidate,
                llm_candidate,
                logprob: score.map_or(0.0, |score| score.logprob),
                logprobs: score
                    .map(|score| score.token_logprobs.clone())
                    .unwrap_or_default(),
                mismatch: score.is_some_and(|score| score.mismatch),
                display_candidate: final_order.get(index).cloned(),
            }
        })
        .collect();

    RerankResult {
        candidates: final_order,
        diagnostics,
        llm_performance: None,
    }
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
    fn english_candidates_are_scored_only_when_they_equal_the_preedit() {
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

        let candidates = vec![c("你好"), c("hello"), c("nihao"), c("3D打印"), c("ni-hao")];
        let scorer = RecordingScorer {
            seen: RefCell::new(Vec::new()),
        };
        let result = try_rerank_selected_candidates_with_scorer_and_preedit(
            &candidates,
            &[0, 1, 2, 3, 4],
            Some("nihao"),
            "前文",
            Some(&scorer),
            5,
            2,
        )
        .unwrap();

        assert_eq!(scorer.seen.into_inner(), vec!["你好", "nihao", "3D打印"]);
        assert_eq!(
            result
                .diagnostics
                .iter()
                .filter_map(|row| row.llm_candidate.as_ref())
                .map(|candidate| candidate.commit_text.as_str())
                .collect::<Vec<_>>(),
            vec!["你好", "nihao", "3D打印"]
        );
        assert!(result
            .candidates
            .iter()
            .any(|candidate| candidate.commit_text == "hello"));
        assert!(result
            .candidates
            .iter()
            .any(|candidate| candidate.commit_text == "ni-hao"));
        let performance = result
            .llm_performance
            .as_ref()
            .expect("scored candidates should have performance");
        assert_eq!(performance.candidate_count, 3);
        assert_eq!(performance.scored_count, 3);
        assert_eq!(performance.target_token_count, 3);
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
        assert_eq!(result.candidates, candidates);
        assert!(result.llm_performance.is_none());
        assert!(result
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
        let result = build_rerank_result(
            &input,
            true,
            vec![
                (3, input[3].clone(), score(-1.0)),
                (1, input[1].clone(), score(-2.0)),
                (2, input[2].clone(), score(-3.0)),
            ],
            2,
        );

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
