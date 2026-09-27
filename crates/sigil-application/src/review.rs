//! User-authored review comments bind to immutable recorded changes, never to write authority.

use serde::{Deserialize, Serialize};

use crate::SafeText;

/// Side of the recorded unified diff whose line numbers the user selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDiffSide {
    Old,
    New,
}

/// One comment on an exact recorded change. The host resolves all source fields afresh.
/// Current workspace drift is advisory; this reference never authorizes a file mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewAnnotation {
    pub checkpoint_id: String,
    pub checkpoint_digest: String,
    pub source_call_id: String,
    pub diff_digest: String,
    pub path: String,
    pub side: ReviewDiffSide,
    pub start_line: u32,
    pub end_line: u32,
    pub comment: SafeText,
}

/// Advisory comparison with the recorded change's committed file bytes; never write permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewFileState {
    Current,
    Changed,
    Unknown,
}

/// One line of a recorded unified diff. Only present numbered sides are valid comment targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewDiffLine {
    pub text: String,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
}

/// Bounded immutable forward diff with fresh advisory file state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationReviewDiff {
    pub source_call_id: String,
    pub diff_digest: String,
    pub path: String,
    pub lines: Vec<ReviewDiffLine>,
    pub file_state: ReviewFileState,
    pub truncated: bool,
}

/// Review of one exact checkpoint. Restore availability and unrelated file drift are not gates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationCheckpointReview {
    pub checkpoint_id: String,
    pub checkpoint_digest: String,
    pub diffs: Vec<ApplicationReviewDiff>,
    pub truncated: bool,
}
