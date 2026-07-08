use sigil_kernel::{
    CommandPermissionMatch, PathTrustZone, PermissionConfirmation, PermissionRisk, ToolCall,
    ToolOperation, ToolPreview, ToolSpec, ToolSubject,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalAction {
    AllowOnce,
    AllowSession,
    Deny,
}

impl ApprovalAction {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::AllowOnce => "Allow once",
            Self::AllowSession => "Allow session",
            Self::Deny => "Deny",
        }
    }

    pub(crate) fn order(session_grant_available: bool) -> &'static [Self] {
        if session_grant_available {
            &[Self::AllowOnce, Self::AllowSession, Self::Deny]
        } else {
            &[Self::AllowOnce, Self::Deny]
        }
    }

    pub(crate) fn normalized(self, session_grant_available: bool) -> Self {
        if self == Self::AllowSession && !session_grant_available {
            Self::AllowOnce
        } else {
            self
        }
    }

    pub(crate) fn next(self, session_grant_available: bool, forward: bool) -> Self {
        let order = Self::order(session_grant_available);
        let current = order
            .iter()
            .position(|action| *action == self.normalized(session_grant_available))
            .unwrap_or(0);
        let len = order.len();
        let next = if forward {
            (current + 1) % len
        } else {
            current.checked_sub(1).unwrap_or(len - 1)
        };
        order[next]
    }

    pub(crate) fn default_for(risk: PermissionRisk, session_grant_available: bool) -> Self {
        match risk {
            PermissionRisk::Low | PermissionRisk::Medium => {
                Self::AllowOnce.normalized(session_grant_available)
            }
            PermissionRisk::High if session_grant_available => Self::AllowOnce,
            PermissionRisk::High | PermissionRisk::Destructive | PermissionRisk::Protected => {
                Self::Deny
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn approved(self) -> bool {
        matches!(self, Self::AllowOnce | Self::AllowSession)
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
    pub call_id: String,
    pub source_agent: Option<String>,
    pub access_label: String,
    pub preview_title: String,
    pub preview_summary: String,
    pub change_set: Option<ApprovalChangeSetSummary>,
    pub metadata_collapsed: bool,
    pub file_rows: Vec<ApprovalFileRow>,
    pub changed_files: Vec<String>,
    pub diff_mode_label: &'static str,
    pub active_hunk_index: usize,
    pub hunk_total: usize,
    pub diff_label: String,
    pub diff_lines: Vec<ApprovalDiffLine>,
    pub selected_action: ApprovalAction,
    pub session_grant_available: bool,
}

#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub call: ToolCall,
    pub spec: ToolSpec,
    pub subjects: Vec<ToolSubject>,
    pub operation: ToolOperation,
    pub risk: PermissionRisk,
    pub subject_zones: Vec<PathTrustZone>,
    pub confirmation: Option<PermissionConfirmation>,
    pub snapshot_required: bool,
    pub command_permission_matches: Vec<CommandPermissionMatch>,
    pub session_grant_available: bool,
    pub preview: Option<ToolPreview>,
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
