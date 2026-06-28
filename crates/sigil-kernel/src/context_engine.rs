use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ArtifactId, EventId, ReceiptId, VerificationVerdict};

pub type ContextItemId = String;
pub type ContextEgressDecisionId = String;
pub type ContextRepoRevision = String;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextSource {
    SystemPrompt,
    UserMessage,
    WorkspaceInstruction,
    RepositoryFile,
    ToolObservation,
    DurableEvent,
    EvidenceReceipt,
    MutationEvidence,
    VerificationEvidence,
    LspSymbol,
    SessionArchive,
    TaskDigest,
    ExtensionProvided,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ContextTrustLevel {
    System,
    UserProvided,
    WorkspaceInstruction,
    UntrustedRepositoryData,
    ToolObservation,
    ExtensionProvided,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ContextSensitivity {
    Public,
    Repository,
    PotentialSecret,
    Secret,
    External,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextInclusionReason {
    StablePrompt,
    UserRequest,
    RecentTurn,
    ActiveFile,
    WorkspaceInstruction,
    VerificationState,
    RetrievalHit,
    RequiredEvidence,
    TokenBudget,
    ExcludedUntrustedWorkspace,
    ExcludedSecret,
    ExcludedEgressDenied,
    ExcludedUnsupported,
}

impl ContextInclusionReason {
    #[must_use]
    pub fn is_included(&self) -> bool {
        !matches!(
            self,
            Self::ExcludedUntrustedWorkspace
                | Self::ExcludedSecret
                | Self::ExcludedEgressDenied
                | Self::ExcludedUnsupported
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextBodyRef {
    Inline {
        content_hash: String,
        byte_len: usize,
    },
    WorkspacePath(PathBuf),
    DurableEvent(EventId),
    Receipt(ReceiptId),
    Artifact(ArtifactId),
}

impl ContextBodyRef {
    #[must_use]
    pub fn inline(body: &str) -> Self {
        Self::Inline {
            content_hash: format!("{:x}", Sha256::digest(body.as_bytes())),
            byte_len: body.len(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ContextItem {
    pub id: ContextItemId,
    pub source: ContextSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_event_id: Option<EventId>,
    pub trust_level: ContextTrustLevel,
    pub sensitivity: ContextSensitivity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress_decision: Option<ContextEgressDecisionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_revision: Option<ContextRepoRevision>,
    pub token_cost: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    pub inclusion_reason: ContextInclusionReason,
    pub body_ref: ContextBodyRef,
}

impl ContextItem {
    /// Validates trust and egress labels before an item can be attached to a digest.
    ///
    /// # Errors
    ///
    /// Returns an error when a trusted workspace instruction is mislabeled or when an included
    /// secret-like item lacks an egress decision.
    pub fn validate(&self) -> Result<()> {
        if self.trust_level == ContextTrustLevel::WorkspaceInstruction
            && self.source != ContextSource::WorkspaceInstruction
        {
            bail!("workspace instruction trust requires workspace instruction source");
        }
        if self.source == ContextSource::WorkspaceInstruction
            && self.trust_level != ContextTrustLevel::WorkspaceInstruction
        {
            bail!("workspace instruction source must carry workspace instruction trust");
        }
        if self.inclusion_reason.is_included()
            && self.sensitivity == ContextSensitivity::Secret
            && self.egress_decision.is_none()
        {
            bail!("included secret context requires an egress decision");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextDigestTextKind {
    UserProvided,
    SystemDerived,
    ModelInferred,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ContextDigestText {
    pub text: String,
    pub kind: ContextDigestTextKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_event_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_receipt_id: Option<ReceiptId>,
}

impl ContextDigestText {
    #[must_use]
    pub fn user_provided(text: impl Into<String>, source_event_id: impl Into<EventId>) -> Self {
        Self {
            text: text.into(),
            kind: ContextDigestTextKind::UserProvided,
            source_event_id: Some(source_event_id.into()),
            source_receipt_id: None,
        }
    }

    #[must_use]
    pub fn system_derived(text: impl Into<String>, source_event_id: impl Into<EventId>) -> Self {
        Self {
            text: text.into(),
            kind: ContextDigestTextKind::SystemDerived,
            source_event_id: Some(source_event_id.into()),
            source_receipt_id: None,
        }
    }

    #[must_use]
    pub fn model_inferred(text: impl Into<String>, source_event_id: impl Into<EventId>) -> Self {
        Self {
            text: text.into(),
            kind: ContextDigestTextKind::ModelInferred,
            source_event_id: Some(source_event_id.into()),
            source_receipt_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ContextDigestV0 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<ContextDigestText>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_files: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_commands: Vec<ReceiptId>,
    pub verification_state: VerificationVerdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_receipt_id: Option<ReceiptId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<ContextDigestText>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_items: Vec<ContextItem>,
}

#[derive(Debug, Clone)]
pub struct ContextDigestV0Builder {
    objective: Option<ContextDigestText>,
    active_files: BTreeSet<PathBuf>,
    recent_commands: Vec<ReceiptId>,
    verification_state: VerificationVerdict,
    verification_receipt_id: Option<ReceiptId>,
    unresolved: Vec<ContextDigestText>,
    context_items: Vec<ContextItem>,
}

impl Default for ContextDigestV0Builder {
    fn default() -> Self {
        Self {
            objective: None,
            active_files: BTreeSet::new(),
            recent_commands: Vec::new(),
            verification_state: VerificationVerdict::NotEvaluated,
            verification_receipt_id: None,
            unresolved: Vec::new(),
            context_items: Vec::new(),
        }
    }
}

impl ContextDigestV0Builder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn objective(mut self, objective: ContextDigestText) -> Self {
        self.objective = Some(objective);
        self
    }

    #[must_use]
    pub fn active_file(mut self, path: impl AsRef<Path>) -> Self {
        self.active_files.insert(path.as_ref().to_path_buf());
        self
    }

    #[must_use]
    pub fn recent_command(mut self, receipt_id: impl Into<ReceiptId>) -> Self {
        let receipt_id = receipt_id.into();
        if !self.recent_commands.contains(&receipt_id) {
            self.recent_commands.push(receipt_id);
        }
        self
    }

    #[must_use]
    pub fn verification_state(
        mut self,
        verdict: VerificationVerdict,
        receipt_id: Option<ReceiptId>,
    ) -> Self {
        self.verification_state = verdict;
        self.verification_receipt_id = receipt_id;
        self
    }

    #[must_use]
    pub fn unresolved(mut self, item: ContextDigestText) -> Self {
        self.unresolved.push(item);
        self
    }

    pub fn context_item(mut self, item: ContextItem) -> Result<Self> {
        item.validate()?;
        self.context_items.push(item);
        Ok(self)
    }

    /// Builds a deterministic digest.
    ///
    /// # Errors
    ///
    /// Returns an error if the digest would claim passed verification without an existing
    /// verification receipt or if one attached context item has invalid trust/egress labels.
    pub fn build(self) -> Result<ContextDigestV0> {
        if self.verification_state == VerificationVerdict::Passed
            && self.verification_receipt_id.is_none()
        {
            bail!("context digest cannot claim passed verification without a receipt reference");
        }
        for item in &self.context_items {
            item.validate()?;
        }

        Ok(ContextDigestV0 {
            objective: self.objective,
            active_files: self.active_files.into_iter().collect(),
            recent_commands: self.recent_commands,
            verification_state: self.verification_state,
            verification_receipt_id: self.verification_receipt_id,
            unresolved: self.unresolved,
            context_items: self.context_items,
        })
    }
}

pub fn estimate_context_token_cost(text: &str) -> usize {
    text.split_whitespace().count().max(1)
}

#[cfg(test)]
#[path = "tests/context_engine_tests.rs"]
mod tests;
