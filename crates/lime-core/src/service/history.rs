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
