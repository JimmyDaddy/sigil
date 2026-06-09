use super::*;

impl AppState {
    pub fn approval_preview_lines(&self) -> Vec<String> {
        let Some(pending) = &self.pending_approval else {
            return self.session_view_lines();
        };

        let mut lines = Vec::new();
        if let Some(preview) = &pending.preview {
            if !self.approval_metadata_collapsed {
                lines.push(format!(
                    "tool={}  id={}  mode={}",
                    pending.call.name,
                    pending.call.id,
                    approval_access_label(&pending.spec)
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
                    self.approval_selected_file_index
                        .min(preview.file_diffs.len() - 1)
                        + 1,
                    preview.file_diffs.len()
                ));
                for (index, file) in preview.file_diffs.iter().enumerate() {
                    let selected = if index == self.approval_selected_file_index {
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
            let active_hunk_line = match self.approval_diff_mode {
                ApprovalDiffMode::Full => hunk_positions
                    .get(self.approval_selected_hunk_index)
                    .copied()
                    .unwrap_or(usize::MAX),
                ApprovalDiffMode::CurrentHunk | ApprovalDiffMode::ChangedOnly => 0,
            };
            lines.push(format!(
                "mode={}  hunk {}/{}  [,] hunk  ,/. file  m meta  v view",
                self.approval_diff_mode.label(),
                if hunk_positions.is_empty() {
                    0
                } else {
                    self.approval_selected_hunk_index + 1
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
            lines.extend(approval_subject_lines(&pending.subjects));
            lines.push(format!("args={}", pending.call.args_json));
        }

        lines.push(String::new());
        lines.push("Y allow  N deny".to_owned());
        lines
    }

    pub(super) fn handle_pending_approval_key_event(
        &mut self,
        key: KeyEvent,
    ) -> Option<Option<AppAction>> {
        if let Some(pending) = &self.pending_approval {
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
                KeyCode::Enter if key.modifiers.is_empty() => {
                    return Some(Some(AppAction::ApprovalDecision {
                        call_id: pending.call.id.clone(),
                        approved: self.approval_selected_action.approved(),
                    }));
                }
                _ => {}
            }
        }

        if self.pending_approval.is_none() || key.modifiers.contains(KeyModifiers::CONTROL) {
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
                self.approval_metadata_collapsed = !self.approval_metadata_collapsed;
                self.approval_scroll_back = 0;
                self.push_event(
                    "approval:view",
                    if self.approval_metadata_collapsed {
                        "metadata collapsed"
                    } else {
                        "metadata expanded"
                    },
                );
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
                self.approval_diff_mode = self.approval_diff_mode.next();
                self.approval_scroll_back = 0;
                self.push_event("approval:view", self.approval_diff_mode.label());
                Some(None)
            }
            KeyCode::Left | KeyCode::Right => {
                self.approval_selected_action = self.approval_selected_action.toggled();
                self.push_event(
                    "approval:action",
                    if self.approval_selected_action.approved() {
                        "allow"
                    } else {
                        "deny"
                    },
                );
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
            KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Tab | KeyCode::BackTab => Some(None),
            _ => None,
        }
    }

    pub(crate) fn approval_modal_view(&self) -> Option<ApprovalModalView> {
        let pending = self.pending_approval.as_ref()?;
        let access_label = approval_access_label(&pending.spec);
        let Some(preview) = pending.preview.as_ref() else {
            return Some(ApprovalModalView {
                tool_name: pending.call.name.clone(),
                call_id: pending.call.id.clone(),
                access_label,
                preview_title: format!("Run {}", pending.call.name),
                preview_summary: approval_subject_summary(&pending.subjects)
                    .unwrap_or_else(|| "Tool preview unavailable for this call.".to_owned()),
                metadata_collapsed: self.approval_metadata_collapsed,
                file_rows: Vec::new(),
                changed_files: Vec::new(),
                diff_mode_label: self.approval_diff_mode.label(),
                active_hunk_index: 0,
                hunk_total: 0,
                diff_label: pending.call.name.clone(),
                diff_lines: vec![ApprovalDiffLine {
                    text: "No structured diff preview available.".to_owned(),
                    kind: ApprovalDiffLineKind::Context,
                    active_hunk: false,
                }],
                selected_action: self.approval_selected_action,
            });
        };

        let raw_diff = self
            .selected_approval_diff()
            .unwrap_or(preview.body.as_str());
        let transformed_diff = self.transform_approval_diff(raw_diff);
        let transformed_lines = transformed_diff.lines().collect::<Vec<_>>();
        let hunk_positions = self.approval_hunk_positions();
        let active_hunk_index = if hunk_positions.is_empty() {
            0
        } else {
            self.approval_selected_hunk_index
                .min(hunk_positions.len() - 1)
                + 1
        };
        let active_hunk_line = match self.approval_diff_mode {
            ApprovalDiffMode::Full => hunk_positions
                .get(self.approval_selected_hunk_index)
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
                    path: file.path.clone(),
                    selected: index == self.approval_selected_file_index,
                })
                .collect()
        } else {
            preview
                .changed_files
                .iter()
                .enumerate()
                .map(|(index, path)| ApprovalFileRow {
                    path: path.clone(),
                    selected: index == self.approval_selected_file_index,
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
            access_label,
            preview_title: preview.title.clone(),
            preview_summary: preview.summary.clone(),
            metadata_collapsed: self.approval_metadata_collapsed,
            file_rows,
            changed_files: preview.changed_files.clone(),
            diff_mode_label: self.approval_diff_mode.label(),
            active_hunk_index,
            hunk_total: hunk_positions.len(),
            diff_label,
            diff_lines,
            selected_action: self.approval_selected_action,
        })
    }

    fn selected_approval_diff(&self) -> Option<&str> {
        let preview = self
            .pending_approval
            .as_ref()
            .and_then(|pending| pending.preview.as_ref())?;
        preview
            .file_diffs
            .get(self.approval_selected_file_index)
            .map(|file| file.diff.as_str())
            .or_else(|| (!preview.body.is_empty()).then_some(preview.body.as_str()))
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
        match self.approval_diff_mode {
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
            .approval_selected_hunk_index
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

    pub(super) fn jump_approval_hunk(&mut self, next: bool) {
        let hunk_positions = self.approval_hunk_positions();
        if hunk_positions.is_empty() {
            return;
        }
        if next {
            self.approval_selected_hunk_index =
                (self.approval_selected_hunk_index + 1).min(hunk_positions.len() - 1);
        } else {
            self.approval_selected_hunk_index = self.approval_selected_hunk_index.saturating_sub(1);
        }
        self.approval_scroll_back = hunk_positions[self.approval_selected_hunk_index];
    }

    pub(super) fn switch_approval_file(&mut self, next: bool) {
        let Some(preview) = self
            .pending_approval
            .as_ref()
            .and_then(|pending| pending.preview.as_ref())
        else {
            return;
        };
        if preview.file_diffs.is_empty() {
            return;
        }

        if next {
            self.approval_selected_file_index =
                (self.approval_selected_file_index + 1).min(preview.file_diffs.len() - 1);
        } else {
            self.approval_selected_file_index = self.approval_selected_file_index.saturating_sub(1);
        }
        self.approval_selected_hunk_index = 0;
        self.approval_scroll_back = 0;
    }
}

fn approval_access_label(spec: &termquill_kernel::ToolSpec) -> String {
    format!("{} {}", spec.category.as_str(), spec.access.as_str())
}

fn approval_subject_lines(subjects: &[termquill_kernel::ToolSubject]) -> Vec<String> {
    subjects
        .iter()
        .map(|subject| format!("subject={}", approval_subject_label(subject)))
        .collect()
}

fn approval_subject_summary(subjects: &[termquill_kernel::ToolSubject]) -> Option<String> {
    (!subjects.is_empty()).then(|| {
        subjects
            .iter()
            .map(approval_subject_label)
            .collect::<Vec<_>>()
            .join(", ")
    })
}

fn approval_subject_label(subject: &termquill_kernel::ToolSubject) -> String {
    let scope = subject.scope.as_str();
    let target = subject
        .canonical_path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| subject.normalized.clone());
    format!("{scope}:{}:{target}", subject.kind.as_str())
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
