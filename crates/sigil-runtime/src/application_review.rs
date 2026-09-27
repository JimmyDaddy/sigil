//! Read-only recorded-change review and validated user annotations; no file mutation authority.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    path::{Component, Path},
};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use sigil_application::{ReviewAnnotation, ReviewDiffSide};
use sigil_kernel::{
    ControlledCheckpointProjection, MutationSubject, SessionLogEntry, TypedDomainEvent,
};
use sigil_resource_authority::file_access::{
    HostSourceEntryObservation, HostSourceObservationRoot,
};

use crate::application_recovery::{committed_checkpoint_previews, read_bound_records};

const MAX_REVIEW_BYTES: usize = 256 * 1024;
const MAX_REVIEW_DIFFS: usize = 64;
const MAX_ANNOTATIONS: usize = 16;
const MAX_COMMENT_BYTES: usize = 4096;
const MAX_RANGE_LINES: u32 = 200;

pub use sigil_application::{
    ApplicationCheckpointReview, ApplicationReviewDiff, ReviewDiffLine, ReviewFileState,
};

/// Reads committed forward previews for one session/checkpoint, without restoring or writing.
///
/// # Errors
/// Rejects foreign scope, stale immutable checkpoint digest or mismatched workspace identity.
/// Missing/changed current files are reported as advisory states and never reject the review.
pub fn application_checkpoint_review(
    session_path: &Path,
    expected_session_scope_id: &str,
    workspace_root: &Path,
    checkpoint_id: &str,
    checkpoint_digest: &str,
) -> Result<ApplicationCheckpointReview> {
    checkpoint_review(
        session_path,
        expected_session_scope_id,
        workspace_root,
        checkpoint_id,
        checkpoint_digest,
        true,
    )
}

fn checkpoint_review(
    session_path: &Path,
    expected_session_scope_id: &str,
    workspace_root: &Path,
    checkpoint_id: &str,
    checkpoint_digest: &str,
    observe_current: bool,
) -> Result<ApplicationCheckpointReview> {
    let records = read_bound_records(session_path, expected_session_scope_id)?;
    let projection = ControlledCheckpointProjection::from_records(&records)?;
    let checkpoint = projection
        .checkpoints
        .iter()
        .find(|checkpoint| {
            checkpoint.checkpoint_id == checkpoint_id
                && checkpoint.checkpoint_digest == checkpoint_digest
        })
        .context("recorded review checkpoint binding changed")?;
    let mut prepared_calls = BTreeMap::new();
    let mut committed_hashes = BTreeMap::new();
    for record in records
        .iter()
        .filter(|record| record.stream_sequence() > checkpoint.turn_boundary_stream_sequence)
    {
        if matches!(record.session_log_entry()?, Some(SessionLogEntry::User(_))) {
            break;
        }
        let Some(event) = record.typed_domain_event_record()? else {
            continue;
        };
        match event.event {
            TypedDomainEvent::MutationPrepared(prepared) => {
                if let Some(call) = prepared.tool_call_id {
                    prepared_calls.insert(prepared.operation_id, call);
                }
            }
            TypedDomainEvent::MutationCommitted(committed) => {
                if let Some(call) = prepared_calls.get(&committed.operation_id)
                    && let MutationSubject::File { path, .. } = committed.committed_subject
                {
                    committed_hashes.insert((call.clone(), path), committed.observed_after_hash);
                }
            }
            _ => {}
        }
    }
    let workspace_id = sigil_kernel::stable_workspace_id(workspace_root)?;
    let source = observe_current
        .then(|| HostSourceObservationRoot::open(workspace_root).ok())
        .flatten();
    let mut diffs = Vec::new();
    let mut remaining = MAX_REVIEW_BYTES;
    let mut observation_budget: u64 = 16 * 1024 * 1024;
    let mut truncated = false;
    for snapshot in committed_checkpoint_previews(&records, checkpoint)? {
        for file in snapshot.file_diffs {
            let Some(binding) = checkpoint
                .files
                .iter()
                .find(|binding| binding.path == Path::new(&file.path))
            else {
                continue;
            };
            let Some(expected_hash) =
                committed_hashes.get(&(snapshot.call_id.clone(), binding.path.clone()))
            else {
                continue;
            };
            ensure!(
                binding.workspace_id == workspace_id,
                "recorded review belongs to another workspace"
            );
            ensure!(
                Path::new(&file.path)
                    .components()
                    .all(|part| matches!(part, Component::Normal(_))),
                "recorded review path is not workspace relative"
            );
            if file.diff.is_empty() {
                continue;
            }
            if diffs.len() == MAX_REVIEW_DIFFS || file.diff.len() > remaining {
                truncated = true;
                continue;
            }
            remaining -= file.diff.len();
            let diff_digest = format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&(
                    &snapshot.call_id,
                    &file.path,
                    &file.diff
                ))?)
            );
            let file_state = match source.as_ref().and_then(|source| {
                source
                    .entry(
                        Path::new(&file.path),
                        observation_budget.min(8 * 1024 * 1024),
                    )
                    .ok()
            }) {
                Some(HostSourceEntryObservation::File {
                    content_digest,
                    observed_bytes,
                }) => {
                    observation_budget = observation_budget.saturating_sub(observed_bytes);
                    match content_digest {
                        Some(hash)
                            if expected_hash.as_deref()
                                == Some(format!("sha256:{hash}").as_str()) =>
                        {
                            ReviewFileState::Current
                        }
                        Some(_) => ReviewFileState::Changed,
                        None => ReviewFileState::Unknown,
                    }
                }
                _ => ReviewFileState::Unknown,
            };
            diffs.push(ApplicationReviewDiff {
                source_call_id: snapshot.call_id.clone(),
                diff_digest,
                path: file.path,
                lines: numbered_diff_lines(&file.diff),
                file_state,
                truncated: file.truncated,
            });
        }
    }
    Ok(ApplicationCheckpointReview {
        checkpoint_id: checkpoint_id.to_owned(),
        checkpoint_digest: checkpoint_digest.to_owned(),
        diffs,
        truncated,
    })
}

/// Materializes explicit user comments with verified recorded source ranges and advisory drift.
/// This is user-request context, not a tool result, file restore or write authorization.
///
/// # Errors
/// Rejects malformed/budget-exceeding comments, foreign or changed immutable bindings, and lines
/// absent from the recorded diff. Current file drift is explicitly allowed and disclosed.
pub fn materialize_review_annotations(
    session_path: &Path,
    expected_session_scope_id: &str,
    workspace_root: &Path,
    annotations: &[ReviewAnnotation],
) -> Result<String> {
    materialize_annotations(
        session_path,
        expected_session_scope_id,
        workspace_root,
        annotations,
        true,
    )
}

/// Materializes immutable queued comment context before the existing queue command is bound.
/// Current-file observations are deliberately omitted: retries of one command must have the
/// same prompt hash, and execution must obtain fresh file contents before proposing a write.
///
/// # Errors
/// Rejects the same exact-source, scope, range and resource-bound violations as direct review.
pub fn materialize_queued_review_annotations(
    session_path: &Path,
    expected_session_scope_id: &str,
    workspace_root: &Path,
    annotations: &[ReviewAnnotation],
) -> Result<String> {
    materialize_annotations(
        session_path,
        expected_session_scope_id,
        workspace_root,
        annotations,
        false,
    )
}

fn materialize_annotations(
    session_path: &Path,
    expected_session_scope_id: &str,
    workspace_root: &Path,
    annotations: &[ReviewAnnotation],
    observe_current: bool,
) -> Result<String> {
    ensure!(
        annotations.len() <= MAX_ANNOTATIONS,
        "too many review annotations"
    );
    let mut context = String::new();
    let mut reviews = BTreeMap::new();
    for annotation in annotations {
        ensure!(
            !annotation.comment.as_str().trim().is_empty()
                && annotation.comment.as_str().len() <= MAX_COMMENT_BYTES,
            "review comment is empty or too large"
        );
        ensure!(
            annotation.start_line > 0
                && annotation.end_line >= annotation.start_line
                && annotation.end_line - annotation.start_line < MAX_RANGE_LINES,
            "review line range is invalid"
        );
        let key = (&annotation.checkpoint_id, &annotation.checkpoint_digest);
        if let std::collections::btree_map::Entry::Vacant(entry) = reviews.entry(key) {
            entry.insert(checkpoint_review(
                session_path,
                expected_session_scope_id,
                workspace_root,
                &annotation.checkpoint_id,
                &annotation.checkpoint_digest,
                observe_current,
            )?);
        }
        let review = &reviews[&key];
        let diff = review
            .diffs
            .iter()
            .find(|diff| {
                diff.source_call_id == annotation.source_call_id
                    && diff.diff_digest == annotation.diff_digest
                    && diff.path == annotation.path
            })
            .context("recorded review diff binding changed")?;
        let mut selected = Vec::new();
        let mut covered = BTreeSet::new();
        for line in &diff.lines {
            let number = match annotation.side {
                ReviewDiffSide::Old => line.old_line,
                ReviewDiffSide::New => line.new_line,
            };
            if let Some(number) = number
                && (annotation.start_line..=annotation.end_line).contains(&number)
            {
                covered.insert(number);
                selected.push((number, &line.text));
            }
        }
        ensure!(
            covered.len() == (annotation.end_line - annotation.start_line + 1) as usize,
            "review line range is absent from recorded diff"
        );
        let state = match diff.file_state {
            ReviewFileState::Current => "matches this recorded change content",
            ReviewFileState::Changed => {
                "outdated: current file differs from checkpoint; read current content before proposing edits"
            }
            ReviewFileState::Unknown => {
                "current file content unknown at execution; read current content before proposing edits"
            }
        };
        writeln!(
            context,
            "\nUser review of recorded change (source reference only, no file-write authority):\ncheckpoint={} digest={} call={} diff={}\npath={} side={:?} lines={}-{}\ncurrent state: {}\ncomment: {}\nrecorded source:",
            annotation.checkpoint_id,
            annotation.checkpoint_digest,
            annotation.source_call_id,
            annotation.diff_digest,
            annotation.path,
            annotation.side,
            annotation.start_line,
            annotation.end_line,
            state,
            annotation.comment.as_str()
        )?;
        for (number, text) in selected {
            writeln!(context, "{number}: {text}")?;
        }
        ensure!(
            context.len() <= MAX_REVIEW_BYTES,
            "review context exceeds byte budget"
        );
    }
    Ok(context)
}

fn numbered_diff_lines(diff: &str) -> Vec<ReviewDiffLine> {
    let mut old = None;
    let mut new = None;
    diff.lines()
        .map(|text| {
            if let Some(header) = text.strip_prefix("@@ -") {
                let ranges = header
                    .split_once(" +")
                    .and_then(|(old, rest)| rest.split_once(" @@").map(|(new, _)| (old, new)));
                old = ranges.and_then(|(old, _)| diff_range(old));
                new = ranges.and_then(|(_, new)| diff_range(new));
                return ReviewDiffLine {
                    text: text.to_owned(),
                    old_line: None,
                    new_line: None,
                };
            }
            let old_line = if text.starts_with(['-', ' ']) {
                take_diff_line(&mut old)
            } else {
                None
            };
            let new_line = if text.starts_with(['+', ' ']) {
                take_diff_line(&mut new)
            } else {
                None
            };
            ReviewDiffLine {
                text: text.to_owned(),
                old_line,
                new_line,
            }
        })
        .collect()
}

fn diff_range(range: &str) -> Option<(u32, u32)> {
    let (start, count) = range.split_once(',').unwrap_or((range, "1"));
    Some((start.parse().ok()?, count.parse().ok()?))
}

fn take_diff_line(range: &mut Option<(u32, u32)>) -> Option<u32> {
    let (line, remaining) = (*range)?;
    if remaining == 0 || line == 0 {
        return None;
    }
    *range = line.checked_add(1).map(|next| (next, remaining - 1));
    Some(line)
}

#[cfg(test)]
#[path = "tests/application_review_tests.rs"]
mod tests;
