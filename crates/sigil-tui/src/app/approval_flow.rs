use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;
use sigil_kernel::{PathTrustZone, PermissionConfirmation, PermissionRisk, ToolOperation};

use super::{
    AppAction, AppState, ApprovalChangeSetSummary, ApprovalDiagnosticSummary, ApprovalDiffLine,
    ApprovalDiffLineKind, ApprovalDiffMode, ApprovalFileRow, ApprovalModalView, PaneFocus,
    formatting::normalize_command_prefix_character,
};

impl AppState {
    pub fn approval_preview_lines(&self) -> Vec<String> {
        let Some(pending) = &self.approval.pending else {
            return self.session_view_lines();
        };

        let mut lines = Vec::new();
        if let Some(preview) = &pending.preview {
            if !self.approval.metadata_collapsed {
                lines.push(format!(
                    "tool={}  id={}  mode={}",
                    pending.call.name,
                    pending.call.id,
                    approval_access_label(&pending.spec)
                ));
                if let Some(source_agent) = self.pending_approval_source_agent(&pending.call.id) {
                    lines.push(format!("source_agent={source_agent}"));
                }
                lines.extend(approval_permission_lines(
                    pending.operation,
                    pending.risk,
                    &pending.subject_zones,
                    pending.confirmation.as_ref(),
                    pending.snapshot_required,
                ));
                lines.extend(approval_subject_lines(&pending.subjects));
                lines.push(format!("preview={}", preview.title));
                if !preview.summary.trim().is_empty() {
                    lines.push(preview.summary.clone());
                }
                lines.push(String::new());
            } else {
                lines.push("meta hidden".to_owned());
            }

            if !preview.file_diffs.is_empty() {
                lines.push(format!(
                    "file {}/{}",
                    self.approval
                        .selected_file_index
                        .min(preview.file_diffs.len() - 1)
                        + 1,
                    preview.file_diffs.len()
                ));
                for (index, file) in preview.file_diffs.iter().enumerate() {
                    let selected = if index == self.approval.selected_file_index {
                        ">"
                    } else {
                        " "
                    };
                    lines.push(format!("{selected} {}", file.path));
                }
                lines.push(String::new());
            } else if !preview.changed_files.is_empty() {
                lines.push(format!("changed: {}", preview.changed_files.join(", ")));
                lines.push(String::new());
            }

            let diff = self
                .selected_approval_diff()
                .unwrap_or(preview.body.as_str());
            let diff = self.transform_approval_diff(diff);
            let hunk_positions = self.approval_hunk_positions();
            let active_hunk_line = match self.approval.diff_mode {
                ApprovalDiffMode::Full => hunk_positions
                    .get(self.approval.selected_hunk_index)
                    .copied()
                    .unwrap_or(usize::MAX),
                ApprovalDiffMode::CurrentHunk | ApprovalDiffMode::ChangedOnly => 0,
            };
            lines.push(format!(
                "mode={}  hunk {}/{}  [,] hunk  ,/. file  m meta  v view",
                self.approval.diff_mode.label(),
                if hunk_positions.is_empty() {
                    0
                } else {
                    self.approval.selected_hunk_index + 1
                },
                hunk_positions.len()
            ));
            lines.push(String::new());
            for (index, line) in diff.lines().enumerate() {
                let prefix = if index == active_hunk_line {
                    ">> "
                } else if line.starts_with("@@") {
                    " > "
                } else {
                    "   "
                };
                lines.push(format!("{prefix}{line}"));
            }
        } else {
            lines.push(format!(
                "tool={}  id={}  mode={}",
                pending.call.name,
                pending.call.id,
                approval_access_label(&pending.spec)
            ));
            if let Some(source_agent) = self.pending_approval_source_agent(&pending.call.id) {
                lines.push(format!("source_agent={source_agent}"));
            }
            lines.extend(approval_permission_lines(
                pending.operation,
                pending.risk,
                &pending.subject_zones,
                pending.confirmation.as_ref(),
                pending.snapshot_required,
            ));
            lines.extend(approval_subject_lines(&pending.subjects));
            lines.push(format!("args={}", pending.call.args_json));
        }

        lines.push(String::new());
        if spawn_agent_background_args_json(&pending.call.name, &pending.call.args_json).is_some() {
            lines.push("Y allow once  B background  N deny".to_owned());
        } else if pending.session_grant_available {
            lines.push("Y allow once  Tab/←/→ action  Enter choose  N deny".to_owned());
        } else {
            lines.push("Y allow once  N deny".to_owned());
        }
        lines
    }

    pub(super) fn handle_pending_approval_key_event(
        &mut self,
        key: KeyEvent,
    ) -> Option<Option<AppAction>> {
        if let Some(pending) = &self.approval.pending {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    return Some(Some(AppAction::ApprovalDecision {
                        call_id: pending.call.id.clone(),
                        approved: true,
                    }));
                }
                KeyCode::Char('n') | KeyCode::Char('N') => {
                    return Some(Some(AppAction::ApprovalDecision {
                        call_id: pending.call.id.clone(),
                        approved: false,
                    }));
                }
                KeyCode::Char('b') | KeyCode::Char('B') => {
                    if let Some(args_json) = spawn_agent_background_args_json(
                        &pending.call.name,
                        &pending.call.args_json,
                    ) {
                        return Some(Some(AppAction::ApprovalDecisionWithArgs {
                            call_id: pending.call.id.clone(),
                            args_json,
                        }));
                    }
                }
                KeyCode::Enter if key.modifiers.is_empty() => {
                    let selected = self
                        .approval
                        .selected_action
                        .normalized(pending.session_grant_available);
                    return Some(Some(match selected {
                        super::ApprovalAction::AllowOnce => AppAction::ApprovalDecision {
                            call_id: pending.call.id.clone(),
                            approved: true,
                        },
                        super::ApprovalAction::AllowSession => AppAction::ApprovalSessionDecision {
                            call_id: pending.call.id.clone(),
                        },
                        super::ApprovalAction::Deny => AppAction::ApprovalDecision {
                            call_id: pending.call.id.clone(),
                            approved: false,
                        },
                    }));
                }
                _ => {}
            }
        }

        if self.approval.pending.is_none() || key.modifiers.contains(KeyModifiers::CONTROL) {
            return None;
        }

        match key.code {
            KeyCode::Char(character) if normalize_command_prefix_character(character).is_some() => {
                self.active_pane = PaneFocus::Composer;
                self.insert_input_character('/');
                self.reset_input_history_navigation();
                self.reset_slash_selector();
                Some(None)
            }
            KeyCode::Char('m') | KeyCode::Char('M') => {
                self.toggle_approval_metadata();
                Some(None)
            }
            KeyCode::Char('[') => {
                self.jump_approval_hunk(false);
                Some(None)
            }
            KeyCode::Char(']') => {
                self.jump_approval_hunk(true);
                Some(None)
            }
            KeyCode::Char(',') => {
                self.switch_approval_file(false);
                Some(None)
            }
            KeyCode::Char('.') => {
                self.switch_approval_file(true);
                Some(None)
            }
            KeyCode::Char('v') | KeyCode::Char('V') => {
                self.cycle_approval_diff_mode();
                Some(None)
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                let forward = matches!(key.code, KeyCode::Right | KeyCode::Tab);
                let session_grant_available = self
                    .approval
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.session_grant_available);
                self.approval.selected_action = self
                    .approval
                    .selected_action
                    .next(session_grant_available, forward);
                self.push_event("approval:action", self.approval.selected_action.label());
                Some(None)
            }
            KeyCode::Up => {
                self.scroll_active_pane(1);
                Some(None)
            }
            KeyCode::Down => {
                self.unscroll_active_pane(1);
                Some(None)
            }
            KeyCode::PageUp => {
                self.scroll_active_pane(8);
                Some(None)
            }
            KeyCode::PageDown => {
                self.unscroll_active_pane(8);
                Some(None)
            }
            KeyCode::Home => {
                self.scroll_active_pane(usize::MAX / 2);
                Some(None)
            }
            KeyCode::End => {
                self.unscroll_active_pane(usize::MAX / 2);
                Some(None)
            }
            KeyCode::Esc => {
                self.active_pane = PaneFocus::Activity;
                Some(None)
            }
            KeyCode::Char(_) | KeyCode::Backspace => Some(None),
            _ => None,
        }
    }

    pub(crate) fn approval_modal_view(&self) -> Option<ApprovalModalView> {
        let pending = self.approval.pending.as_ref()?;
        let access_label = approval_access_label(&pending.spec);
        let source_agent = self.pending_approval_source_agent(&pending.call.id);
        let Some(preview) = pending.preview.as_ref() else {
            return Some(ApprovalModalView {
                tool_name: pending.call.name.clone(),
                call_id: pending.call.id.clone(),
                source_agent,
                access_label,
                preview_title: format!("Run {}", pending.call.name),
                preview_summary: approval_subject_summary(&pending.subjects)
                    .unwrap_or_else(|| "Tool preview unavailable for this call.".to_owned()),
                change_set: None,
                metadata_collapsed: self.approval.metadata_collapsed,
                file_rows: Vec::new(),
                changed_files: Vec::new(),
                diff_mode_label: self.approval.diff_mode.label(),
                active_hunk_index: 0,
                hunk_total: 0,
                diff_label: pending.call.name.clone(),
                diff_lines: vec![ApprovalDiffLine {
                    text: "No structured diff preview available.".to_owned(),
                    kind: ApprovalDiffLineKind::Context,
                    active_hunk: false,
                }],
                selected_action: self
                    .approval
                    .selected_action
                    .normalized(pending.session_grant_available),
                session_grant_available: pending.session_grant_available,
            });
        };

        let change_set = approval_changeset_summary(&pending.call.name, &pending.call.args_json);
        let change_set_files =
            approval_changeset_file_metadata(&pending.call.name, &pending.call.args_json);
        let raw_diff = self
            .selected_approval_diff()
            .unwrap_or(preview.body.as_str());
        let transformed_diff = self.transform_approval_diff(raw_diff);
        let transformed_lines = transformed_diff.lines().collect::<Vec<_>>();
        let hunk_positions = self.approval_hunk_positions();
        let active_hunk_index = if hunk_positions.is_empty() {
            0
        } else {
            self.approval
                .selected_hunk_index
                .min(hunk_positions.len() - 1)
                + 1
        };
        let active_hunk_line = match self.approval.diff_mode {
            ApprovalDiffMode::Full => hunk_positions
                .get(self.approval.selected_hunk_index)
                .copied()
                .unwrap_or(usize::MAX),
            ApprovalDiffMode::CurrentHunk | ApprovalDiffMode::ChangedOnly => transformed_lines
                .iter()
                .position(|line| line.starts_with("@@"))
                .unwrap_or(0),
        };

        let mut diff_lines = transformed_lines
            .iter()
            .enumerate()
            .map(|(index, line)| ApprovalDiffLine {
                text: (*line).to_owned(),
                kind: approval_diff_line_kind(line),
                active_hunk: index == active_hunk_line && line.starts_with("@@"),
            })
            .collect::<Vec<_>>();
        if diff_lines.is_empty() {
            diff_lines.push(ApprovalDiffLine {
                text: "No preview body available.".to_owned(),
                kind: ApprovalDiffLineKind::Context,
                active_hunk: false,
            });
        }

        let file_rows: Vec<ApprovalFileRow> = if !preview.file_diffs.is_empty() {
            preview
                .file_diffs
                .iter()
                .enumerate()
                .map(|(index, file)| ApprovalFileRow {
                    action: change_set_files
                        .get(&normalize_approval_diagnostic_path(&file.path))
                        .map(|metadata| metadata.action.clone()),
                    risk: change_set_files
                        .get(&normalize_approval_diagnostic_path(&file.path))
                        .map(|metadata| metadata.risk.clone()),
                    path: file.path.clone(),
                    selected: index == self.approval.selected_file_index,
                    diagnostics: self.approval_diagnostics_for_path(&file.path),
                })
                .collect()
        } else {
            preview
                .changed_files
                .iter()
                .enumerate()
                .map(|(index, path)| ApprovalFileRow {
                    action: change_set_files
                        .get(&normalize_approval_diagnostic_path(path))
                        .map(|metadata| metadata.action.clone()),
                    risk: change_set_files
                        .get(&normalize_approval_diagnostic_path(path))
                        .map(|metadata| metadata.risk.clone()),
                    path: path.clone(),
                    selected: index == self.approval.selected_file_index,
                    diagnostics: self.approval_diagnostics_for_path(path),
                })
                .collect()
        };

        let diff_label = file_rows
            .iter()
            .find(|row| row.selected)
            .map(|row| row.path.clone())
            .filter(|path: &String| !path.is_empty())
            .unwrap_or_else(|| preview.title.clone());

        Some(ApprovalModalView {
            tool_name: pending.call.name.clone(),
            call_id: pending.call.id.clone(),
            source_agent,
            access_label,
            preview_title: preview.title.clone(),
            preview_summary: preview.summary.clone(),
            change_set,
            metadata_collapsed: self.approval.metadata_collapsed,
            file_rows,
            changed_files: preview.changed_files.clone(),
            diff_mode_label: self.approval.diff_mode.label(),
            active_hunk_index,
            hunk_total: hunk_positions.len(),
            diff_label,
            diff_lines,
            selected_action: self
                .approval
                .selected_action
                .normalized(pending.session_grant_available),
            session_grant_available: pending.session_grant_available,
        })
    }

    fn selected_approval_diff(&self) -> Option<&str> {
        let preview = self
            .approval
            .pending
            .as_ref()
            .and_then(|pending| pending.preview.as_ref())?;
        preview
            .file_diffs
            .get(self.approval.selected_file_index)
            .map(|file| file.diff.as_str())
            .or_else(|| (!preview.body.is_empty()).then_some(preview.body.as_str()))
    }

    fn approval_diagnostics_for_path(&self, path: &str) -> Option<ApprovalDiagnosticSummary> {
        self.runtime
            .code_intelligence_diagnostics_by_path
            .get(&normalize_approval_diagnostic_path(path))
            .copied()
    }

    fn pending_approval_source_agent(&self, call_id: &str) -> Option<String> {
        let projection = sigil_kernel::AgentThreadStateProjection::from_entries(
            &self.session_browser.current_entries,
        );
        let route = projection
            .approval_routes
            .values()
            .find(|route| route.call_id == call_id)?;
        let source_label = projection
            .threads
            .get(&route.source_thread_id)
            .and_then(|thread| thread.display_name.clone())
            .or_else(|| {
                projection
                    .threads
                    .get(&route.source_thread_id)
                    .and_then(|thread| thread.profile_id.as_ref())
                    .map(|profile_id| profile_id.as_str().to_owned())
            })
            .unwrap_or_else(|| route.source_thread_id.as_str().to_owned());
        if source_label == route.source_thread_id.as_str() {
            Some(source_label)
        } else {
            Some(format!(
                "{source_label} · {}",
                route.source_thread_id.as_str()
            ))
        }
    }

    fn approval_hunk_positions(&self) -> Vec<usize> {
        self.selected_approval_diff()
            .map(|diff| {
                diff.lines()
                    .enumerate()
                    .filter_map(|(index, line)| line.starts_with("@@").then_some(index))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn transform_approval_diff(&self, diff: &str) -> String {
        match self.approval.diff_mode {
            ApprovalDiffMode::Full => diff.to_owned(),
            ApprovalDiffMode::CurrentHunk => self.extract_current_hunk(diff),
            ApprovalDiffMode::ChangedOnly => self.extract_changed_only(diff),
        }
    }

    fn extract_current_hunk(&self, diff: &str) -> String {
        let lines = diff.lines().collect::<Vec<_>>();
        let hunk_positions = self.approval_hunk_positions();
        if hunk_positions.is_empty() {
            return diff.to_owned();
        }
        let hunk_index = self
            .approval
            .selected_hunk_index
            .min(hunk_positions.len().saturating_sub(1));
        let start = hunk_positions[hunk_index];
        let end = hunk_positions
            .get(hunk_index + 1)
            .copied()
            .unwrap_or(lines.len());

        let mut out = Vec::new();
        let header_limit = start.min(2);
        out.extend(lines.iter().take(header_limit).copied());
        if header_limit < start {
            out.push("...");
        }
        out.extend(lines[start..end].iter().copied());
        out.join("\n")
    }

    fn extract_changed_only(&self, diff: &str) -> String {
        diff.lines()
            .filter(|line| {
                line.starts_with("---")
                    || line.starts_with("+++")
                    || line.starts_with("@@")
                    || (line.starts_with('+') && !line.starts_with("+++"))
                    || (line.starts_with('-') && !line.starts_with("---"))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(super) fn toggle_approval_metadata(&mut self) -> bool {
        if self.approval.pending.is_none() {
            return false;
        }
        self.approval.metadata_collapsed = !self.approval.metadata_collapsed;
        self.approval.scroll_back = 0;
        self.push_event(
            "approval:view",
            if self.approval.metadata_collapsed {
                "metadata collapsed"
            } else {
                "metadata expanded"
            },
        );
        true
    }

    pub(super) fn cycle_approval_diff_mode(&mut self) -> bool {
        if self.approval.pending.is_none() {
            return false;
        }
        self.approval.diff_mode = self.approval.diff_mode.next();
        self.approval.scroll_back = 0;
        self.push_event("approval:view", self.approval.diff_mode.label());
        true
    }

    pub(super) fn jump_approval_hunk(&mut self, next: bool) -> bool {
        let hunk_positions = self.approval_hunk_positions();
        if hunk_positions.is_empty() {
            return false;
        }
        let previous_index = self.approval.selected_hunk_index;
        let next_index = if next {
            (self.approval.selected_hunk_index + 1).min(hunk_positions.len() - 1)
        } else {
            self.approval.selected_hunk_index.saturating_sub(1)
        };
        if next_index == previous_index {
            return false;
        }
        self.approval.selected_hunk_index = next_index;
        self.approval.scroll_back = hunk_positions[self.approval.selected_hunk_index];
        true
    }

    pub(super) fn switch_approval_file(&mut self, next: bool) -> bool {
        let Some(preview) = self
            .approval
            .pending
            .as_ref()
            .and_then(|pending| pending.preview.as_ref())
        else {
            return false;
        };
        if preview.file_diffs.is_empty() {
            return false;
        }

        let previous_index = self.approval.selected_file_index;
        if next {
            self.approval.selected_file_index =
                (self.approval.selected_file_index + 1).min(preview.file_diffs.len() - 1);
        } else {
            self.approval.selected_file_index = self.approval.selected_file_index.saturating_sub(1);
        }
        self.approval.selected_hunk_index = 0;
        self.approval.scroll_back = 0;
        previous_index != self.approval.selected_file_index
    }
}

fn approval_access_label(spec: &sigil_kernel::ToolSpec) -> String {
    format!("{} {}", spec.category.as_str(), spec.access.as_str())
}

fn approval_permission_lines(
    operation: ToolOperation,
    risk: PermissionRisk,
    zones: &[PathTrustZone],
    confirmation: Option<&PermissionConfirmation>,
    snapshot_required: bool,
) -> Vec<String> {
    let mut lines = vec![
        format!("operation={}", approval_operation_label(operation)),
        format!("risk={}", approval_risk_label(risk)),
    ];
    if !zones.is_empty() {
        lines.push(format!(
            "path_zone={}",
            zones
                .iter()
                .map(|zone| approval_path_zone_label(*zone))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(confirmation) = confirmation {
        lines.push(format!(
            "confirmation={}",
            approval_confirmation_label(confirmation)
        ));
    }
    if snapshot_required {
        lines.push("recovery=pre-change snapshot required".to_owned());
    }
    lines
}

fn approval_operation_label(operation: ToolOperation) -> &'static str {
    match operation {
        ToolOperation::Read => "read",
        ToolOperation::Search => "search",
        ToolOperation::CreateFile => "create file",
        ToolOperation::EditFile => "edit file",
        ToolOperation::OverwriteFile => "overwrite file",
        ToolOperation::DeleteFile => "delete file",
        ToolOperation::RenamePath => "rename path",
        ToolOperation::CreateDirectory => "create directory",
        ToolOperation::DeleteDirectory => "delete directory",
        ToolOperation::RecursiveDelete => "recursive delete",
        ToolOperation::ApplyChangeSet => "apply change set",
        ToolOperation::ExecuteReadOnlyCommand => "run read-only command",
        ToolOperation::ExecuteMutatingCommand => "run mutating command",
        ToolOperation::ExecuteUnknownCommand => "run command",
        ToolOperation::ExecuteDestructiveCommand => "run destructive command",
        ToolOperation::SendTerminalInput => "send terminal input",
        ToolOperation::NetworkRequest => "network request",
        ToolOperation::SpawnAgent => "spawn agent",
        ToolOperation::MessageAgent => "message agent",
        ToolOperation::CloseAgent => "close agent",
        ToolOperation::LoadSkill => "load skill",
        ToolOperation::InvokePlugin => "invoke plugin",
    }
}

fn approval_risk_label(risk: PermissionRisk) -> &'static str {
    match risk {
        PermissionRisk::Low => "low",
        PermissionRisk::Medium => "medium",
        PermissionRisk::High => "high",
        PermissionRisk::Destructive => "destructive",
        PermissionRisk::Protected => "protected",
    }
}

fn approval_path_zone_label(zone: PathTrustZone) -> &'static str {
    match zone {
        PathTrustZone::WorkspaceSource => "workspace source",
        PathTrustZone::WorkspaceDocs => "workspace docs",
        PathTrustZone::WorkspaceProjectAsset => "project asset",
        PathTrustZone::WorkspaceRuntimeState => "runtime state",
        PathTrustZone::WorkspaceIgnored => "ignored file",
        PathTrustZone::WorkspaceGitMetadata => "git metadata",
        PathTrustZone::WorkspaceConfigSecret => "config or secret",
        PathTrustZone::UserState => "user state",
        PathTrustZone::UserCache => "user cache",
        PathTrustZone::External => "external path",
        PathTrustZone::Unknown => "unknown",
    }
}

fn approval_confirmation_label(confirmation: &PermissionConfirmation) -> &'static str {
    match confirmation {
        PermissionConfirmation::Standard => "standard approval",
        PermissionConfirmation::TypePath => "type the path before approval",
        PermissionConfirmation::TypePhrase { .. } => "type the requested phrase before approval",
    }
}

fn approval_subject_lines(subjects: &[sigil_kernel::ToolSubject]) -> Vec<String> {
    subjects
        .iter()
        .map(|subject| format!("subject={}", approval_subject_label(subject)))
        .collect()
}

fn approval_subject_summary(subjects: &[sigil_kernel::ToolSubject]) -> Option<String> {
    (!subjects.is_empty()).then(|| {
        subjects
            .iter()
            .map(approval_subject_label)
            .collect::<Vec<_>>()
            .join(", ")
    })
}

fn approval_subject_label(subject: &sigil_kernel::ToolSubject) -> String {
    let scope = subject.scope.as_str();
    let target = subject
        .canonical_path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| subject.normalized.clone());
    format!("{scope}:{}:{target}", subject.kind.as_str())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApprovalChangeSetFileMetadata {
    action: String,
    risk: String,
}

fn approval_changeset_summary(
    tool_name: &str,
    args_json: &str,
) -> Option<ApprovalChangeSetSummary> {
    if tool_name != "apply_changeset" {
        return None;
    }
    let args: Value = serde_json::from_str(args_json).ok()?;
    let files = args.get("files").and_then(Value::as_array)?;
    let paths = files
        .iter()
        .filter_map(|file| file.get("path").and_then(Value::as_str).map(str::to_owned))
        .collect::<Vec<_>>();
    Some(ApprovalChangeSetSummary {
        id: args
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        risk: normalize_changeset_label(
            args.get("risk").and_then(Value::as_str).unwrap_or("medium"),
        ),
        format_hint: approval_format_hint(&paths),
    })
}

fn approval_changeset_file_metadata(
    tool_name: &str,
    args_json: &str,
) -> BTreeMap<String, ApprovalChangeSetFileMetadata> {
    if tool_name != "apply_changeset" {
        return BTreeMap::new();
    }
    let Ok(args) = serde_json::from_str::<Value>(args_json) else {
        return BTreeMap::new();
    };
    let default_risk =
        normalize_changeset_label(args.get("risk").and_then(Value::as_str).unwrap_or("medium"));
    let Some(files) = args.get("files").and_then(Value::as_array) else {
        return BTreeMap::new();
    };

    files
        .iter()
        .filter_map(|file| {
            let path = file.get("path").and_then(Value::as_str)?;
            let action = file
                .get("action")
                .and_then(Value::as_str)
                .map(normalize_changeset_label)
                .unwrap_or_else(|| "change".to_owned());
            let risk = file
                .get("risk")
                .and_then(Value::as_str)
                .map(normalize_changeset_label)
                .unwrap_or_else(|| default_risk.clone());
            Some((
                normalize_approval_diagnostic_path(path),
                ApprovalChangeSetFileMetadata { action, risk },
            ))
        })
        .collect()
}

fn normalize_changeset_label(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('_', "-")
}

fn approval_format_hint(paths: &[String]) -> String {
    let has_rust = paths.iter().any(|path| path.ends_with(".rs"));
    let has_json = paths.iter().any(|path| path.ends_with(".json"));
    let has_markdown = paths
        .iter()
        .any(|path| path.ends_with(".md") || path.ends_with(".markdown"));
    let has_yaml = paths
        .iter()
        .any(|path| path.ends_with(".yml") || path.ends_with(".yaml"));

    let mut hints = Vec::new();
    if has_rust {
        hints.push("cargo fmt --all");
    }
    if has_json {
        hints.push("validate JSON formatting");
    }
    if has_yaml {
        hints.push("validate YAML formatting");
    }
    if has_markdown {
        hints.push("review Markdown rendering");
    }
    if hints.is_empty() {
        hints.push("run the relevant formatter before commit");
    }
    hints.join("; ")
}

fn approval_diff_line_kind(line: &str) -> ApprovalDiffLineKind {
    if line.starts_with("---")
        || line.starts_with("+++")
        || line.starts_with("diff ")
        || line.starts_with("index ")
    {
        ApprovalDiffLineKind::Header
    } else if line.starts_with("@@") {
        ApprovalDiffLineKind::Hunk
    } else if line.starts_with('+') && !line.starts_with("+++") {
        ApprovalDiffLineKind::Added
    } else if line.starts_with('-') && !line.starts_with("---") {
        ApprovalDiffLineKind::Removed
    } else {
        ApprovalDiffLineKind::Context
    }
}

fn normalize_approval_diagnostic_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_owned()
}

fn spawn_agent_background_args_json(tool_name: &str, args_json: &str) -> Option<String> {
    if tool_name != sigil_runtime::SPAWN_AGENT_TOOL_NAME {
        return None;
    }
    let mut args: Value = serde_json::from_str(args_json).ok()?;
    let object = args.as_object_mut()?;
    if object
        .get("mode")
        .and_then(Value::as_str)
        .is_some_and(|mode| mode == "background")
    {
        return None;
    }
    object.insert("mode".to_owned(), Value::String("background".to_owned()));
    serde_json::to_string(&args).ok()
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/approval_flow_detail_tests.rs"]
mod tests;
