use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{PlanId, PlanReviewAttemptId, PlanReviewId};

/// Durable purpose of a session stream. Internal research retains its own audit log but does not
/// grant the right to reopen it as a standalone user conversation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionPurposeV1 {
    Conversation,
    PlanReviewResearch {
        parent_session_id: String,
        plan_review_id: PlanReviewId,
        attempt_id: PlanReviewAttemptId,
        plan_id: PlanId,
    },
}

/// Append-only host binding written before a purpose-specific child begins work. Absence of this
/// record preserves compatibility with ordinary historical conversations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionPurposeBoundV1 {
    pub schema_version: u16,
    pub purpose: SessionPurposeV1,
}

impl SessionPurposeBoundV1 {
    /// Checks the bounded host identity carried by this purpose record.
    ///
    /// # Errors
    /// Returns an error for an unsupported schema or an unsafe parent identity.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 1,
            "unsupported session purpose schema"
        );
        if let SessionPurposeV1::PlanReviewResearch {
            parent_session_id, ..
        } = &self.purpose
        {
            ensure!(
                !parent_session_id.is_empty()
                    && parent_session_id.len() <= 128
                    && parent_session_id
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-'),
                "invalid parent session identity in session purpose"
            );
        }
        Ok(())
    }
}
