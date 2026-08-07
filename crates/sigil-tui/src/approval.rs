use std::collections::BTreeSet;

use sigil_kernel::{
    ApprovalMode, CommandPermissionMatch, ExecutionContainmentRequest, NetworkEffect,
    PathTrustZone, PermissionConfirmation, PermissionDecisionReason, PermissionRisk,
    ToolAnalysisStatus, ToolApprovalSessionGrantUnavailableReason,
    ToolApprovalSessionGrantUnavailableReasonCode, ToolCall, ToolOperation, ToolPermissionEffect,
    ToolPermissionSummary, ToolPreview, ToolSpec, ToolSubject,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalAction {
    AllowOnce,
    AllowSession,
    AllowFamily,
    Deny,
}

impl ApprovalAction {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::AllowOnce => "Allow once",
            Self::AllowSession => "Allow session",
            Self::AllowFamily => "Allow family",
            Self::Deny => "Deny",
        }
    }

    pub(crate) fn order(session_grant_available: bool, family_available: bool) -> &'static [Self] {
        match (session_grant_available, family_available) {
            (true, true) => &[
                Self::AllowOnce,
                Self::AllowSession,
                Self::AllowFamily,
                Self::Deny,
            ],
            (true, false) => &[Self::AllowOnce, Self::AllowSession, Self::Deny],
            (false, true) => &[Self::AllowOnce, Self::AllowFamily, Self::Deny],
            (false, false) => &[Self::AllowOnce, Self::Deny],
        }
    }

    pub(crate) fn normalized(self, session_grant_available: bool, family_available: bool) -> Self {
        match self {
            Self::AllowSession if !session_grant_available => Self::AllowOnce,
            Self::AllowFamily if !family_available => Self::AllowOnce,
            _ => self,
        }
    }

    pub(crate) fn next(
        self,
        session_grant_available: bool,
        family_available: bool,
        forward: bool,
    ) -> Self {
        let order = Self::order(session_grant_available, family_available);
        let current = order
            .iter()
            .position(|action| {
                *action == self.normalized(session_grant_available, family_available)
            })
            .unwrap_or(0);
        let len = order.len();
        let next = if forward {
            (current + 1) % len
        } else {
            current.checked_sub(1).unwrap_or(len - 1)
        };
        order[next]
    }

    pub(crate) fn default_for(
        risk: PermissionRisk,
        session_grant_available: bool,
        family_available: bool,
    ) -> Self {
        match risk {
            PermissionRisk::Low | PermissionRisk::Medium => {
                Self::AllowOnce.normalized(session_grant_available, family_available)
            }
            PermissionRisk::High if session_grant_available => Self::AllowOnce,
            PermissionRisk::High | PermissionRisk::Destructive | PermissionRisk::Protected => {
                Self::Deny
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn approved(self) -> bool {
        matches!(
            self,
            Self::AllowOnce | Self::AllowSession | Self::AllowFamily
        )
    }

    #[cfg(test)]
    pub(crate) fn grants_session(self) -> bool {
        matches!(self, Self::AllowSession)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalDiffLineKind {
    Header,
    Hunk,
    Added,
    Removed,
    Context,
}

#[derive(Debug, Clone)]
pub(crate) struct ApprovalDiffLine {
    pub text: String,
    pub kind: ApprovalDiffLineKind,
    pub active_hunk: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ApprovalDiagnosticSummary {
    pub errors: usize,
    pub warnings: usize,
}

impl ApprovalDiagnosticSummary {
    pub(crate) fn is_clean(self) -> bool {
        self.errors == 0 && self.warnings == 0
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ApprovalFileRow {
    pub path: String,
    pub selected: bool,
    pub diagnostics: Option<ApprovalDiagnosticSummary>,
    pub action: Option<String>,
    pub risk: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApprovalChangeSetSummary {
    pub id: String,
    pub risk: String,
    pub format_hint: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ApprovalModalView {
    pub tool_name: String,
    pub source_agent: Option<String>,
    pub access_label: String,
    pub risk: PermissionRisk,
    pub policy_label: String,
    pub effects: BTreeSet<ToolPermissionEffect>,
    pub subjects: Vec<ToolSubject>,
    pub analysis: ToolAnalysisStatus,
    pub containment: ExecutionContainmentRequest,
    pub safe_summary: ToolPermissionSummary,
    pub decision_reasons: Vec<PermissionDecisionReason>,
    pub preview_title: String,
    pub preview_summary: String,
    pub change_set: Option<ApprovalChangeSetSummary>,
    pub metadata_collapsed: bool,
    pub has_diff_preview: bool,
    pub file_rows: Vec<ApprovalFileRow>,
    pub changed_files: Vec<String>,
    pub diff_mode_label: &'static str,
    pub active_hunk_index: usize,
    pub hunk_total: usize,
    pub diff_label: String,
    pub diff_lines: Vec<ApprovalDiffLine>,
    pub selected_action: ApprovalAction,
    pub session_grant_available: bool,
    pub session_grant_unavailable_reason: Option<ToolApprovalSessionGrantUnavailableReason>,
    pub command_family_allow_pattern: Option<String>,
}

#[cfg(test)]
impl Default for ApprovalModalView {
    fn default() -> Self {
        Self {
            tool_name: String::new(),
            source_agent: None,
            access_label: String::new(),
            risk: PermissionRisk::Low,
            policy_label: String::new(),
            effects: BTreeSet::new(),
            subjects: Vec::new(),
            analysis: ToolAnalysisStatus::Complete,
            containment: ExecutionContainmentRequest::default(),
            safe_summary: ToolPermissionSummary::default(),
            decision_reasons: Vec::new(),
            preview_title: String::new(),
            preview_summary: String::new(),
            change_set: None,
            metadata_collapsed: false,
            has_diff_preview: false,
            file_rows: Vec::new(),
            changed_files: Vec::new(),
            diff_mode_label: "full",
            active_hunk_index: 0,
            hunk_total: 0,
            diff_label: String::new(),
            diff_lines: Vec::new(),
            selected_action: ApprovalAction::AllowOnce,
            session_grant_available: false,
            session_grant_unavailable_reason: Some(ToolApprovalSessionGrantUnavailableReason {
                code: ToolApprovalSessionGrantUnavailableReasonCode::NoReusableApprovalFacet,
            }),
            command_family_allow_pattern: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub approval_request_id: String,
    pub call: ToolCall,
    pub spec: ToolSpec,
    pub effects: BTreeSet<ToolPermissionEffect>,
    pub subjects: Vec<ToolSubject>,
    pub analysis: ToolAnalysisStatus,
    pub containment: ExecutionContainmentRequest,
    pub safe_summary: ToolPermissionSummary,
    pub decision_reasons: Vec<PermissionDecisionReason>,
    pub network_effect: Option<NetworkEffect>,
    pub local_policy_decision: ApprovalMode,
    pub network_policy_decision: ApprovalMode,
    pub source_policy_decision: ApprovalMode,
    pub operation: ToolOperation,
    pub risk: PermissionRisk,
    pub subject_zones: Vec<PathTrustZone>,
    pub confirmation: Option<PermissionConfirmation>,
    pub snapshot_required: bool,
    pub command_permission_matches: Vec<CommandPermissionMatch>,
    pub session_grant_available: bool,
    pub session_grant_unavailable_reason: Option<ToolApprovalSessionGrantUnavailableReason>,
    /// Derived durable allow pattern (`cargo test*`) shown as the "Allow family" action, when
    /// the command can be safely widened into a command-family rule.
    pub command_family_allow_pattern: Option<String>,
    pub preview: Option<ToolPreview>,
    pub presentation_state: ApprovalPresentationState,
}

/// Derives the durable command-family allow pattern for a shell tool call, or `None` when the
/// tool does not take a shell command or the command must not be widened.
pub(crate) fn command_family_pattern_for_call(call: &ToolCall) -> Option<String> {
    sigil_kernel::derive_command_family_allow_pattern_for_call(call)
}

pub(crate) fn session_grant_unavailable_reason_label(
    reason: Option<ToolApprovalSessionGrantUnavailableReason>,
) -> &'static str {
    use ToolApprovalSessionGrantUnavailableReasonCode as Reason;

    match reason.map(|reason| reason.code) {
        Some(Reason::AnalysisIncomplete) => {
            "tool effects could not be analyzed completely; only this request can be approved"
        }
        Some(Reason::SemanticScopeUnavailable) => {
            "the request has no stable reusable scope; only this request can be approved"
        }
        Some(Reason::NonGrantableEffect) => {
            "the request includes an effect that cannot be safely reused"
        }
        Some(Reason::ContainmentBindingUnavailable) => {
            "the execution limits cannot be bound to reusable authority"
        }
        Some(Reason::PolicyDecisionNotGrantable) => {
            "the current permission policy does not allow reusable authority"
        }
        Some(Reason::NoReusableApprovalFacet) => {
            "there is no pending permission facet that can be reused"
        }
        Some(Reason::NetworkScopeNotGrantable) => {
            "the network scope is not a stable, bounded read-only scope"
        }
        Some(Reason::ConfirmationRequired) => "the request requires an explicit confirmation",
        Some(Reason::SnapshotRequired) => "the decision is bound to a file snapshot",
        Some(Reason::SubjectScopeUnavailable) => {
            "the request targets cannot be represented as a stable bounded scope"
        }
        Some(Reason::RiskNotGrantable) => {
            "the request risk level does not allow reusable authority"
        }
        Some(Reason::ExternalMutation) => {
            "mutations outside the workspace cannot receive reusable authority"
        }
        Some(Reason::OperationNotGrantable) => {
            "this operation type cannot receive reusable authority"
        }
        None => "this request cannot receive reusable authority",
    }
}

/// Local presentation state for one exact approval request.
///
/// `DecisionAccepted` is an acknowledgement from the local worker route. It does not imply that
/// the tool has started; the matching kernel resolution/execution events remain authoritative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalPresentationState {
    Pending,
    DecisionAccepted { command_id: String },
    DeliveryUncertain { command_id: String },
}

impl PendingApproval {
    pub(crate) fn actions_available(&self) -> bool {
        matches!(self.presentation_state, ApprovalPresentationState::Pending)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDiffMode {
    Full,
    CurrentHunk,
    ChangedOnly,
}

impl ApprovalDiffMode {
    pub(crate) fn next(self) -> Self {
        match self {
            Self::Full => Self::CurrentHunk,
            Self::CurrentHunk => Self::ChangedOnly,
            Self::ChangedOnly => Self::Full,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::CurrentHunk => "current-hunk",
            Self::ChangedOnly => "changed-only",
        }
    }
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/approval_tests.rs"]
mod tests;
