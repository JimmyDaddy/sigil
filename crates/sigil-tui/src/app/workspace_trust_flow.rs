use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sigil_kernel::{
    ControlEntry, JsonlSessionStore, SessionLogEntry, WorkspaceTrust, WorkspaceTrustDecisionEntry,
    stable_event_uuid, stable_workspace_id,
};

use super::{AppAction, AppState, TimelineRole};
use crate::workspace_trust::WorkspaceTrustGateState;

impl AppState {
    pub(crate) fn enter_workspace_trust_gate(&mut self) -> Result<()> {
        let workspace_id = stable_workspace_id(&self.workspace_root)?;
        self.workspace_trust_gate_state = Some(WorkspaceTrustGateState::new(workspace_id));
        self.bootstrap_workspace_trust_gate();
        Ok(())
    }

    pub(crate) fn workspace_trust_gate_lines(&self) -> Vec<String> {
        let workspace_id = self
            .workspace_trust_gate_state
            .as_ref()
            .map(|state| state.workspace_id.as_str())
            .unwrap_or("unknown");
        vec![
            "Workspace trust".to_owned(),
            "[workspace]".to_owned(),
            format!("path: {}", self.workspace_root.display()),
            format!("id: {workspace_id}"),
            String::new(),
            "[what this means]".to_owned(),
            "Trust this workspace before Sigil loads repo-local instructions or checks.".to_owned(),
            "Untrusted repositories stay data-only and cannot start normal agent execution."
                .to_owned(),
            String::new(),
            "Enter trust and continue  Ctrl-C quit".to_owned(),
        ]
    }

    pub(super) fn handle_workspace_trust_gate_key_event(
        &mut self,
        key: KeyEvent,
    ) -> Result<Option<AppAction>> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return Ok(None);
        }
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.last_notice = Some("trusting workspace".to_owned());
                Ok(Some(AppAction::TrustWorkspace))
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
                self.should_quit = true;
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    pub(crate) fn confirm_workspace_trust_gate(&mut self) -> Result<()> {
        self.ensure_current_workspace_trust_decision("trusted by user at workspace entry")?;
        self.workspace_trust_gate_state = None;
        self.last_notice = Some("workspace trusted".to_owned());
        self.bootstrap();
        Ok(())
    }

    pub(crate) fn ensure_current_workspace_trust_decision(&mut self, reason: &str) -> Result<()> {
        let workspace_id = stable_workspace_id(&self.workspace_root)?;
        if workspace_trust_from_entries(&self.session_browser.current_entries, &workspace_id)
            == Some(WorkspaceTrust::Trusted)
        {
            return Ok(());
        }
        let entry = self.workspace_trust_decision_entry(workspace_id, reason);
        let control = ControlEntry::WorkspaceTrustDecision(entry);
        let store = JsonlSessionStore::new(&self.session_log_path)?;
        store.append(&SessionLogEntry::Control(control.clone()))?;
        self.append_current_session_control(control);
        Ok(())
    }

    pub(crate) fn workspace_is_trusted_from_history(&self) -> bool {
        let Ok(workspace_id) = stable_workspace_id(&self.workspace_root) else {
            return false;
        };
        if workspace_trust_from_entries(&self.session_browser.current_entries, &workspace_id)
            == Some(WorkspaceTrust::Trusted)
        {
            return true;
        }
        self.session_browser.history.iter().any(|entry| {
            JsonlSessionStore::read_entries(&entry.path)
                .ok()
                .and_then(|entries| workspace_trust_from_entries(&entries, &workspace_id))
                == Some(WorkspaceTrust::Trusted)
        })
    }

    pub fn is_workspace_trust_gate_mode(&self) -> bool {
        self.workspace_trust_gate_state.is_some()
    }

    fn bootstrap_workspace_trust_gate(&mut self) {
        self.timeline.clear();
        self.tool_activity_cache.clear();
        self.tool_activity_visible_rows.clear();
        self.events.clear();
        self.ensure_scratch_dir();
        self.push_timeline(TimelineRole::System, "workspace trust");
        self.push_timeline(
            TimelineRole::Notice,
            "trust this workspace before using sigil",
        );
        self.push_event("mode", "workspace_trust");
        self.push_event("workspace", self.workspace_root.display().to_string());
        self.push_event("session_log", self.session_log_path.display().to_string());
        self.reset_scroll();
    }

    fn workspace_trust_decision_entry(
        &self,
        workspace_id: String,
        reason: &str,
    ) -> WorkspaceTrustDecisionEntry {
        let seed = format!(
            "{}:{}:{}",
            workspace_id,
            self.session_id,
            self.session_browser.current_entries.len()
        );
        let event_id = stable_event_uuid("sigil.workspace_trust", &seed);
        WorkspaceTrustDecisionEntry {
            workspace_id,
            workspace_trust_snapshot_id: format!("workspace-trust:{event_id}"),
            trust: WorkspaceTrust::Trusted,
            decided_by_event_id: Some(event_id),
            reason: Some(reason.to_owned()),
        }
    }
}

fn workspace_trust_from_entries(
    entries: &[SessionLogEntry],
    workspace_id: &str,
) -> Option<WorkspaceTrust> {
    entries.iter().rev().find_map(|entry| match entry {
        SessionLogEntry::Control(ControlEntry::WorkspaceTrustDecision(decision))
            if decision.workspace_id == workspace_id =>
        {
            Some(decision.trust)
        }
        _ => None,
    })
}
