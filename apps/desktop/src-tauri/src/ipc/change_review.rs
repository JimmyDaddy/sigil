use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopReviewAnnotationInput {
    checkpoint_id: String,
    checkpoint_digest: String,
    source_call_id: String,
    diff_digest: String,
    path: String,
    side: sigil_desktop::DesktopReviewDiffSide,
    start_line: u32,
    end_line: u32,
    comment: sigil_desktop::DesktopReviewComment,
}

impl From<DesktopReviewAnnotationInput> for sigil_desktop::DesktopReviewAnnotation {
    fn from(value: DesktopReviewAnnotationInput) -> Self {
        Self {
            checkpoint_id: value.checkpoint_id,
            checkpoint_digest: value.checkpoint_digest,
            source_call_id: value.source_call_id,
            diff_digest: value.diff_digest,
            path: value.path,
            side: value.side,
            start_line: value.start_line,
            end_line: value.end_line,
            comment: value.comment,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopCheckpointReview {
    checkpoint_id: String,
    checkpoint_digest: String,
    diffs: Vec<DesktopReviewDiff>,
    truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopReviewDiff {
    source_call_id: String,
    diff_digest: String,
    path: String,
    lines: Vec<DesktopReviewDiffLine>,
    file_state: sigil_desktop::DesktopReviewFileState,
    truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopReviewDiffLine {
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    old_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_line: Option<u32>,
}

impl From<sigil_desktop::DesktopCheckpointReview> for DesktopCheckpointReview {
    fn from(value: sigil_desktop::DesktopCheckpointReview) -> Self {
        Self {
            checkpoint_id: value.checkpoint_id,
            checkpoint_digest: value.checkpoint_digest,
            diffs: value
                .diffs
                .into_iter()
                .map(|diff| DesktopReviewDiff {
                    source_call_id: diff.source_call_id,
                    diff_digest: diff.diff_digest,
                    path: diff.path,
                    lines: diff
                        .lines
                        .into_iter()
                        .map(|line| DesktopReviewDiffLine {
                            text: line.text,
                            old_line: line.old_line,
                            new_line: line.new_line,
                        })
                        .collect(),
                    file_state: diff.file_state,
                    truncated: diff.truncated,
                })
                .collect(),
            truncated: value.truncated,
        }
    }
}
