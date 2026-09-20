use super::*;

#[derive(Clone, Debug)]
pub(super) struct CandidatePlan {
    pub(super) score_ids: Vec<llama_token>,
    pub(super) mismatch: bool,
}

/// Keep the same token anchor for every candidate, including candidates that do not themselves
/// cross the original context boundary. Each suffix belongs to the joint canonical tokenization.
pub(super) fn plan_joint_candidates(
    base: &[llama_token],
    combined: Vec<Vec<llama_token>>,
) -> Result<(usize, Vec<CandidatePlan>), String> {
    let prefix_len = combined.iter().fold(base.len(), |length, tokens| {
        base[..length]
            .iter()
            .zip(tokens)
            .take_while(|(left, right)| left == right)
            .count()
    });
    let plans = combined
        .into_iter()
        .enumerate()
        .map(|(index, tokens)| {
            if tokens.len() <= prefix_len {
                return Err(format!(
                    "candidate {index} has no tokens after the shared context"
                ));
            }
            Ok(CandidatePlan {
                mismatch: !tokens.starts_with(base),
                score_ids: tokens[prefix_len..].to_vec(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((prefix_len, plans))
}

pub(super) struct CandidateBatches {
    pub(super) indices: Vec<Vec<usize>>,
    pub(super) base_start: usize,
}

/// Plan all admitted batches before decoding so every score uses the same truncated history.
pub(super) fn plan_scoring_batches(
    plans: &[CandidatePlan],
    path: ScoringPath,
    sequence_count: usize,
    context_limit: usize,
    base_len: usize,
    inference_count_limit: usize,
) -> Result<CandidateBatches, String> {
    let mut order = (0..plans.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| (plans[*index].score_ids.len(), *index));
    let sorted = order
        .iter()
        .map(|index| plans[*index].clone())
        .collect::<Vec<_>>();
    let mut indices = Vec::new();
    let mut start = 0;
    let mut base_budget = context_limit;
    let mut inference_count = 0;
    while start < order.len() {
        let remaining = &sorted[start..];
        let (length, budget) = match path {
            ScoringPath::PackedAttention => {
                plan_attention_chunk(remaining, sequence_count, context_limit)?
            }
            ScoringPath::PaddedRecurrent => {
                plan_candidate_chunk(remaining, sequence_count, context_limit, base_len)?
            }
        };
        if length == 0 {
            return Err("candidate chunk planner produced an empty chunk".to_owned());
        }
        let has_continuation = remaining[..length]
            .iter()
            .any(|plan| plan.score_ids.len() > 1);
        if has_continuation && inference_count >= inference_count_limit.max(1) {
            break;
        }
        base_budget = base_budget.min(budget);
        indices.push(order[start..start + length].to_vec());
        inference_count += usize::from(has_continuation);
        start += length;
    }
    Ok(CandidateBatches {
        indices,
        base_start: base_len.saturating_sub(base_budget),
    })
}

/// Text previews are optional: token boundaries can fall inside UTF-8 characters, and some
/// tokenizers normalize their input. Never infer source offsets from lossy token-piece strings.
pub(super) fn describe_boundary_rollback(
    preceding_text: &str,
    prefix_bytes: &[u8],
    prefix_token_count: usize,
    context_token_count: usize,
) -> lime_protocol::LlmBoundaryRollback {
    let split = std::str::from_utf8(prefix_bytes)
        .ok()
        .filter(|prefix| preceding_text.starts_with(prefix))
        .map(|prefix| prefix.len());
    lime_protocol::LlmBoundaryRollback {
        prefix_token_count: prefix_token_count.min(u32::MAX as usize) as u32,
        replayed_token_count: context_token_count
            .saturating_sub(prefix_token_count)
            .min(u32::MAX as usize) as u32,
        prefix_text: split.map(|position| preceding_text[..position].to_owned()),
        replayed_text: split.map(|position| preceding_text[position..].to_owned()),
    }
}

/// llama.cpp may report tokenizer defaults as BOS even when the GGUF has no BOS metadata
/// (Qwen3.5's GPT2 default is an ordinary comma). Use only a declared boundary token. A declared
/// EOS provides the document boundary for models without BOS; never silently inject plain text.
fn empty_context_token(model: &LlamaModel) -> Result<llama_token, String> {
    let vocab = model.get_vocab();
    let vocab_size = unsafe { (model.backend.symbols.llama_vocab_n_tokens)(vocab.handle) };
    for key in ["tokenizer.ggml.bos_token_id", "tokenizer.ggml.eos_token_id"] {
        if let Some(value) = model.metadata(key) {
            let token = value
                .parse::<llama_token>()
                .map_err(|_| format!("invalid GGUF boundary token metadata: {key}"))?;
            if token < 0 || token >= vocab_size {
                return Err(format!(
                    "GGUF boundary token is outside the vocabulary: {key}"
                ));
            }
            if key == "tokenizer.ggml.eos_token_id" && !vocab.is_eog(token) {
                return Err("GGUF EOS is not an end-of-generation token".to_owned());
            }
            return Ok(token);
        }
    }
    Err("cannot score an empty shared context: GGUF declares neither BOS nor EOS".to_owned())
}

/// Choose the largest first chunk that fits llama.cpp's padded continuation
/// budget. A candidate that fits by itself may still force the current chunk
/// to split because every active sequence is padded to the longest suffix.
pub(super) fn plan_candidate_chunk(
    plans: &[CandidatePlan],
    sequence_count: usize,
    context_limit: usize,
    base_token_count: usize,
) -> Result<(usize, usize), String> {
    if sequence_count == 0 {
        return Err("llama sequence capacity is zero".to_owned());
    }
    let mut chunk_len = 0_usize;
    let mut longest_prefix = 0_usize;
    let mut active_prefix_count = 0_usize;
    for plan in plans.iter().take(sequence_count) {
        let candidate_prefix = plan.score_ids.len().saturating_sub(1);
        let next_longest = longest_prefix.max(candidate_prefix);
        if next_longest >= context_limit {
            return Err(format!(
                "candidate requires {} decoded prefix tokens, context limit is {context_limit}",
                next_longest
            ));
        }
        let next_active_prefix_count =
            active_prefix_count.saturating_add(usize::from(candidate_prefix > 0));
        let continuation_budget = next_longest.saturating_mul(next_active_prefix_count);
        if continuation_budget >= context_limit {
            // The candidate fits individually, but adding it would overfill
            // this padded batch. Leave it for the next chunk.
            break;
        }
        let base_budget = context_limit - continuation_budget;
        let decoded_base_tokens = if base_token_count == 0 {
            1 // scoring supplies a document-boundary token for an empty shared context
        } else {
            base_token_count.min(base_budget)
        };
        let next_batch_tokens = decoded_base_tokens.saturating_add(continuation_budget);
        if next_batch_tokens > context_limit {
            if chunk_len == 0 {
                return Err(format!(
                    "candidate decode requires {next_batch_tokens} tokens, context limit is {context_limit}"
                ));
            }
            break;
        }
        longest_prefix = next_longest;
        active_prefix_count = next_active_prefix_count;
        chunk_len += 1;
    }
    let base_budget =
        context_limit.saturating_sub(longest_prefix.saturating_mul(active_prefix_count));
    Ok((chunk_len, base_budget))
}

/// Choose the largest short-first chunk for the ragged packed attention tree. Unlike the
/// recurrent planner, each continuation contributes only its real prefix length.
pub(super) fn plan_attention_chunk(
    plans: &[CandidatePlan],
    sequence_count: usize,
    context_limit: usize,
) -> Result<(usize, usize), String> {
    if sequence_count == 0 {
        return Err("llama sequence capacity is zero".to_owned());
    }
    let minimum_base_tokens = 1;
    let mut chunk_len = 0_usize;
    let mut continuation_budget = 0_usize;
    for plan in plans.iter().take(sequence_count) {
        let next_budget =
            continuation_budget.saturating_add(plan.score_ids.len().saturating_sub(1));
        if next_budget.saturating_add(minimum_base_tokens) > context_limit {
            if chunk_len == 0 {
                return Err(format!(
                    "candidate requires {} packed continuation tokens, context limit is {context_limit}",
                    plan.score_ids.len().saturating_sub(1)
                ));
            }
            break;
        }
        continuation_budget = next_budget;
        chunk_len += 1;
    }
    if chunk_len == 0 {
        return Err("packed attention planner produced an empty chunk".to_owned());
    }
    let base_budget = context_limit.saturating_sub(continuation_budget);
    Ok((chunk_len, base_budget))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn score_attention_tree_batch(
    backend: &LlamaBackend,
    model: &LlamaModel,
    context: &mut MutexGuard<'_, LlamaContext>,
    gpu_samplers: &mut GpuSamplerSet,
    base_tokens: &[llama_token],
    candidates: &[Vec<llama_token>],
    sequence_count: usize,
    timings: &mut ScoringTimings,
) -> Result<Vec<CandidateScore>, String> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let decode_base = if base_tokens.is_empty() {
        vec![empty_context_token(model)?]
    } else {
        base_tokens.to_vec()
    };
    if candidates.iter().any(|tokens| tokens.is_empty()) {
        return Err("attention tree contains an empty candidate".to_owned());
    }
    let total_tokens = decode_base.len()
        + candidates
            .iter()
            .map(|tokens| tokens.len().saturating_sub(1))
            .sum::<usize>();
    if total_tokens > timings.context_limit {
        return Err(format!(
            "packed attention tree has {total_tokens} tokens, context limit is {}",
            timings.context_limit
        ));
    }
    if candidates.len() > gpu_samplers.states.len() {
        return Err(format!(
            "llama sampler has {} sequence slots, but {} candidates were requested",
            gpu_samplers.states.len(),
            candidates.len()
        ));
    }

    context.kv_cache_clear();
    let sequence_ids = (0..candidates.len() as i32).collect::<Vec<_>>();
    let mut token_logprobs = candidates
        .iter()
        .map(|tokens| Vec::with_capacity(tokens.len()))
        .collect::<Vec<_>>();
    let output_count = 1 + candidates
        .iter()
        .map(|tokens| tokens.len().saturating_sub(1))
        .sum::<usize>();
    timings.add_decode_workload(total_tokens, output_count);

    // Decode the shared context separately. The root row is shared by all sequence ids, while
    // candidate branches are decoded afterwards; putting both in one llama.cpp batch couples the
    // shared prefix to future branch positions and changes the root logits for attention models.
    let mut base_batch = LlamaBatch::new(
        backend.lib.clone(),
        decode_base.len().max(1) as i32,
        0,
        sequence_count as i32,
    );
    for (position, token) in decode_base.iter().copied().enumerate() {
        base_batch.add(
            token,
            position as i32,
            &sequence_ids,
            position + 1 == decode_base.len(),
        );
    }
    let mut base_target_groups = vec![Vec::new(); gpu_samplers.states.len()];
    base_target_groups[0].push(candidates.iter().map(|tokens| tokens[0]).collect());
    configure_gpu_samplers(gpu_samplers, base_target_groups)?;
    let decode_started = Instant::now();
    context
        .decode(&base_batch)
        .map_err(|error| format!("llama.cpp decode failed: {error}"))?;
    timings.add_decode(decode_started);
    unsafe { (backend.lib.symbols.llama_synchronize)(context.handle) };

    let base_state = unsafe { &*gpu_samplers.states[0] };
    let base_values = read_gpu_sampler_row(base_state, 0)?;
    for (candidate_index, value) in base_values.into_iter().enumerate() {
        token_logprobs[candidate_index].push(value);
    }
    let max_continuations = candidates
        .iter()
        .map(|tokens| tokens.len().saturating_sub(1))
        .max()
        .unwrap_or(0);
    if max_continuations > 0 {
        let continuation_tokens = total_tokens.saturating_sub(decode_base.len());
        let mut continuation_batch = LlamaBatch::new(
            backend.lib.clone(),
            continuation_tokens.max(1) as i32,
            0,
            sequence_count as i32,
        );
        let mut continuation_target_groups = vec![Vec::new(); gpu_samplers.states.len()];
        for (candidate_index, tokens) in candidates.iter().enumerate() {
            for token_index in 0..tokens.len().saturating_sub(1) {
                continuation_batch.add(
                    tokens[token_index],
                    (decode_base.len() + token_index) as i32,
                    &[sequence_ids[candidate_index]],
                    true,
                );
                continuation_target_groups[candidate_index].push(vec![tokens[token_index + 1]]);
            }
        }
        configure_gpu_samplers(gpu_samplers, continuation_target_groups)?;
        let decode_started = Instant::now();
        context
            .decode(&continuation_batch)
            .map_err(|error| format!("llama.cpp decode failed: {error}"))?;
        timings.add_decode(decode_started);
        unsafe { (backend.lib.symbols.llama_synchronize)(context.handle) };
        for (candidate_index, tokens) in candidates.iter().enumerate() {
            let state = unsafe { &*gpu_samplers.states[candidate_index] };
            for row_index in 0..tokens.len().saturating_sub(1) {
                let values = read_gpu_sampler_row(state, row_index)?;
                if let Some(value) = values.first().copied() {
                    token_logprobs[candidate_index].push(value);
                }
            }
        }
    }
    if let Some((candidate_index, (lp, tokens))) = token_logprobs
        .iter()
        .zip(candidates)
        .enumerate()
        .find(|(_, (lp, tokens))| {
            lp.len() != tokens.len() || lp.iter().any(|value| !value.is_finite())
        })
    {
        let sampler_state = unsafe { &*gpu_samplers.states[candidate_index] };
        return Err(format!(
            "llama.cpp GPU sampler returned incomplete packed-tree logprob rows for candidate {candidate_index}: got {}, expected {}; sampler results={}, target rows={}, current groups={}",
            lp.len(),
            tokens.len(),
            sampler_state.results.len(),
            sampler_state.target_tensors.len(),
            sampler_state.groups.len(),
        ));
    }
    Ok(token_logprobs
        .into_iter()
        .zip(candidates)
        .map(|(logprobs, tokens)| CandidateScore {
            token_ids: tokens.clone(),
            logprob: logprobs.iter().sum(),
            token_logprobs: logprobs,
            mismatch: false,
        })
        .collect())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn score_recurrent_batch(
    backend: &LlamaBackend,
    model: &LlamaModel,
    context: &mut MutexGuard<'_, LlamaContext>,
    gpu_samplers: &mut GpuSamplerSet,
    base_tokens: &[llama_token],
    candidates: &[Vec<llama_token>],
    sequence_count: usize,
    timings: &mut ScoringTimings,
) -> Result<Vec<CandidateScore>, String> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let decode_base = if base_tokens.is_empty() {
        vec![empty_context_token(model)?]
    } else {
        base_tokens.to_vec()
    };
    if decode_base.len() > timings.context_limit {
        return Err(format!(
            "base prompt has {} tokens, context limit is {}",
            decode_base.len(),
            timings.context_limit
        ));
    }
    if candidates.iter().any(|tokens| {
        tokens.is_empty()
            || decode_base.len() + tokens.len().saturating_sub(1) > timings.context_limit
    }) {
        return Err(format!(
            "candidate exceeds llama context limit of {} tokens",
            timings.context_limit
        ));
    }
    let total_tokens = decode_base.len()
        + candidates
            .iter()
            .map(|tokens| tokens.len().saturating_sub(1))
            .sum::<usize>();
    if total_tokens > timings.context_limit {
        return Err(format!(
            "llama decode batch has {total_tokens} tokens, context limit is {}",
            timings.context_limit
        ));
    }
    let max_continuations = candidates
        .iter()
        .map(|tokens| tokens.len().saturating_sub(1))
        .max()
        .unwrap_or(0);
    let active_candidates = candidates.iter().filter(|tokens| tokens.len() > 1).count();
    let physical_total_tokens = decode_base
        .len()
        .saturating_add(max_continuations.saturating_mul(active_candidates));
    if physical_total_tokens > timings.context_limit {
        return Err(format!(
            "llama padded decode batch has {physical_total_tokens} tokens, context limit is {}",
            timings.context_limit
        ));
    }
    if candidates.len() > gpu_samplers.states.len() {
        return Err(format!(
            "llama sampler has {} sequence slots, but {} candidates were requested",
            gpu_samplers.states.len(),
            candidates.len()
        ));
    }

    context.kv_cache_clear();
    // Sequence ids are zero-based in llama.cpp. Keeping them in `0..N` lets `n_seq_max=N`
    // describe exactly the number of candidate sequences without a reserved extra slot.
    let sequence_ids = (0..candidates.len() as i32).collect::<Vec<_>>();
    let mut token_logprobs = candidates
        .iter()
        .map(|tokens| Vec::with_capacity(tokens.len()))
        .collect::<Vec<_>>();
    timings.add_decode_workload(
        physical_total_tokens,
        1 + candidates
            .iter()
            .map(|tokens| tokens.len().saturating_sub(1))
            .sum::<usize>(),
    );
    // Decode the shared context by itself so every candidate's first token is scored from the
    // same final context row. Later continuation rows are read from the sampler result tensors.
    let mut base_batch = LlamaBatch::new(
        backend.lib.clone(),
        decode_base.len().max(1) as i32,
        0,
        sequence_count as i32,
    );
    for (position, token) in decode_base.iter().copied().enumerate() {
        base_batch.add(
            token,
            position as i32,
            &sequence_ids,
            position + 1 == decode_base.len(),
        );
    }
    let mut base_target_groups = vec![Vec::new(); gpu_samplers.states.len()];
    base_target_groups[0].push(candidates.iter().map(|tokens| tokens[0]).collect());
    configure_gpu_samplers(gpu_samplers, base_target_groups)?;

    let decode_started = Instant::now();
    context
        .decode(&base_batch)
        .map_err(|error| format!("llama.cpp decode failed: {error}"))?;
    timings.add_decode(decode_started);
    unsafe { (backend.lib.symbols.llama_synchronize)(context.handle) };
    let base_state = unsafe { &*gpu_samplers.states[0] };
    let base_values = read_gpu_sampler_row(base_state, 0)?;
    for (candidate_index, value) in base_values.into_iter().enumerate() {
        token_logprobs[candidate_index].push(value);
    }
    // Submit all candidate continuation prefixes in one logical batch. Padding keeps the
    // recurrent sequences equal-length inside llama.cpp; only real candidate positions request
    // sampler output.
    let max_continuations = candidates
        .iter()
        .map(|tokens| tokens.len().saturating_sub(1))
        .max()
        .unwrap_or(0);
    if max_continuations > 0 {
        let active_candidates = candidates
            .iter()
            .enumerate()
            .filter_map(|(candidate_index, tokens)| (tokens.len() > 1).then_some(candidate_index))
            .collect::<Vec<_>>();
        let mut continuation_batch = LlamaBatch::new(
            backend.lib.clone(),
            (active_candidates.len() * max_continuations) as i32,
            0,
            sequence_count as i32,
        );
        let mut continuation_target_groups = vec![Vec::new(); gpu_samplers.states.len()];
        let mut output_candidates = Vec::new();
        for &candidate_index in &active_candidates {
            let tokens = &candidates[candidate_index];
            for token_index in 0..max_continuations {
                let is_real = token_index + 1 < tokens.len();
                let token = if token_index < tokens.len() {
                    tokens[token_index]
                } else {
                    model.get_vocab().eos()
                };
                continuation_batch.add(
                    token,
                    (decode_base.len() + token_index) as i32,
                    &[sequence_ids[candidate_index]],
                    is_real,
                );
                if is_real {
                    continuation_target_groups[candidate_index].push(vec![tokens[token_index + 1]]);
                    output_candidates.push(candidate_index);
                }
            }
        }
        configure_gpu_samplers(gpu_samplers, continuation_target_groups)?;
        let decode_started = Instant::now();
        context
            .decode(&continuation_batch)
            .map_err(|error| format!("llama.cpp decode failed: {error}"))?;
        timings.add_decode(decode_started);
        unsafe { (backend.lib.symbols.llama_synchronize)(context.handle) };
        let mut row_offsets = vec![0_usize; candidates.len()];
        for candidate_index in output_candidates {
            let state = unsafe { &*gpu_samplers.states[candidate_index] };
            let row_index = row_offsets[candidate_index];
            row_offsets[candidate_index] += 1;
            let values = read_gpu_sampler_row(state, row_index)?;
            token_logprobs[candidate_index].push(values[0]);
        }
    }
    if let Some((candidate_index, (lp, tokens))) = token_logprobs
        .iter()
        .zip(candidates)
        .enumerate()
        .find(|(_, (lp, tokens))| {
            lp.len() != tokens.len() || lp.iter().any(|value| !value.is_finite())
        })
    {
        let sampler_state = gpu_samplers
            .states
            .get(candidate_index)
            .map(|state| unsafe { &**state });
        return Err(format!(
            "llama.cpp GPU sampler returned incomplete logprob rows for candidate {candidate_index}: got {} values, expected {}; sampler results={}, target rows={}, current groups={}",
            lp.len(),
            tokens.len(),
            sampler_state.map_or(0, |state| state.results.len()),
            sampler_state.map_or(0, |state| state.target_tensors.len()),
            sampler_state.map_or(0, |state| state.groups.len()),
        ));
    }
    Ok(token_logprobs
        .into_iter()
        .zip(candidates)
        .map(|(logprobs, tokens)| CandidateScore {
            token_ids: tokens.clone(),
            logprob: logprobs.iter().sum(),
            token_logprobs: logprobs,
            mismatch: false,
        })
        .collect())
}

#[cfg(test)]
pub(super) fn log_normalizer(logits_pointer: &[f32]) -> Option<f64> {
    let maximum = logits_pointer
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max) as f64;
    if !maximum.is_finite() {
        return None;
    }
    let denominator = maximum
        + logits_pointer
            .iter()
            .map(|value| (*value as f64 - maximum).exp())
            .sum::<f64>()
            .ln();
    if !denominator.is_finite() {
        None
    } else {
        Some(denominator)
    }
}

#[cfg(test)]
pub(super) fn logprob_with_normalizer(
    logits_pointer: &[f32],
    token: llama_token,
    normalizer: f64,
) -> f64 {
    if token < 0 || logits_pointer.len() <= token as usize {
        return f64::NEG_INFINITY;
    }
    let value = logits_pointer[token as usize] as f64;
    if !value.is_finite() {
        f64::NEG_INFINITY
    } else {
        value - normalizer
    }
}

#[cfg(test)]
pub(super) fn logprob_for(logits_pointer: &[f32], token: llama_token) -> f64 {
    // `logits_for` is expanded to the full vocabulary below; this helper is kept separate so the
    // numerically stable log-sum-exp implementation is easy to test.
    log_normalizer(logits_pointer)
        .map(|normalizer| logprob_with_normalizer(logits_pointer, token, normalizer))
        .unwrap_or(f64::NEG_INFINITY)
}
