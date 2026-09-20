use super::*;

#[test]
fn joint_tokenization_replays_context_for_every_candidate() {
    let base = [1, 2, 3, 4];
    let combined = vec![vec![1, 2, 3, 9], vec![1, 2, 8], vec![1, 2, 3, 4, 5]];
    let (prefix, plans) = plan_joint_candidates(&base, combined.clone()).unwrap();
    assert_eq!(prefix, 2);
    assert_eq!(
        plans.iter().map(|plan| plan.mismatch).collect::<Vec<_>>(),
        [true, true, false]
    );
    for (plan, expected) in plans.iter().zip(combined) {
        assert_eq!([&base[..prefix], &plan.score_ids].concat(), expected);
    }
    assert_eq!(plans[2].score_ids, [3, 4, 5]);
}

#[test]
fn joint_tokenization_keeps_exact_boundaries_and_allows_empty_anchor() {
    let (prefix, plans) =
        plan_joint_candidates(&[1, 2], vec![vec![1, 2, 3], vec![1, 2, 4]]).unwrap();
    assert_eq!(prefix, 2);
    assert_eq!(plans[0].score_ids, [3]);
    assert!(plans.iter().all(|plan| !plan.mismatch));
    // Including the original context in the LCP prevents swallowing shared candidate text.
    let (prefix, plans) = plan_joint_candidates(&[1], vec![vec![1, 2, 3], vec![1, 2, 4]]).unwrap();
    assert_eq!(prefix, 1);
    assert_eq!(plans[0].score_ids, [2, 3]);
    let (prefix, plans) = plan_joint_candidates(&[1], vec![vec![2], vec![1, 3]]).unwrap();
    assert_eq!(prefix, 0);
    assert_eq!(plans[0].score_ids, [2]);
    assert_eq!(plans[1].score_ids, [1, 3]);
    assert!(plan_joint_candidates(&[1], vec![vec![1]]).is_err());
}

fn plans(lengths: &[usize]) -> Vec<CandidatePlan> {
    lengths
        .iter()
        .map(|length| CandidatePlan {
            score_ids: vec![1; *length],
            mismatch: false,
        })
        .collect()
}

#[test]
fn all_batches_share_one_context_truncation_window() {
    for path in [ScoringPath::PackedAttention, ScoringPath::PaddedRecurrent] {
        let candidates = plans(&[1, 2, 4, 5]);
        let batches = plan_scoring_batches(&candidates, path, 2, 12, 10, 8).unwrap();
        assert_eq!(batches.indices, [vec![0, 1], vec![2, 3]]);
        let expected_budget = match path {
            ScoringPath::PackedAttention => 5,
            ScoringPath::PaddedRecurrent => 4,
        };
        assert_eq!(batches.base_start, 10 - expected_budget);
        for chunk in batches.indices {
            let continuation = match path {
                ScoringPath::PackedAttention => chunk
                    .iter()
                    .map(|i| candidates[*i].score_ids.len() - 1)
                    .sum(),
                ScoringPath::PaddedRecurrent => {
                    chunk
                        .iter()
                        .map(|i| candidates[*i].score_ids.len() - 1)
                        .max()
                        .unwrap()
                        * chunk
                            .iter()
                            .filter(|i| candidates[**i].score_ids.len() > 1)
                            .count()
                }
            };
            assert!(expected_budget + continuation <= 12);
        }
    }
}

#[test]
fn batch_limit_keeps_index_mapping_and_ignores_unscored_context_budgets() {
    for path in [ScoringPath::PackedAttention, ScoringPath::PaddedRecurrent] {
        let batches = plan_scoring_batches(&plans(&[5, 1, 2, 1]), path, 2, 5, 4, 1).unwrap();
        assert_eq!(batches.indices, [vec![1, 3], vec![2]]);
        assert_eq!(batches.base_start, 0);
        let unlimited = plan_scoring_batches(&plans(&[5, 1, 2, 1]), path, 2, 5, 4, 2).unwrap();
        assert_eq!(unlimited.indices, [vec![1, 3], vec![2], vec![0]]);
        assert_eq!(unlimited.base_start, 3);
        assert!(plan_scoring_batches(&plans(&[1]), path, 0, 5, 1, 1).is_err());
        assert!(plan_scoring_batches(&plans(&[6]), path, 2, 5, 1, 1).is_err());
    }
}

#[test]
fn rollback_preview_preserves_exact_unicode_and_whitespace() {
    let context = " \n一直😀用的是公司";
    let prefix = " \n一直😀用的是";
    let rollback = describe_boundary_rollback(context, prefix.as_bytes(), 6, 7);
    assert_eq!(rollback.prefix_token_count, 6);
    assert_eq!(rollback.replayed_token_count, 1);
    assert_eq!(rollback.prefix_text.as_deref(), Some(prefix));
    assert_eq!(rollback.replayed_text.as_deref(), Some("公司"));
    let start = describe_boundary_rollback(context, &[], 0, 7);
    assert_eq!(start.prefix_text.as_deref(), Some(""));
    assert_eq!(start.replayed_text.as_deref(), Some(context));
}

#[test]
fn rollback_preview_never_invents_character_offsets() {
    for bytes in [&"😀".as_bytes()[..2], b"normalized ".as_slice()] {
        let rollback = describe_boundary_rollback("😀公司", bytes, 1, 3);
        assert_eq!(rollback.prefix_token_count, 1);
        assert_eq!(rollback.replayed_token_count, 2);
        assert!(rollback.prefix_text.is_none());
        assert!(rollback.replayed_text.is_none());
    }
}

/// Full-logit teacher forcing in a separate native context, without the production sampler.
/// Match the native batch shape: quantized kernels can differ between single- and multi-row
/// matrix products. Comparing those different computations would test an invalid invariant.
fn reference_logprobs(
    runtime: &LlamaRuntime,
    base: &[llama_token],
    suffixes: &[Vec<llama_token>],
) -> Vec<Vec<f64>> {
    assert!(
        !base.is_empty(),
        "the oracle requires an explicit document boundary"
    );
    let capacity = runtime.runtime_context_tokens;
    let sequence_capacity = runtime.gpu_samplers.lock().unwrap().states.len();
    let mut params = LlamaContext::default_params(&runtime.model);
    params.n_ctx = capacity as u32;
    params.n_batch = capacity as u32;
    params.n_ubatch = capacity as u32;
    params.n_seq_max = sequence_capacity as u32;
    params.n_outputs_max = capacity as u32;
    params.n_outputs_max_per_seq = capacity as u32;
    params.kv_unified = true;
    let mut context = LlamaContext::new(&runtime.model, params).unwrap();
    let sequences = (0..suffixes.len() as i32).collect::<Vec<_>>();
    let mut batch = LlamaBatch::new(
        runtime.backend.lib.clone(),
        capacity as i32,
        0,
        sequence_capacity as i32,
    );
    for (position, token) in base.iter().enumerate() {
        batch.add(
            *token,
            position as i32,
            &sequences,
            position + 1 == base.len(),
        );
    }
    context.decode(&batch).unwrap();
    let read = |row: i32, token: llama_token| {
        let pointer =
            unsafe { (runtime.backend.lib.symbols.llama_get_logits_ith)(context.handle, row) };
        assert!(!pointer.is_null());
        logprob_for(
            unsafe { std::slice::from_raw_parts(pointer, runtime.vocab_size) },
            token,
        )
    };
    let mut result = suffixes
        .iter()
        .map(|suffix| vec![read(-1, suffix[0])])
        .collect::<Vec<_>>();
    let longest = suffixes
        .iter()
        .map(|suffix| suffix.len() - 1)
        .max()
        .unwrap_or(0);
    if longest > 0 {
        let mut continuation = LlamaBatch::new(
            runtime.backend.lib.clone(),
            capacity as i32,
            0,
            sequence_capacity as i32,
        );
        let mut targets = Vec::new();
        for (sequence, suffix) in suffixes
            .iter()
            .enumerate()
            .filter(|(_, suffix)| suffix.len() > 1)
        {
            let rows = match runtime.scoring_path {
                ScoringPath::PackedAttention => suffix.len() - 1,
                ScoringPath::PaddedRecurrent => longest,
            };
            for position in 0..rows {
                let target = suffix.get(position + 1).copied();
                if let Some(token) = target {
                    targets.push((continuation.handle.n_tokens, sequence, token));
                }
                continuation.add(
                    suffix
                        .get(position)
                        .copied()
                        .unwrap_or(runtime.model.get_vocab().eos()),
                    (base.len() + position) as i32,
                    &[sequence as i32],
                    target.is_some(),
                );
            }
        }
        context.decode(&continuation).unwrap();
        for (row, sequence, token) in targets {
            let pointer =
                unsafe { (runtime.backend.lib.symbols.llama_get_logits_ith)(context.handle, row) };
            assert!(!pointer.is_null());
            result[sequence].push(logprob_for(
                unsafe { std::slice::from_raw_parts(pointer, runtime.vocab_size) },
                token,
            ));
        }
    }
    result
}

#[test]
#[ignore = "requires a local Qwen GGUF model and llama.cpp CPU runtime"]
fn configured_runtime_scores_joint_boundaries_against_full_logits() {
    let model = std::env::var_os("LIME_LLAMA_TEST_MODEL")
        .expect("LIME_LLAMA_TEST_MODEL must point to a Qwen GGUF");
    let runtime_dir = std::env::var_os("LIME_LLAMA_RUNTIME_DIR")
        .expect("LIME_LLAMA_RUNTIME_DIR must point to llama.cpp runtime");
    let mut runtime =
        LlamaRuntime::load_with_runtime_dir_and_backend_preference_and_sequence_count(
            model,
            runtime_dir,
            256,
            BackendPreference::Cpu,
            8,
        )
        .unwrap();
    for (context, prefix_text) in [("一直用的是公司", "一直用的是"), ("公司", "")] {
        let candidates = ["的", "得", "地", "德", "锝", "嘚"].map(|text| Candidate {
            display_text: text.into(),
            commit_text: text.into(),
        });
        let base = runtime.model.tokenize(context, false, false).unwrap();
        let shared = runtime.model.tokenize(prefix_text, false, false).unwrap();
        // These Qwen models use endoftext as the document boundary. Resolve it independently of
        // the production BOS/EOS metadata helper, which must not accept a default comma as BOS.
        let reference_base = if shared.is_empty() {
            let tokens = runtime
                .model
                .tokenize("<|endoftext|>", false, true)
                .unwrap();
            assert_eq!(tokens.len(), 1);
            assert_eq!(
                runtime.model.token_to_piece_bytes(tokens[0]).unwrap(),
                b"<|endoftext|>"
            );
            tokens
        } else {
            shared.clone()
        };
        assert!(base.starts_with(&shared));
        let mut reference = Vec::new();
        for candidate in &candidates {
            let joint = runtime
                .model
                .tokenize(&format!("{context}{}", candidate.commit_text), false, false)
                .unwrap();
            assert!(joint.starts_with(&shared));
            let suffix = joint[shared.len()..].to_vec();
            reference.push(suffix);
        }
        // Exercise both one batch and forced splitting without changing the scoring anchor.
        for sequence_count in [8, 2] {
            runtime.sequence_count = sequence_count;
            let result = runtime
                .score_candidates_with_inference_limit(context, &candidates, 8)
                .unwrap();
            assert_eq!(result.scored_indices, [0, 1, 2, 3, 4, 5]);
            let rollback = result.performance.boundary_rollback.as_ref().unwrap();
            assert_eq!(rollback.prefix_token_count as usize, shared.len());
            assert_eq!(
                rollback.replayed_token_count as usize,
                base.len() - shared.len()
            );
            assert_eq!(rollback.prefix_text.as_deref(), Some(prefix_text));
            assert_eq!(rollback.replayed_text.as_deref(), Some("公司"));
            assert!(result.scores[0].mismatch);
            assert!(result.scores[1..].iter().all(|score| !score.mismatch));
            let mut expected_scores = Vec::new();
            for suffixes in reference.chunks(sequence_count) {
                expected_scores.extend(reference_logprobs(&runtime, &reference_base, suffixes));
            }
            for ((score, ids), expected) in
                result.scores.iter().zip(&reference).zip(&expected_scores)
            {
                assert_eq!(&score.token_ids, ids);
                assert_eq!(score.token_logprobs.len(), expected.len());
                for (actual, expected) in score.token_logprobs.iter().zip(expected) {
                    assert!(
                        (actual - expected).abs() < 1e-4,
                        "full-logit oracle mismatch: {actual} vs {expected}"
                    );
                }
                assert!((score.logprob - expected.iter().sum::<f64>()).abs() < 1e-4);
            }
            println!(
                "BOUNDARY {:?} {:?}",
                result
                    .scores
                    .iter()
                    .map(|score| score.logprob)
                    .collect::<Vec<_>>(),
                result.performance
            );
        }
    }
}
