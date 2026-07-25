use super::normalize::{
    MarkdownRepairDiagnostic, is_closing_fence, normalize_completed_markdown,
    normalize_streaming_markdown, opening_fence,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MarkdownPhase {
    Streaming,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProjectedMarkdownBlockKind {
    Markdown,
    Code,
    Mermaid,
}

impl ProjectedMarkdownBlockKind {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Code => "code",
            Self::Mermaid => "mermaid",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProjectedMarkdownStability {
    Stable,
    Live,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProjectedMarkdownBlock {
    pub(super) key: String,
    pub(super) source: String,
    pub(super) source_start: usize,
    pub(super) source_end: usize,
    pub(super) stability: ProjectedMarkdownStability,
    pub(super) kind: ProjectedMarkdownBlockKind,
    pub(super) synthetic_closing_fence: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MarkdownProjection {
    pub(super) mode: MarkdownPhase,
    pub(super) source_length: usize,
    pub(super) source: String,
    pub(super) blocks: Vec<ProjectedMarkdownBlock>,
    pub(super) diagnostics: Vec<MarkdownRepairDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MarkdownProjectionCursor {
    content_id: String,
    phase: MarkdownPhase,
    source: String,
    projection: MarkdownProjection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MarkdownProjectionUpdate {
    pub(super) projection: MarkdownProjection,
    pub(super) cursor: MarkdownProjectionCursor,
    pub(super) reused_stable_blocks: usize,
}

#[derive(Debug, Clone, Copy)]
struct SourceLine<'a> {
    text: &'a str,
    start: usize,
    end: usize,
}

#[derive(Debug, Clone, Copy)]
struct BlockSlice {
    start: usize,
    end: usize,
    kind: ProjectedMarkdownBlockKind,
    closed: bool,
}

pub(super) fn project_markdown(
    source: &str,
    phase: MarkdownPhase,
    content_id: &str,
) -> MarkdownProjection {
    project_markdown_with_cursor(source, phase, content_id, None).projection
}

pub(super) fn project_markdown_with_cursor(
    source: &str,
    phase: MarkdownPhase,
    content_id: &str,
    previous: Option<&MarkdownProjectionCursor>,
) -> MarkdownProjectionUpdate {
    let reusable_prefix = reusable_stable_prefix(previous, source, phase, content_id);
    let normalized = match phase {
        MarkdownPhase::Streaming => normalize_streaming_markdown(source),
        MarkdownPhase::Complete => normalize_completed_markdown(source),
    };
    let scan_start = reusable_prefix
        .last()
        .map(|block| block.source_end)
        .unwrap_or(0);
    let reused_stable_blocks = reusable_prefix.len();
    let slices = scan_blocks(&normalized.source[scan_start..])
        .into_iter()
        .map(|block| BlockSlice {
            start: block.start.saturating_add(scan_start),
            end: block.end.saturating_add(scan_start),
            ..block
        })
        .collect::<Vec<_>>();
    let final_index = reused_stable_blocks
        .saturating_add(slices.len())
        .saturating_sub(1);
    let mut blocks = reusable_prefix;
    blocks.extend(
        slices
            .into_iter()
            .enumerate()
            .map(|(relative_index, block)| {
                let index = reused_stable_blocks.saturating_add(relative_index);
                let stability = match phase {
                    MarkdownPhase::Complete => ProjectedMarkdownStability::Stable,
                    MarkdownPhase::Streaming
                        if block.closed
                            && (index < final_index
                                || ends_at_safe_boundary(&normalized.source, block.end)) =>
                    {
                        ProjectedMarkdownStability::Stable
                    }
                    MarkdownPhase::Streaming => ProjectedMarkdownStability::Live,
                };
                let synthetic_closing_fence = normalized.diagnostics.iter().any(|diagnostic| {
                    diagnostic.source_start >= block.start && diagnostic.source_start <= block.end
                });
                ProjectedMarkdownBlock {
                    key: format!("{}:{}:{}", content_id, block.start, block.kind.label()),
                    source: normalized.source[block.start..block.end].to_owned(),
                    source_start: block.start,
                    source_end: block.end,
                    stability,
                    kind: block.kind,
                    synthetic_closing_fence,
                }
            }),
    );
    let projection = MarkdownProjection {
        mode: phase,
        source_length: source.len(),
        source: normalized.source,
        blocks,
        diagnostics: normalized.diagnostics,
    };
    MarkdownProjectionUpdate {
        reused_stable_blocks,
        cursor: MarkdownProjectionCursor {
            content_id: content_id.to_owned(),
            phase,
            source: source.to_owned(),
            projection: projection.clone(),
        },
        projection,
    }
}

fn reusable_stable_prefix(
    previous: Option<&MarkdownProjectionCursor>,
    source: &str,
    phase: MarkdownPhase,
    content_id: &str,
) -> Vec<ProjectedMarkdownBlock> {
    let Some(previous) = previous else {
        return Vec::new();
    };
    if phase != MarkdownPhase::Streaming
        || previous.phase != MarkdownPhase::Streaming
        || previous.content_id != content_id
        || !source.starts_with(&previous.source)
    {
        return Vec::new();
    }
    let stable = previous
        .projection
        .blocks
        .iter()
        .take_while(|block| block.stability == ProjectedMarkdownStability::Stable)
        .cloned()
        .collect::<Vec<_>>();
    let boundary = stable.last().map(|block| block.source_end).unwrap_or(0);
    if source.get(..boundary) != previous.projection.source.get(..boundary) {
        return Vec::new();
    }
    stable
}

fn scan_blocks(source: &str) -> Vec<BlockSlice> {
    if source.is_empty() {
        return Vec::new();
    }
    let lines = source_lines(source);
    let mut blocks = Vec::new();
    let mut markdown_start = None;
    let mut index = 0usize;

    while index < lines.len() {
        let line = lines[index];
        let Some(fence) = opening_fence(line.text) else {
            if markdown_start.is_none() {
                markdown_start = Some(line.start);
            }
            if line.text.trim().is_empty() {
                flush_markdown(&mut blocks, source, &mut markdown_start, line.end, true);
            }
            index += 1;
            continue;
        };

        flush_markdown(&mut blocks, source, &mut markdown_start, line.start, true);
        let kind = if fence_info(line.text).eq_ignore_ascii_case("mermaid") {
            ProjectedMarkdownBlockKind::Mermaid
        } else {
            ProjectedMarkdownBlockKind::Code
        };
        let mut end = line.end;
        let mut closed = false;
        index += 1;
        while index < lines.len() {
            end = lines[index].end;
            if is_closing_fence(lines[index].text, fence) {
                closed = true;
                index += 1;
                break;
            }
            index += 1;
        }
        blocks.push(BlockSlice {
            start: line.start,
            end,
            kind,
            closed,
        });
    }

    flush_markdown(
        &mut blocks,
        source,
        &mut markdown_start,
        source.len(),
        false,
    );
    blocks
}

fn flush_markdown(
    blocks: &mut Vec<BlockSlice>,
    source: &str,
    start: &mut Option<usize>,
    end: usize,
    closed: bool,
) {
    let Some(block_start) = start.take() else {
        return;
    };
    if end > block_start && !source[block_start..end].trim().is_empty() {
        blocks.push(BlockSlice {
            start: block_start,
            end,
            kind: ProjectedMarkdownBlockKind::Markdown,
            closed,
        });
    }
}

fn source_lines(source: &str) -> Vec<SourceLine<'_>> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    for part in source.split_inclusive('\n') {
        let text = part.strip_suffix('\n').unwrap_or(part);
        let end = start.saturating_add(part.len());
        lines.push(SourceLine { text, start, end });
        start = end;
    }
    if source.ends_with('\n') {
        lines.push(SourceLine {
            text: "",
            start,
            end: start,
        });
    }
    lines
}

fn fence_info(line: &str) -> &str {
    let trimmed = line.trim_start();
    let Some(marker) = trimmed.chars().next() else {
        return "";
    };
    let marker_len = trimmed.chars().take_while(|value| *value == marker).count();
    let marker_bytes = marker.len_utf8().saturating_mul(marker_len);
    trimmed[marker_bytes..]
        .split_whitespace()
        .next()
        .unwrap_or("")
}

fn ends_at_safe_boundary(source: &str, end: usize) -> bool {
    if end < source.len() {
        return true;
    }
    let tail = &source[..end];
    tail.ends_with("\n\n")
        || tail
            .lines()
            .next_back()
            .is_some_and(|line| opening_fence(line).is_some() && line.trim().len() >= 3)
}
