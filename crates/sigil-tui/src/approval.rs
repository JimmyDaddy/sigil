use sigil_kernel::{ToolCall, ToolPreview, ToolSpec, ToolSubject};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalAction {
    Allow,
    Deny,
}

impl ApprovalAction {
    pub(crate) fn toggled(self) -> Self {
        match self {
            Self::Allow => Self::Deny,
            Self::Deny => Self::Allow,
        }
    }

    pub(crate) fn approved(self) -> bool {
        matches!(self, Self::Allow)
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
}

#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub call: ToolCall,
    pub spec: ToolSpec,
    pub subjects: Vec<ToolSubject>,
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
