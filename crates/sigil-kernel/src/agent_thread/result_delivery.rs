use serde::{Deserialize, Serialize};

use super::{AgentThreadId, AgentThreadResultDeliveredEntry};
use crate::{ControlEntry, SessionLogEntry};

/// Body-free coverage of result pages, independent of the order they were delivered.
///
/// Owners must scope receipts to one result identity and reset this accumulator when that
/// result is replaced. Persisted projections retain coverage; a new transient context starts
/// with empty coverage even if older audit receipts exist.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AgentResultDeliveryCoverage {
    ranges: Vec<(usize, usize)>,
    terminal_page: Option<AgentThreadResultDeliveredEntry>,
}

impl AgentResultDeliveryCoverage {
    /// Records a receipt already matched to this accumulator's result identity.
    pub fn record(&mut self, entry: &AgentThreadResultDeliveredEntry) {
        let mut start = entry.offset_chars;
        let mut end = start
            .saturating_add(entry.returned_chars)
            .min(entry.total_chars);
        if start < end {
            let first = self.ranges.partition_point(|(_, end)| *end < start);
            let mut last = first;
            while let Some(&(range_start, range_end)) = self.ranges.get(last) {
                if range_start > end {
                    break;
                }
                start = start.min(range_start);
                end = end.max(range_end);
                last += 1;
            }
            self.ranges.splice(first..last, [(start, end)]);
        }
        if !entry.truncated {
            self.terminal_page = Some(entry.clone());
        }
    }

    /// Returns the delivered prefix length, stopping at the first unread gap.
    #[must_use]
    pub fn contiguous_chars(&self) -> usize {
        self.ranges
            .first()
            .filter(|(start, _)| *start == 0)
            .map_or(0, |(_, end)| *end)
    }

    /// Returns the terminal-page receipt only after all characters have been delivered.
    #[must_use]
    pub fn fully_delivered_receipt(&self) -> Option<&AgentThreadResultDeliveredEntry> {
        self.terminal_page
            .as_ref()
            .filter(|page| self.contiguous_chars() >= page.total_chars)
    }

    /// Returns whether every character in a requested page is already present in the current
    /// transient context. Zero-character pages are matched to their exact terminal receipt.
    #[must_use]
    pub fn page_was_delivered(
        &self,
        offset_chars: usize,
        returned_chars: usize,
        total_chars: usize,
    ) -> bool {
        if returned_chars == 0 {
            return self.fully_delivered_receipt().is_some()
                || self.terminal_page.as_ref().is_some_and(|page| {
                    page.offset_chars == offset_chars
                        && page.returned_chars == 0
                        && page.total_chars == total_chars
                });
        }
        let end = offset_chars.saturating_add(returned_chars).min(total_chars);
        self.ranges
            .iter()
            .any(|(start, covered_end)| *start <= offset_chars && *covered_end >= end)
    }

    pub(crate) fn from_entries(
        entries: &[SessionLogEntry],
        thread_id: &AgentThreadId,
        output_hash: &str,
    ) -> Self {
        let mut coverage = Self::default();
        for entry in entries {
            match entry {
                SessionLogEntry::Control(ControlEntry::AgentThreadResultRecorded(recorded))
                    if recorded.result.thread_id == *thread_id =>
                {
                    coverage = Self::default();
                }
                SessionLogEntry::Control(ControlEntry::AgentThreadResultDelivered(delivered))
                    if delivered.thread_id == *thread_id
                        && delivered.output_hash == output_hash =>
                {
                    coverage.record(delivered);
                }
                _ => {}
            }
        }
        coverage
    }
}

#[cfg(test)]
#[path = "tests/result_delivery_tests.rs"]
mod tests;
