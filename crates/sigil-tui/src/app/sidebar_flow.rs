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
                    if self.sidebar_agent_selected < last_index {
                        self.sidebar_agent_selected += 1;
                    } else {
                        self.sidebar_selected_card = SidebarCard::Review;
                    }
                } else if self.sidebar_agent_selected > 0 {
                    self.sidebar_agent_selected -= 1;
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
        if self.runtime.is_busy {
            self.last_notice = Some("busy; permission locked".to_owned());
            self.push_timeline(
                TimelineRole::Notice,
                "busy; permission mode stays unchanged",
            );
            return Ok(None);
        }
        let Some(root_config) = self.config_snapshot.as_ref() else {
            return Ok(None);
        };
        let mut next_config = root_config.clone();
        next_config.permission.mode = cycle_permission_mode(next_config.permission.mode);
        persisted_root_config(&next_config).save(&self.config_path)?;
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
        Ok(Some(AppAction::RuntimeConfigUpdated {
            root_config: Box::new(next_config),
        }))
    }
}
