use anyhow::Result;
use sigil_kernel::{ControlEntry, SessionLogEntry, SessionPurposeV1, SessionStreamRecord};
use thiserror::Error;

/// A valid internal stream cannot be attached as a standalone conversation. This is an eligibility
/// failure, separate from damaged JSONL, route recovery, or provider availability.
#[derive(Debug, Error)]
#[error(
    "this is an internal plan-review research record; resume its parent conversation {parent_session_id}"
)]
pub struct InternalSessionResumeError {
    pub parent_session_id: String,
}

/// Reads host-owned purpose/Plan lineage without inferring purpose from names, message counts,
/// language, or provider configuration. Historical parent Plan drafts remain conversations.
///
/// # Errors
/// Returns an error for an invalid purpose binding, conflicting purposes, or a self-parent cycle.
pub fn internal_session_parent_from_entries(
    session_id: &str,
    entries: &[SessionLogEntry],
) -> Result<Option<String>> {
    let mut purpose = None;
    for entry in entries {
        if let SessionLogEntry::Control(ControlEntry::SessionPurposeBoundV1(binding)) = entry {
            binding.validate()?;
            if let Some(existing) = purpose.as_ref() {
                anyhow::ensure!(
                    existing == &binding.purpose,
                    "session purpose binding changed"
                );
            }
            purpose = Some(binding.purpose.clone());
        }
    }
    if let Some(purpose) = purpose {
        return match purpose {
            SessionPurposeV1::Conversation => Ok(None),
            SessionPurposeV1::PlanReviewResearch {
                parent_session_id, ..
            } => {
                anyhow::ensure!(
                    parent_session_id != session_id,
                    "internal session is its own parent"
                );
                Ok(Some(parent_session_id))
            }
        };
    }
    // Before explicit purpose bindings, only the research stream carried a Plan whose typed
    // source turn belongs to another session. The parent carries the same Plan bound to itself.
    Ok(entries.iter().find_map(|entry| match entry {
        SessionLogEntry::Control(ControlEntry::PlanDraftCreated(draft))
            if draft.source.plan_review_id.is_some() =>
        {
            draft
                .source
                .source_turn
                .as_ref()
                .filter(|source| source.session_scope_id != session_id)
                .map(|source| source.session_scope_id.clone())
        }
        SessionLogEntry::Control(ControlEntry::PlanReviewCandidateRecordedV1(candidate)) => {
            candidate
                .source
                .source_turn
                .as_ref()
                .filter(|source| source.session_scope_id != session_id)
                .map(|source| source.session_scope_id.clone())
        }
        _ => None,
    }))
}

pub(crate) fn internal_session_parent_from_records(
    records: &[SessionStreamRecord],
) -> Result<Option<String>> {
    let Some(first) = records.first() else {
        return Ok(None);
    };
    let entries = records
        .iter()
        .map(SessionStreamRecord::session_log_entry)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    internal_session_parent_from_entries(first.session_id(), &entries)
}
