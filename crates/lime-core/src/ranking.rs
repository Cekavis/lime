use lime_protocol::{Candidate, CandidateDiagnostic};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

pub use crate::llama::{CandidateScore, LlamaRuntime, ModelMetadata, TokenInfo};

/// Result of candidate ranking together with the rows consumed by the management UI.
#[derive(Clone, Debug, PartialEq)]
pub struct RerankResult {
    pub candidates: Vec<Candidate>,
    pub diagnostics: Vec<CandidateDiagnostic>,
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
    let pool_indices = selected_pool_indices(candidate_indices, candidates.len(), rerank_count);
    let pool_candidates = pool_indices
        .iter()
        .map(|index| candidates[*index].clone())
        .collect::<Vec<_>>();

    let score_rows = if let Some(runtime) = runtime {
        let scores = runtime.score_candidates(preceding_text, &pool_candidates)?;
        if scores.len() != pool_candidates.len() {
            return Err(format!(
                "llama.cpp returned {} scores for {} candidates",
                scores.len(),
                pool_candidates.len(),
            ));
        }

        let mut ranked = pool_indices
            .iter()
            .copied()
            .zip(pool_candidates)
            .zip(scores)
            .map(|((index, candidate), score)| (index, candidate, score))
            .collect::<Vec<_>>();
        // Larger log probabilities are better. Keep original Rime order as a stable tie breaker.
        ranked.sort_by(|(left_index, _, left), (right_index, _, right)| {
            right
                .logprob
                .total_cmp(&left.logprob)
                .then_with(|| left_index.cmp(right_index))
        });

        ranked
    } else {
        Vec::new()
    };

    Ok(build_rerank_result(
        candidates,
        runtime.is_some(),
        score_rows,
        effective_count,
    ))
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
