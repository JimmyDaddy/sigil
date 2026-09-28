use std::{collections::BTreeSet, sync::mpsc, thread::JoinHandle};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sigil_runtime::mcp_import::{McpConfigurationImport, McpImportIssue};

use super::{AppState, ModalState, modal_flow::TextInputTarget};
use crate::config_panel::ConfigDraftBinding;

pub(super) struct McpImportModalState {
    request_id: u64,
    draft_binding: ConfigDraftBinding,
    preview: Option<McpConfigurationImport>,
    selected: BTreeSet<usize>,
    focused: usize,
    error: Option<String>,
}

impl std::fmt::Debug for McpImportModalState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpImportModalState")
            .field("request_id", &self.request_id)
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

pub(super) struct McpImportTask {
    request_id: u64,
    receiver: mpsc::Receiver<Result<McpConfigurationImport, String>>,
    handle: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for McpImportTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpImportTask")
            .field("request_id", &self.request_id)
            .finish_non_exhaustive()
    }
}

impl Drop for McpImportTask {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl McpImportModalState {
    pub(super) fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            "Choose MCP servers to add to the settings draft.".to_owned(),
            "Nothing starts during import. Save settings afterwards; activation stays explicit."
                .to_owned(),
            "Inline environment/header values are omitted. Command arguments WILL be saved."
                .to_owned(),
            "↑/↓ choose · Space select · Enter add selected · Esc cancel".to_owned(),
            String::new(),
        ];
        match &self.preview {
            None if self.error.is_none() => lines.push("Reading the selected file…".to_owned()),
            None => {}
            Some(preview) if preview.summaries().is_empty() => {
                lines.push("No MCP servers in this document.".to_owned())
            }
            Some(preview) => {
                for (index, item) in preview
                    .summaries()
                    .iter()
                    .enumerate()
                    .skip(self.focused.saturating_sub(3))
                    .take(7)
                {
                    lines.push(format!(
                        "{} [{}] {} · {}{}",
                        if index == self.focused { "›" } else { " " },
                        if self.selected.contains(&item.index) {
                            "x"
                        } else {
                            " "
                        },
                        item.name,
                        item.transport.as_deref().unwrap_or("unknown transport"),
                        if item.importable {
                            ""
                        } else {
                            " · unavailable"
                        },
                    ));
                }
                if let Some(item) = preview.summaries().get(self.focused) {
                    if !item.description.is_empty() {
                        lines.push(item.description.clone());
                    }
                    for issue in &item.issues {
                        lines.push(issue_description(*issue).to_owned());
                    }
                }
                if preview.root_fields_ignored() {
                    lines.push("Other root configuration fields are ignored.".to_owned());
                }
            }
        }
        if let Some(error) = &self.error {
            lines.push(format!("Import unavailable: {error}"));
        }
        lines
    }
}

fn issue_description(issue: McpImportIssue) -> &'static str {
    match issue {
        McpImportIssue::InvalidServerObject => "This server entry is not an object.",
        McpImportIssue::InvalidConfiguration => "This server configuration is invalid.",
        McpImportIssue::AmbiguousTransport => "Choose one transport in the source file.",
        McpImportIssue::UnsupportedTransport => "This transport is not supported.",
        McpImportIssue::IgnoredFields => "Unsupported server fields are ignored.",
        McpImportIssue::EnvironmentValuesNotImported => {
            "Inline environment values are not imported."
        }
        McpImportIssue::HeaderValuesNotImported => "Inline header values are not imported.",
        McpImportIssue::CommandArgumentsWillBeSaved => {
            "Command and arguments will be saved; check the source for embedded secrets."
        }
    }
}

impl AppState {
    pub(super) fn open_mcp_import_path(&mut self) {
        if self.config_state.is_some() {
            self.open_text_input(TextInputTarget::McpImportPath, "");
        }
    }

    pub(super) fn start_mcp_import_preview(&mut self, value: String) {
        let Some(config) = self.config_state.as_ref() else {
            return;
        };
        if self.mcp_import_task.is_some() {
            self.last_notice = Some("the previous import file is still being read".to_owned());
            return;
        }
        let draft_binding = config.save_binding();
        let path = std::path::PathBuf::from(value.trim());
        if path.as_os_str().is_empty() {
            self.last_notice = Some("choose an MCP JSON file path".to_owned());
            return;
        }
        let path = if path.is_absolute() {
            path
        } else {
            self.workspace_root.join(path)
        };
        let request_id = self.next_background_request_id();
        let (sender, receiver) = mpsc::channel();
        let handle = std::thread::Builder::new()
            .name("sigil-mcp-import-preview".to_owned())
            .spawn(move || {
                let result =
                    sigil_runtime::mcp_import::preview_mcp_configuration_import_file(&path)
                        .map_err(|error| error.to_string());
                let _ = sender.send(result);
            });
        match handle {
            Ok(handle) => {
                self.mcp_import_task = Some(McpImportTask {
                    request_id,
                    receiver,
                    handle: Some(handle),
                });
                self.modal_state = Some(ModalState::McpImport(Box::new(McpImportModalState {
                    request_id,
                    draft_binding,
                    preview: None,
                    selected: BTreeSet::new(),
                    focused: 0,
                    error: None,
                })));
            }
            Err(_) => self.last_notice = Some("could not start the MCP import reader".to_owned()),
        }
    }

    pub(super) fn poll_mcp_import(&mut self) -> bool {
        let Some(task) = self.mcp_import_task.as_ref() else {
            return false;
        };
        if task
            .handle
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return false;
        }
        let Some(task) = self.mcp_import_task.take() else {
            return false;
        };
        let result = task.receiver.try_recv().unwrap_or_else(|_| {
            Err("MCP import reader stopped before producing a preview".to_owned())
        });
        if let Some(ModalState::McpImport(state)) = self.modal_state.as_mut()
            && state.request_id == task.request_id
            && self
                .config_state
                .as_ref()
                .is_some_and(|config| config.save_binding().same_panel(state.draft_binding))
        {
            match result {
                Ok(preview) => state.preview = Some(preview),
                Err(error) => state.error = Some(error),
            }
        }
        // The reader remains owned even when the preview was dismissed.
        drop(task);
        true
    }

    pub(crate) fn mcp_import_modal_open(&self) -> bool {
        matches!(self.modal_state, Some(ModalState::McpImport(_)))
    }

    pub(super) fn handle_mcp_import_key_event(&mut self, key: KeyEvent) {
        if key.modifiers != KeyModifiers::NONE {
            return;
        }
        if key.code == KeyCode::Esc {
            self.modal_state = None;
            self.last_notice = Some("MCP import cancelled; settings draft kept".to_owned());
            return;
        }
        let Some(ModalState::McpImport(state)) = self.modal_state.as_mut() else {
            return;
        };
        let Some(preview) = state.preview.as_ref() else {
            return;
        };
        match key.code {
            KeyCode::Up => state.focused = state.focused.saturating_sub(1),
            KeyCode::Down => {
                state.focused = state
                    .focused
                    .saturating_add(1)
                    .min(preview.summaries().len().saturating_sub(1))
            }
            KeyCode::Char(' ') => {
                if let Some(item) = preview
                    .summaries()
                    .get(state.focused)
                    .filter(|item| item.importable)
                    && !state.selected.remove(&item.index)
                {
                    state.selected.insert(item.index);
                }
            }
            KeyCode::Enter => {
                if state.selected.is_empty() {
                    state.error = Some("select at least one valid server".to_owned());
                    return;
                }
                let Some(config) = self
                    .config_state
                    .as_mut()
                    .filter(|config| config.save_binding().same_panel(state.draft_binding))
                else {
                    state.error = Some("the settings draft changed; reopen import".to_owned());
                    return;
                };
                // Only names participate in conflict detection. Preserve existing draft edits,
                // including temporarily incomplete fields, until the usual save validation.
                let existing = config
                    .draft
                    .mcp_servers
                    .iter()
                    .map(|draft| {
                        let mut server = draft.base_config.clone();
                        server.name = draft.name.trim().to_owned();
                        server
                    })
                    .collect::<Vec<_>>();
                match preview.selected_configurations(
                    &state.selected.iter().copied().collect::<Vec<_>>(),
                    &existing,
                ) {
                    Ok(servers) => {
                        config.draft.mcp_servers.extend(
                            servers
                                .iter()
                                .skip(existing.len())
                                .map(crate::config_panel::McpServerDraft::from_config),
                        );
                        config.mark_dirty();
                        self.modal_state = None;
                        self.last_notice = Some(
                            "MCP servers added to draft; Ctrl-S saves, activation stays explicit"
                                .to_owned(),
                        );
                    }
                    Err(error) => state.error = Some(error.to_string()),
                }
            }
            _ => {}
        }
    }
}
