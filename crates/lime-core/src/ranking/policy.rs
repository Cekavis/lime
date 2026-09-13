use super::*;

pub(super) fn candidate_allowed_for_llm(candidate: &Candidate, preedit: &str) -> bool {
    !is_english_candidate(candidate) || candidate.commit_text == preedit
}

pub(super) fn is_english_candidate(candidate: &Candidate) -> bool {
    let text = candidate.commit_text.as_str();
    !text.is_empty() && text.is_ascii() && text.bytes().any(|byte| byte.is_ascii_alphabetic())
}

pub(super) fn contains_emoji(text: &str) -> bool {
    text.chars().any(is_emoji_code_point)
}

pub(super) fn is_emoji_code_point(character: char) -> bool {
    let code = character as u32;
    matches!(
        code,
        0x1F000..=0x1FAFF
            | 0x2194..=0x21FF
            | 0x2300..=0x23FF
            | 0x25AA..=0x25FF
            | 0x2600..=0x27BF
            | 0x2934..=0x2935
            | 0x2B00..=0x2BFF
            | 0x00A9
            | 0x00AE
            | 0x203C
            | 0x2049
            | 0x2122
            | 0x2139
            | 0x3030
            | 0x303D
            | 0x3297
            | 0x3299
    )
}

pub(super) fn selected_pool_indices(
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

pub(super) fn build_rerank_result_with_indices(
    candidates: &[Candidate],
    model_active: bool,
    score_rows: Vec<(usize, Candidate, CandidateScore)>,
    effective_count: usize,
) -> (RerankResult, Vec<usize>) {
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
    let mut final_indices = if model_active {
        score_rows
            .iter()
            .take(effective_count.min(score_rows.len()))
            .map(|(index, _, _)| *index)
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
            final_indices.push(index);
        }
    }
    if !model_active {
        // The no-model path must preserve the exact Rime order, including duplicate entries.
        final_order = candidates.to_vec();
        final_indices = (0..candidates.len()).collect();
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

    (
        RerankResult {
            candidates: final_order,
            diagnostics,
        },
        final_indices,
    )
}
