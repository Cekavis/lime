use std::cmp::Reverse;

use lime_protocol::{InputHistoryEntry, InputHistoryPage, INPUT_HISTORY_PAGE_SIZE};

pub(crate) fn snapshot(entries: &[InputHistoryEntry]) -> Vec<InputHistoryEntry> {
    let mut entries = entries.to_vec();
    // Newest first; request_id is never the sort key.
    entries.sort_by_key(|entry| Reverse(entry.timestamp_ms));
    entries
}

pub(crate) fn page(entries: &[InputHistoryEntry], page: u32, page_size: u32) -> InputHistoryPage {
    let ordered = snapshot(entries);
    let total = ordered.len() as u64;
    let page = page.max(1);
    let page_size = if page_size == 0 {
        INPUT_HISTORY_PAGE_SIZE
    } else {
        page_size.clamp(1, INPUT_HISTORY_PAGE_SIZE)
    };
    let start = (u64::from(page - 1) * u64::from(page_size)) as usize;
    InputHistoryPage {
        items: ordered
            .into_iter()
            .skip(start)
            .take(page_size as usize)
            .collect(),
        total,
        page,
        page_size,
    }
}

use super::*;

impl CoreService {
    pub(super) fn history_page(&self, page: u32, page_size: u32) -> InputHistoryPage {
        history::page(
            &self.history.lock().expect("history mutex poisoned"),
            page,
            page_size,
        )
    }

    pub(super) fn dictionary_page(
        &self,
        page: u32,
        page_size: u32,
    ) -> Result<DictionaryPage, crate::error::CoreError> {
        let entries = self
            .engine
            .lock()
            .expect("engine mutex poisoned")
            .export_dictionary()?;
        let total = entries.len() as u64;
        let page = page.max(1);
        let page_size = if page_size == 0 {
            DICTIONARY_PAGE_SIZE
        } else {
            page_size.clamp(1, DICTIONARY_PAGE_SIZE)
        };
        let start = (u64::from(page - 1) * u64::from(page_size)) as usize;
        let items = entries
            .into_iter()
            .skip(start)
            .take(page_size as usize)
            .collect();
        Ok(DictionaryPage {
            items,
            total,
            page,
            page_size,
        })
    }

    /// Return the current history revision, waiting until it changes from the
    /// caller's last observed value.  The revision is separate from the
    /// entries so a client can wait without transferring input content.
    pub(super) fn wait_for_history_revision(&self, revision: u64) -> u64 {
        let (lock, changed) = &*self.history_revision;
        let current = lock.lock().expect("history revision mutex poisoned");
        let (current, _) = changed
            .wait_timeout_while(current, Duration::from_secs(30), |value| *value == revision)
            .expect("history revision mutex poisoned");
        *current
    }

    pub(super) fn bump_history_revision(&self) {
        let (lock, changed) = &*self.history_revision;
        let mut revision = lock.lock().expect("history revision mutex poisoned");
        *revision = revision.saturating_add(1);
        changed.notify_all();
    }

    pub(super) fn record_input_history(&self, record: InputHistoryRecord<'_>) {
        let InputHistoryRecord {
            request,
            rime_candidates,
            final_candidates,
            diagnostics,
            service_state,
            model_name,
            rime_duration_ms,
            llm_performance,
            end_to_end_duration_ms,
        } = record;
        let timestamp_ms = self.next_timestamp_ms();
        self.history
            .lock()
            .expect("history mutex poisoned")
            .push(InputHistoryEntry {
                request_id: request.request_id,
                timestamp_ms,
                end_to_end_duration_ms,
                preceding_text: request.preceding_text.clone(),
                preedit: request.preedit.clone(),
                rime_candidates,
                final_candidates,
                service_state,
                model_name,
                rime_duration_ms,
                diagnostics,
                llm_performance,
            });
        self.bump_history_revision();
    }

    pub(super) fn append_candidate_history(
        &self,
        original_request_id: u64,
        request: &InputRequest,
        candidates: &[lime_protocol::Candidate],
    ) {
        let changed = {
            let mut history = self.history.lock().expect("history mutex poisoned");
            let Some(entry) = history.iter_mut().rev().find(|entry| {
                entry.request_id == original_request_id
                    && entry.preedit == request.preedit
                    && entry.preceding_text == request.preceding_text
            }) else {
                return;
            };
            let rime_start = entry.rime_candidates.len();
            if candidates.len() <= rime_start {
                false
            } else {
                entry
                    .rime_candidates
                    .extend(candidates[rime_start..].iter().cloned());

                let final_start = entry.final_candidates.len().min(candidates.len());
                entry
                    .final_candidates
                    .extend(candidates[final_start..].iter().cloned());

                let diagnostic_start = entry.diagnostics.len().min(candidates.len());
                entry
                    .diagnostics
                    .extend(candidates[diagnostic_start..].iter().enumerate().map(
                        |(offset, candidate)| CandidateDiagnostic {
                            rank: (diagnostic_start + offset + 1) as u32,
                            rime_candidate: Some(candidate.clone()),
                            llm_candidate: None,
                            logprob: 0.0,
                            logprobs: Vec::new(),
                            mismatch: false,
                            display_candidate: Some(candidate.clone()),
                        },
                    ));
                true
            }
        };
        if changed {
            self.bump_history_revision();
        }
    }

    pub(super) fn next_timestamp_ms(&self) -> u64 {
        let now = now_unix_ms();
        let mut previous = self.history_clock.load(Ordering::Acquire);
        loop {
            let next = now.max(previous.saturating_add(1));
            match self.history_clock.compare_exchange(
                previous,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return next,
                Err(observed) => previous = observed,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(timestamp_ms: u64) -> InputHistoryEntry {
        InputHistoryEntry {
            request_id: timestamp_ms,
            timestamp_ms,
            end_to_end_duration_ms: None,
            preceding_text: String::new(),
            preedit: String::new(),
            rime_candidates: Vec::new(),
            final_candidates: Vec::new(),
            service_state: lime_protocol::ServiceState::RimeOnly,
            model_name: None,
            rime_duration_ms: None,
            diagnostics: Vec::new(),
            llm_performance: None,
        }
    }

    #[test]
    fn sorts_newest_first_and_bounds_page_size() {
        let entries = vec![entry(1), entry(3), entry(2)];
        let result = page(&entries, 1, 0);
        assert_eq!(result.items[0].timestamp_ms, 3);
        assert_eq!(result.page_size, INPUT_HISTORY_PAGE_SIZE);
    }
}
