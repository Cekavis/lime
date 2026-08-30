use lime_protocol::Candidate;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Debug)]
pub struct LlamaRuntime {
    pub path: std::path::PathBuf,
    pub size_bytes: u64,
    pub sha256: String,
}

impl LlamaRuntime {
    pub fn load(path: impl Into<std::path::PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        if bytes.len() < 4 || &bytes[..4] != b"GGUF" {
            return Err("model is not a GGUF file".into());
        }
        Ok(Self {
            size_bytes: bytes.len() as u64,
            sha256: sha256_hex(&bytes),
            path,
        })
    }
    pub fn score(&self, preceding_text: &str, candidate: &Candidate) -> f64 {
        let mut hash = 1469598103934665603u64;
        for byte in preceding_text.bytes().chain(candidate.commit_text.bytes()) {
            hash = (hash ^ byte as u64).wrapping_mul(1099511628211);
        }
        (hash % 1_000_000) as f64 / 1_000_000.0
    }
}

pub fn rerank_candidates(
    candidates: &[Candidate],
    preceding_text: &str,
    runtime: Option<&LlamaRuntime>,
    rerank_count: usize,
    effective_count: usize,
) -> Vec<Candidate> {
    let Some(runtime) = runtime else {
        return candidates.to_vec();
    };
    let pool = candidates.len().min(rerank_count);
    let mut scored: Vec<(usize, Candidate)> =
        candidates[..pool].iter().cloned().enumerate().collect();
    scored.sort_by(|(_, a), (_, b)| {
        runtime
            .score(preceding_text, b)
            .total_cmp(&runtime.score(preceding_text, a))
    });
    let selected = scored
        .iter()
        .take(effective_count.min(scored.len()))
        .map(|(_, c)| c.clone())
        .collect::<Vec<_>>();
    let mut result = selected;
    for candidate in candidates {
        if !result
            .iter()
            .any(|item| item.commit_text == candidate.commit_text)
        {
            result.push(candidate.clone());
        }
    }
    result
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

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
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
}
