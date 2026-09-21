//! Path-free wire contract for explicit command-history recovery.
use serde::{Deserialize, Serialize};

use crate::DesktopClientError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopControlLogRecoveryRequest {
    pub logical_journal_id: String,
    pub operation_id: String,
    pub from_generation: u64,
    pub successor_generation: u64,
    pub header_digest: String,
    pub owner_context_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopControlLogRecoveryAuthorityPreview {
    pub request: DesktopControlLogRecoveryRequest,
    pub old_namespace_hash: String,
    pub successor_namespace_hash: String,
    pub old_byte_length: u64,
    pub old_content_digest: String,
    pub old_file_identity: Option<String>,
    pub preview_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopControlLogRecoveryScope {
    pub scope_digest: String,
    pub session_id: Option<String>,
    pub workspace_id: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopControlLogUnresolvedCommand {
    pub key_digest: String,
    pub scope_digest: String,
    pub command_id: String,
    pub command_kind: String,
    pub phase: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopControlLogRecoveryImpact {
    pub verified_prefix_bytes: u64,
    pub verified_record_count: u64,
    pub verified_prefix_digest: String,
    pub known_command_count: u64,
    pub affected_scope_count: u64,
    pub affected_scopes: Vec<DesktopControlLogRecoveryScope>,
    pub scopes_truncated: bool,
    pub known_unresolved_count: u64,
    pub unresolved_commands: Vec<DesktopControlLogUnresolvedCommand>,
    pub commands_truncated: bool,
    pub unparsed_tail_bytes: u64,
    pub tail_command_count_unknown: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopControlLogRecoveryPreview {
    pub authority: DesktopControlLogRecoveryAuthorityPreview,
    pub impact: DesktopControlLogRecoveryImpact,
}

impl std::ops::Deref for DesktopControlLogRecoveryPreview {
    type Target = DesktopControlLogRecoveryAuthorityPreview;
    fn deref(&self) -> &Self::Target {
        &self.authority
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DesktopControlLogRecoveryAction {
    Preview,
    SealAndRotate {
        preview: Box<DesktopControlLogRecoveryPreview>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopCommandJournalBinding {
    pub logical_journal_id: String,
    pub command_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DesktopControlLogRecoveryOutcome {
    Preview(Box<DesktopControlLogRecoveryPreview>),
    Activated(DesktopCommandJournalBinding),
}

const MAX_EXACT_RENDERER_INTEGER: u64 = 9_007_199_254_740_991;

fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl DesktopControlLogRecoveryPreview {
    pub(crate) fn validate(&self) -> Result<(), DesktopClientError> {
        if self.request.from_generation.checked_add(1) != Some(self.request.successor_generation)
            || self.request.successor_generation > MAX_EXACT_RENDERER_INTEGER
            || self.old_byte_length > MAX_EXACT_RENDERER_INTEGER
            || self.request.operation_id.is_empty()
            || self.request.operation_id.len() > 256
            || self.request.operation_id.chars().any(char::is_control)
            || self.old_namespace_hash == self.successor_namespace_hash
            || [
                &self.request.logical_journal_id,
                &self.request.header_digest,
                &self.request.owner_context_digest,
                &self.old_namespace_hash,
                &self.successor_namespace_hash,
                &self.old_content_digest,
                &self.preview_digest,
            ]
            .into_iter()
            .any(|value| !valid_hash(value))
            || self
                .old_file_identity
                .as_ref()
                .is_some_and(|value| !valid_hash(value))
            || (self.old_byte_length > 0 && self.old_file_identity.is_none())
            || self.impact.affected_scopes.len() > 16
            || self.impact.unresolved_commands.len() > 32
            || !valid_hash(&self.impact.verified_prefix_digest)
            || self
                .impact
                .verified_prefix_bytes
                .checked_add(self.impact.unparsed_tail_bytes)
                != Some(self.old_byte_length)
            || self.impact.tail_command_count_unknown != (self.impact.unparsed_tail_bytes > 0)
            || self.impact.known_unresolved_count > self.impact.known_command_count
            || self.impact.affected_scope_count > self.impact.known_command_count
            || self.impact.affected_scope_count < self.impact.affected_scopes.len() as u64
            || self.impact.known_unresolved_count < self.impact.unresolved_commands.len() as u64
            || self.impact.scopes_truncated
                != (self.impact.affected_scope_count > self.impact.affected_scopes.len() as u64)
            || self.impact.commands_truncated
                != (self.impact.known_unresolved_count
                    > self.impact.unresolved_commands.len() as u64)
            || [
                self.impact.verified_record_count,
                self.impact.known_command_count,
                self.impact.affected_scope_count,
                self.impact.known_unresolved_count,
            ]
            .into_iter()
            .any(|value| value > MAX_EXACT_RENDERER_INTEGER)
            || self.impact.affected_scopes.iter().any(|scope| {
                !valid_hash(&scope.scope_digest)
                    || scope.session_id.as_ref().is_some_and(|id| !valid_label(id))
                    || scope
                        .workspace_id
                        .as_ref()
                        .is_some_and(|id| !valid_label(id))
            })
            || self.impact.unresolved_commands.iter().any(|command| {
                !valid_hash(&command.key_digest)
                    || !valid_hash(&command.scope_digest)
                    || !valid_label(&command.command_id)
                    || !valid_label(&command.command_kind)
                    || !matches!(
                        command.phase.as_str(),
                        "Reserved"
                            | "DispatchStarted"
                            | "EffectStarted"
                            | "DomainCommitted"
                            | "Settled"
                            | "Uncertain"
                            | "ConfirmedNoEffect"
                    )
            })
        {
            return Err(DesktopClientError::InvalidResponse);
        }
        Ok(())
    }
}

fn valid_label(value: &str) -> bool {
    !value.is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control)
}

impl DesktopControlLogRecoveryOutcome {
    pub(crate) fn validate(&self) -> Result<(), DesktopClientError> {
        match self {
            Self::Preview(preview) => preview.validate(),
            Self::Activated(binding) => binding.validate(),
        }
    }
}

impl DesktopCommandJournalBinding {
    pub(crate) fn validate(&self) -> Result<(), DesktopClientError> {
        if !valid_hash(&self.logical_journal_id)
            || self.command_generation > MAX_EXACT_RENDERER_INTEGER
        {
            return Err(DesktopClientError::InvalidResponse);
        }
        Ok(())
    }
}
