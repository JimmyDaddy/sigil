use anyhow::Result;

use super::{
    AppAction, AppState, SidebarCard, TimelineRole, config_flow::cycle_permission_mode,
    formatting::persisted_root_config,
};

impl AppState {
    pub(super) fn move_sidebar_selection(&mut self, next: bool) {
        match self.sidebar_selected_card {
            SidebarCard::Permission => {
                self.sidebar_selected_card = if next {
                    self.sidebar_selected_card.next()
                } else {
                    self.sidebar_selected_card.previous()
                };
            }
            SidebarCard::Agents => {
                let last_index = self.agent_sidebar_rows().len().saturating_sub(1);
                if next {
                    if self.agent_panel.selected < last_index {
                        self.agent_panel.selected += 1;
                    } else {
                        self.sidebar_selected_card = SidebarCard::Review;
                    }
                } else if self.agent_panel.selected > 0 {
                    self.agent_panel.selected -= 1;
                } else {
                    self.sidebar_selected_card = SidebarCard::Permission;
                }
            }
            SidebarCard::Review | SidebarCard::Usage => {
                self.sidebar_selected_card = if next {
                    self.sidebar_selected_card.next()
                } else {
                    self.sidebar_selected_card.previous()
                };
            }
        }
    }

    pub(super) fn toggle_runtime_permission_mode(&mut self) -> Result<Option<AppAction>> {
        let Some(root_config) = self.config_snapshot.as_ref() else {
            return Ok(None);
        };
        let mut next_config = root_config.clone();
        next_config.permission.mode = cycle_permission_mode(next_config.permission.mode);
        persisted_root_config(&next_config)
            .save_if_unchanged(&self.config_path, &persisted_root_config(root_config))?;
        self.apply_runtime_config_snapshot(&next_config);
        self.last_notice = Some(format!(
            "permission mode = {}",
            next_config.permission.mode.as_str()
        ));
        self.push_event("permission_mode", self.runtime.permission_mode.clone());
        self.push_timeline(
            TimelineRole::Notice,
            format!("permission mode -> {}", self.runtime.permission_mode),
        );
        self.schedule_balance_refresh();
        if self.runtime.is_busy {
            // During an active run the worker must not restart; switch the shared mode override
            // so the next permission decision uses the new mode immediately.
            return Ok(Some(AppAction::UpdateActiveRunPermissionMode {
                mode: next_config.permission.mode,
            }));
        }
        Ok(Some(AppAction::RuntimeConfigUpdated {
            root_config: Box::new(next_config),
        }))
    }
}
