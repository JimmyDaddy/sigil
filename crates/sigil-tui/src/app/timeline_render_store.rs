use std::ops::Range;

use ratatui::text::Line;

use super::{
    TimelineEntry,
    formatting::{hash_timeline_line, line_has_visible_content, plain_line_text},
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TimelineRenderGlobalKey {
    max_content_width: usize,
    expand_tool_previews: bool,
    expand_thinking_blocks: bool,
    theme: crate::ui::theme::Theme,
}

impl TimelineRenderGlobalKey {
    fn from_options(options: &crate::ui::TimelineRenderOptions) -> Self {
        Self {
            max_content_width: options.max_content_width,
            expand_tool_previews: options.expand_tool_previews,
            expand_thinking_blocks: options.expand_thinking_blocks,
            theme: options.theme.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum TimelineBlockKind {
    #[default]
    Entry,
    EntryWithTrailingSeparator,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RenderedTimelineBlock {
    entry_index: usize,
    lines: Vec<Line<'static>>,
    plain_lines: Vec<String>,
    kind: TimelineBlockKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppendOutcome {
    Appended,
    Rebuilt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RerenderOutcome {
    Rerendered,
    Rebuilt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TimelineRenderInvariantError {
    PrefixLineCountsLen {
        expected: usize,
        actual: usize,
    },
    PrefixLineCountsFirst {
        actual: usize,
    },
    PrefixLineCountsNotMonotonic {
        index: usize,
        previous: usize,
        current: usize,
    },
    PrefixLineCountsTotal {
        expected: usize,
        actual: usize,
    },
    BlockLineCountMismatch {
        entry_index: usize,
        lines: usize,
        plain_lines: usize,
    },
    PrefixHashesLen {
        expected: usize,
        actual: usize,
    },
    BlockEntryIndexMismatch {
        block_index: usize,
        entry_index: usize,
    },
    EffectiveLineCountExceedsBlock {
        entry_index: usize,
        effective_lines: usize,
        block_lines: usize,
    },
    LastVisibleBlockMismatch {
        expected: Option<usize>,
        actual: Option<usize>,
    },
}

#[derive(Debug, Clone, Default)]
pub(crate) struct TimelineRenderStore {
    global_key: TimelineRenderGlobalKey,
    blocks: Vec<RenderedTimelineBlock>,
    prefix_line_counts: Vec<usize>,
    prefix_hashes: Vec<u64>,
    last_visible_block_index: Option<usize>,
    revision: u64,
}

impl TimelineRenderStore {
    pub(crate) fn rebuild(
        &mut self,
        timeline: &[TimelineEntry],
        options: &crate::ui::TimelineRenderOptions,
    ) {
        self.global_key = TimelineRenderGlobalKey::from_options(options);
        self.blocks = timeline
            .iter()
            .enumerate()
            .map(|(index, entry)| render_block(entry, options, index))
            .collect();
        self.repair_separators();
        self.last_visible_block_index = self.find_last_visible_block_index();
        self.rebuild_indexes();
        self.revision = self.revision.saturating_add(1);
        debug_assert!(self.validate_invariants().is_ok());
    }

    pub(crate) fn append_entry(
        &mut self,
        timeline: &[TimelineEntry],
        index: usize,
        options: &crate::ui::TimelineRenderOptions,
    ) -> AppendOutcome {
        if index != self.blocks.len()
            || timeline.len() != self.blocks.len().saturating_add(1)
            || index >= timeline.len()
            || !self.global_key_matches(options)
        {
            self.rebuild(timeline, options);
            return AppendOutcome::Rebuilt;
        }
        let block = render_block(&timeline[index], options, index);
        let block_is_visible = block_is_visible(&block);
        let previous_last_visible = self.last_visible_block_index;
        if block_is_visible && let Some(previous_index) = previous_last_visible {
            self.restore_separator_on_block(previous_index);
        }
        self.blocks.push(block);
        if block_is_visible {
            self.last_visible_block_index = Some(index);
            self.rebuild_indexes_from(previous_last_visible.unwrap_or(0));
        } else {
            self.prefix_line_counts.push(self.total_lines());
        }
        self.revision = self.revision.saturating_add(1);
        debug_assert!(self.validate_invariants().is_ok());
        AppendOutcome::Appended
    }

    pub(crate) fn rerender_entry(
        &mut self,
        timeline: &[TimelineEntry],
        index: usize,
        options: &crate::ui::TimelineRenderOptions,
    ) -> RerenderOutcome {
        let Some(entry) = timeline.get(index) else {
            self.rebuild(timeline, options);
            return RerenderOutcome::Rebuilt;
        };
        if self.blocks.get(index).is_none()
            || timeline.len() != self.blocks.len()
            || index.saturating_add(1) != self.blocks.len()
            || !self.global_key_matches(options)
        {
            self.rebuild(timeline, options);
            return RerenderOutcome::Rebuilt;
        }
        let next_block = render_block(entry, options, index);
        let was_visible = self.last_visible_block_index == Some(index);
        let is_visible = block_is_visible(&next_block);
        let previous_visible = if was_visible {
            self.blocks[..index].iter().rposition(block_is_visible)
        } else {
            self.last_visible_block_index
        };
        self.blocks[index] = next_block;
        match (was_visible, is_visible) {
            (true, true) => self.rebuild_indexes_from(index),
            (false, false) => {}
            (false, true) => {
                if let Some(previous_index) = previous_visible {
                    self.restore_separator_on_block(previous_index);
                }
                self.last_visible_block_index = Some(index);
                self.rebuild_indexes_from(previous_visible.unwrap_or(0));
            }
            (true, false) => {
                self.last_visible_block_index = previous_visible;
                self.rebuild_indexes_from(previous_visible.unwrap_or(0));
            }
        }
        self.revision = self.revision.saturating_add(1);
        debug_assert!(self.validate_invariants().is_ok());
        RerenderOutcome::Rerendered
    }

    pub(crate) fn snapshot(&self) -> TimelineRenderSnapshot<'_> {
        TimelineRenderSnapshot {
            blocks: &self.blocks,
            prefix_line_counts: &self.prefix_line_counts,
            prefix_hashes: &self.prefix_hashes,
            total_lines: self.total_lines(),
            revision: self.revision,
        }
    }

    pub(crate) fn validate_invariants(&self) -> Result<(), TimelineRenderInvariantError> {
        let actual_last_visible = self.find_last_visible_block_index();
        if self.last_visible_block_index != actual_last_visible {
            return Err(TimelineRenderInvariantError::LastVisibleBlockMismatch {
                expected: actual_last_visible,
                actual: self.last_visible_block_index,
            });
        }
        let expected_prefix_len = self.blocks.len().saturating_add(1);
        if self.prefix_line_counts.len() != expected_prefix_len {
            return Err(TimelineRenderInvariantError::PrefixLineCountsLen {
                expected: expected_prefix_len,
                actual: self.prefix_line_counts.len(),
            });
        }
        if self
            .prefix_line_counts
            .first()
            .copied()
            .is_some_and(|actual| actual != 0)
        {
            return Err(TimelineRenderInvariantError::PrefixLineCountsFirst {
                actual: self.prefix_line_counts[0],
            });
        }
        for (index, pair) in self.prefix_line_counts.windows(2).enumerate() {
            if pair[1] < pair[0] {
                return Err(TimelineRenderInvariantError::PrefixLineCountsNotMonotonic {
                    index,
                    previous: pair[0],
                    current: pair[1],
                });
            }
        }
        let effective_line_counts = self.effective_block_line_counts();
        let expected_total = effective_line_counts.iter().sum();
        if self.prefix_line_counts.last().copied().unwrap_or(0) != expected_total {
            return Err(TimelineRenderInvariantError::PrefixLineCountsTotal {
                expected: expected_total,
                actual: self.prefix_line_counts.last().copied().unwrap_or(0),
            });
        }
        for (block_index, block) in self.blocks.iter().enumerate() {
            if block.entry_index != block_index {
                return Err(TimelineRenderInvariantError::BlockEntryIndexMismatch {
                    block_index,
                    entry_index: block.entry_index,
                });
            }
            if block.lines.len() != block.plain_lines.len() {
                return Err(TimelineRenderInvariantError::BlockLineCountMismatch {
                    entry_index: block.entry_index,
                    lines: block.lines.len(),
                    plain_lines: block.plain_lines.len(),
                });
            }
            let effective_lines = effective_line_counts.get(block_index).copied().unwrap_or(0);
            if effective_lines > block.lines.len() {
                return Err(
                    TimelineRenderInvariantError::EffectiveLineCountExceedsBlock {
                        entry_index: block.entry_index,
                        effective_lines,
                        block_lines: block.lines.len(),
                    },
                );
            }
        }
        if self.prefix_hashes.len() != expected_total {
            return Err(TimelineRenderInvariantError::PrefixHashesLen {
                expected: expected_total,
                actual: self.prefix_hashes.len(),
            });
        }
        Ok(())
    }

    fn total_lines(&self) -> usize {
        self.prefix_line_counts.last().copied().unwrap_or(0)
    }

    fn rebuild_indexes(&mut self) {
        self.prefix_line_counts.clear();
        self.prefix_line_counts.push(0);
        self.prefix_hashes.clear();
        self.rebuild_indexes_from(0);
    }

    fn rebuild_indexes_from(&mut self, start_block_index: usize) {
        let start_block_index = start_block_index.min(self.blocks.len());
        let base_line_count = self
            .prefix_line_counts
            .get(start_block_index)
            .copied()
            .unwrap_or(0);
        self.prefix_line_counts.truncate(start_block_index + 1);
        self.prefix_hashes.truncate(base_line_count);
        let mut line_count = base_line_count;
        let mut hash = self.prefix_hashes.last().copied().unwrap_or(0);
        for (index, block) in self.blocks.iter().enumerate().skip(start_block_index) {
            let effective_lines = self.effective_block_line_count(index);
            line_count = line_count.saturating_add(effective_lines);
            self.prefix_line_counts.push(line_count);
            for plain in block.plain_lines.iter().take(effective_lines) {
                hash = hash_timeline_line(hash, plain);
                self.prefix_hashes.push(hash);
            }
        }
    }

    fn effective_block_line_counts(&self) -> Vec<usize> {
        self.blocks
            .iter()
            .enumerate()
            .map(|(index, _)| self.effective_block_line_count(index))
            .collect()
    }

    fn effective_block_line_count(&self, index: usize) -> usize {
        let Some(last_visible_index) = self.last_visible_block_index else {
            return 0;
        };
        let Some(block) = self.blocks.get(index) else {
            return 0;
        };
        if index < last_visible_index {
            block.lines.len()
        } else if index == last_visible_index {
            tail_trimmed_line_count(block)
        } else {
            0
        }
    }

    fn find_last_visible_block_index(&self) -> Option<usize> {
        self.blocks.iter().rposition(block_is_visible)
    }

    fn global_key_matches(&self, options: &crate::ui::TimelineRenderOptions) -> bool {
        self.global_key == TimelineRenderGlobalKey::from_options(options)
    }

    fn restore_separator_on_block(&mut self, index: usize) {
        let Some(block) = self.blocks.get_mut(index) else {
            return;
        };
        if block
            .lines
            .last()
            .is_some_and(|line| !line_has_visible_content(line))
        {
            block.kind = TimelineBlockKind::EntryWithTrailingSeparator;
            return;
        }
        push_separator(block);
    }

    fn repair_separators(&mut self) {
        let mut has_later_visible_block = false;
        for block in self.blocks.iter_mut().rev() {
            let visible = block.lines.iter().any(line_has_visible_content);
            if visible
                && has_later_visible_block
                && block.lines.last().is_none_or(line_has_visible_content)
            {
                push_separator(block);
            }
            if visible {
                has_later_visible_block = true;
            }
            block.kind = block_kind_for_lines(&block.lines);
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TimelineRenderSnapshot<'a> {
    blocks: &'a [RenderedTimelineBlock],
    prefix_line_counts: &'a [usize],
    prefix_hashes: &'a [u64],
    total_lines: usize,
    revision: u64,
}

impl<'a> TimelineRenderSnapshot<'a> {
    pub(crate) fn total_lines(self) -> usize {
        self.total_lines
    }

    pub(crate) fn revision(self) -> u64 {
        self.revision
    }

    pub(crate) fn range_for_entry(self, entry_index: usize) -> Option<Range<usize>> {
        let start = *self.prefix_line_counts.get(entry_index)?;
        let end = *self.prefix_line_counts.get(entry_index.saturating_add(1))?;
        Some(start..end.min(self.total_lines))
    }

    pub(crate) fn entry_at_line(self, line_index: usize) -> Option<usize> {
        if line_index >= self.total_lines {
            return None;
        }
        let partition = self
            .prefix_line_counts
            .partition_point(|count| *count <= line_index);
        partition.checked_sub(1)
    }

    pub(crate) fn lines_range(self, range: Range<usize>) -> Vec<Line<'static>> {
        let range = clamp_range(range, self.total_lines);
        self.iter_lines()
            .skip(range.start)
            .take(range.end.saturating_sub(range.start))
            .cloned()
            .collect()
    }

    pub(crate) fn plain_lines_range(self, range: Range<usize>) -> Vec<String> {
        let range = clamp_range(range, self.total_lines);
        self.iter_plain_lines()
            .skip(range.start)
            .take(range.end.saturating_sub(range.start))
            .cloned()
            .collect()
    }

    pub(crate) fn plain_line(self, line_index: usize) -> Option<&'a str> {
        self.entry_at_line(line_index)?;
        self.iter_plain_lines().nth(line_index).map(String::as_str)
    }

    pub(crate) fn prefix_hashes(self) -> &'a [u64] {
        self.prefix_hashes
    }

    fn iter_lines(self) -> impl Iterator<Item = &'a Line<'static>> {
        self.blocks
            .iter()
            .enumerate()
            .flat_map(move |(index, block)| {
                block.lines.iter().take(self.effective_block_len(index))
            })
    }

    fn iter_plain_lines(self) -> impl Iterator<Item = &'a String> {
        self.blocks
            .iter()
            .enumerate()
            .flat_map(move |(index, block)| {
                block
                    .plain_lines
                    .iter()
                    .take(self.effective_block_len(index))
            })
    }

    fn effective_block_len(self, index: usize) -> usize {
        let Some(start) = self.prefix_line_counts.get(index).copied() else {
            return 0;
        };
        let Some(end) = self
            .prefix_line_counts
            .get(index.saturating_add(1))
            .copied()
        else {
            return 0;
        };
        end.saturating_sub(start)
    }
}

fn render_block(
    entry: &TimelineEntry,
    options: &crate::ui::TimelineRenderOptions,
    entry_index: usize,
) -> RenderedTimelineBlock {
    let lines = crate::ui::render_timeline_entry_lines_with_options(entry, options, entry_index);
    let plain_lines = lines.iter().map(plain_line_text).collect::<Vec<_>>();
    RenderedTimelineBlock {
        entry_index,
        kind: block_kind_for_lines(&lines),
        lines,
        plain_lines,
    }
}

fn push_separator(block: &mut RenderedTimelineBlock) {
    let line = Line::raw(String::new());
    let plain = plain_line_text(&line);
    block.lines.push(line);
    block.plain_lines.push(plain);
    block.kind = TimelineBlockKind::EntryWithTrailingSeparator;
}

fn block_is_visible(block: &RenderedTimelineBlock) -> bool {
    block.lines.iter().any(line_has_visible_content)
}

fn block_kind_for_lines(lines: &[Line<'_>]) -> TimelineBlockKind {
    if lines
        .last()
        .is_some_and(|line| !line_has_visible_content(line))
    {
        TimelineBlockKind::EntryWithTrailingSeparator
    } else {
        TimelineBlockKind::Entry
    }
}

fn clamp_range(range: Range<usize>, total_lines: usize) -> Range<usize> {
    let start = range.start.min(total_lines);
    let end = range.end.min(total_lines).max(start);
    start..end
}

fn tail_trimmed_line_count(block: &RenderedTimelineBlock) -> usize {
    block
        .lines
        .iter()
        .rposition(line_has_visible_content)
        .map(|index| index + 1)
        .unwrap_or(0)
}
