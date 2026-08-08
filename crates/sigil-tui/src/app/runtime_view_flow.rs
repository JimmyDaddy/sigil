use sigil_kernel::{RootConfig, TerminalKeyboardEnhancement};
use sigil_runtime::support::SupportBuildInfo;

use super::{
    AppState, ComposerMode, EGRESS_DISCLOSURE_HEIGHT, formatting::sidebar_width_for_terminal,
};

impl AppState {
    pub(crate) fn set_support_build_info(&mut self, build_info: SupportBuildInfo) {
        self.support_build_info = build_info;
    }

    pub(crate) fn support_build_info(&self) -> &SupportBuildInfo {
        &self.support_build_info
    }

    pub fn cache_hit_ratio(&self) -> f64 {
        let total = self.runtime.stats.cache_hit_tokens + self.runtime.stats.cache_miss_tokens;
        if total == 0 {
            0.0
        } else {
            self.runtime.stats.cache_hit_tokens as f64 / total as f64
        }
    }

    pub fn last_notice(&self) -> Option<&str> {
        self.last_notice.as_deref()
    }

    pub fn terminal_mouse_capture_enabled(&self) -> bool {
        self.config_snapshot
            .as_ref()
            .is_some_and(|config| config.terminal.mouse_capture)
    }

    pub fn terminal_keyboard_enhancement_enabled(&self) -> bool {
        self.terminal_keyboard_enhancement_enabled
    }

    pub fn set_terminal_keyboard_enhancement_enabled(&mut self, enabled: bool) {
        self.terminal_keyboard_enhancement_enabled = enabled;
    }

    pub fn terminal_keyboard_enhancement_policy(&self) -> TerminalKeyboardEnhancement {
        self.config_snapshot
            .as_ref()
            .map(|config| config.terminal.keyboard_enhancement)
            .unwrap_or(TerminalKeyboardEnhancement::Off)
    }

    pub fn terminal_osc52_clipboard_enabled(&self) -> bool {
        self.config_snapshot
            .as_ref()
            .is_none_or(|config| config.terminal.osc52_clipboard)
    }

    pub fn terminal_scroll_sensitivity(&self) -> usize {
        self.config_snapshot
            .as_ref()
            .map(|config| config.terminal.scroll_sensitivity as usize)
            .unwrap_or(sigil_kernel::config::DEFAULT_TERMINAL_SCROLL_SENSITIVITY as usize)
            .max(1)
    }

    #[cfg(not(test))]
    pub(crate) fn terminal_notification_config(&self) -> sigil_kernel::TerminalNotificationConfig {
        self.config_snapshot
            .as_ref()
            .map(|config| config.terminal.notifications.clone())
            .unwrap_or_default()
    }

    pub(crate) fn root_config_snapshot(&self) -> Option<&RootConfig> {
        self.config_snapshot.as_ref()
    }

    pub(crate) fn pending_session_route_recovery_binding(&self) -> Option<&str> {
        self.runtime
            .pending_session_route_recovery_binding
            .as_deref()
    }

    pub(crate) fn pending_session_attachment_recovery_binding_for(
        &self,
        session_log_path: &std::path::Path,
    ) -> Option<&str> {
        (self.runtime.pending_session_route_recovery_code
            == Some(sigil_kernel::PublicRouteRecoveryCode::SessionAlreadyActive)
            && self
                .runtime
                .pending_session_route_recovery_target
                .as_deref()
                == Some(session_log_path))
        .then(|| self.pending_session_route_recovery_binding())
        .flatten()
    }

    pub(crate) fn mark_pending_session_transition_target(
        &mut self,
        session_log_path: std::path::PathBuf,
    ) {
        self.runtime.pending_session_route_recovery_target = Some(session_log_path);
    }

    pub(crate) fn pending_session_route_confirmation_binding(&self) -> Option<&str> {
        (self.runtime.pending_session_route_recovery_code
            != Some(sigil_kernel::PublicRouteRecoveryCode::SessionAlreadyActive))
        .then(|| self.pending_session_route_recovery_binding())
        .flatten()
    }

    pub(crate) fn pending_session_route_selection(
        &self,
    ) -> Option<&(String, sigil_kernel::ResolvedModelRoute)> {
        self.runtime.pending_session_route_selection.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn set_pending_session_route_recovery_binding(&mut self, binding: Option<String>) {
        self.runtime.pending_session_route_recovery_binding = binding;
    }

    pub(crate) fn set_pending_session_route_recovery(
        &mut self,
        code: sigil_kernel::PublicRouteRecoveryCode,
        binding: String,
    ) {
        self.runtime.pending_session_route_recovery_code = Some(code);
        if code == sigil_kernel::PublicRouteRecoveryCode::SessionAlreadyActive {
            if self.runtime.pending_session_route_recovery_target.is_none() {
                self.runtime.pending_session_route_recovery_target =
                    Some(self.session_log_path.clone());
            }
        } else {
            self.runtime.pending_session_route_recovery_target = None;
        }
        self.runtime.pending_session_route_recovery_binding = Some(binding);
    }

    pub(crate) fn mark_pending_session_route_selection(
        &mut self,
        provider_name: String,
        route: sigil_kernel::ResolvedModelRoute,
    ) {
        self.runtime.pending_session_route_selection = Some((provider_name, route));
    }

    pub(crate) fn clear_pending_session_route_startup(&mut self) {
        self.runtime.pending_session_route_recovery_binding = None;
        self.runtime.pending_session_route_recovery_code = None;
        self.runtime.pending_session_route_recovery_target = None;
        self.runtime.pending_session_route_selection = None;
    }

    pub(crate) fn runtime_config_for_current_session(
        &self,
        mut saved_config: RootConfig,
    ) -> anyhow::Result<Option<RootConfig>> {
        let Some(route) = self.runtime.model_route.as_ref() else {
            return Ok(None);
        };
        sigil_runtime::provider_connections::validate_persisted_model_route(&saved_config, route)
            .map_err(anyhow::Error::new)?;
        saved_config.agent.runtime_provider.clear();
        saved_config.agent.connection = Some(route.model_ref.connection_id.clone());
        saved_config.agent.model = route.model_ref.model_id.clone();
        Ok(Some(saved_config))
    }

    pub(crate) fn record_started_model_route(&mut self) {
        let Some(root_config) = self.config_snapshot.as_ref() else {
            return;
        };
        let Some(model_ref) = self
            .runtime
            .model_route
            .as_ref()
            .map(|route| route.model_ref.clone())
        else {
            return;
        };
        if let Err(error) = sigil_runtime::provider_connections::record_recent_model_ref(
            &self.sigil_paths.state_root,
            root_config,
            &model_ref,
        ) {
            self.push_event(
                "model_recent",
                format!(
                    "not recorded: {}",
                    sigil_kernel::safe_persistence_text(&error.to_string())
                ),
            );
            return;
        }
        self.recent_model_refs
            .retain(|candidate| candidate != &model_ref);
        self.recent_model_refs.insert(0, model_ref);
        self.recent_model_refs.truncate(20);
    }

    pub(crate) fn apply_saved_default_model(&mut self, root_config: RootConfig) {
        let default = sigil_runtime::provider_connections::load_provider_connections(&root_config)
            .default_model;
        self.schedule_connection_inventory_refresh(&root_config);
        self.config_snapshot = Some(root_config);
        if let Some(default) = default {
            let notice = format!(
                "saved default -> {}/{}; current session unchanged",
                default.connection_id, default.model_id
            );
            self.last_notice = Some(notice.clone());
            self.push_timeline(super::TimelineRole::Notice, notice);
        }
    }

    pub fn set_terminal_size(&mut self, width: u16, height: u16) -> bool {
        let next_width = width.max(3);
        let next_height = height.max(8);
        let height_changed = self.terminal_height != next_height;
        let width_changed = self.terminal_width != next_width;
        self.terminal_width = next_width;
        self.terminal_height = next_height;
        self.clamp_input_cursor();
        if width_changed {
            self.rebuild_timeline_render_surfaces();
        }
        self.timeline_scroll_back = self
            .timeline_scroll_back
            .min(self.max_timeline_scroll_back());
        width_changed || height_changed
    }

    pub(crate) fn footer_strip_height(&self) -> u16 {
        let desired = self
            .composer_height()
            .saturating_add(self.composer_agent_panel_rows());
        let minimum_live_content_height = self.minimum_live_panel_content_height();
        let sidebar_width = if self.info_rail_visible() {
            sidebar_width_for_terminal(self.terminal_width as usize) as u16
        } else {
            0
        };
        let live_panel_width = self.terminal_width.saturating_sub(sidebar_width);
        let disclosure_height = self.egress_disclosure_reserved_rows(
            live_panel_width,
            EGRESS_DISCLOSURE_HEIGHT.saturating_add(minimum_live_content_height),
        );
        let minimum_live_panel_height =
            minimum_live_content_height.saturating_add(disclosure_height);
        let maximum_footer_strip = self
            .terminal_height
            .saturating_sub(1)
            .saturating_sub(minimum_live_panel_height);
        desired.min(maximum_footer_strip)
    }

    pub(crate) fn minimum_live_panel_content_height(&self) -> u16 {
        let actionable_status = self.composer.pending_plan_approval.is_some()
            || self.is_composer_queue_panel_focused()
            || self.verification_card_focused();
        if actionable_status { 4 } else { 1 }
    }

    pub(crate) fn composer_mode_label(&self) -> &'static str {
        if self.composer.pending_plan_approval.is_some() {
            return ComposerMode::Plan.label();
        }
        self.composer.mode.label()
    }

    pub(crate) fn reasoning_effort_label(&self) -> &'static str {
        self.runtime.reasoning_effort.as_str()
    }

    pub(crate) fn info_rail_detail_enabled(&self) -> bool {
        self.info_rail_detail
    }

    pub(crate) fn info_rail_visible(&self) -> bool {
        self.info_rail_visible
    }

    pub(crate) fn toggle_info_rail_visibility(&mut self) {
        self.info_rail_visible = !self.info_rail_visible;
        self.rebuild_timeline_render_surfaces();
        self.timeline_scroll_back = self
            .timeline_scroll_back
            .min(self.max_timeline_scroll_back());
        self.clamp_input_cursor();
        let (mode, notice) = if !self.info_rail_visible {
            ("hidden", "info rail: hidden")
        } else if sidebar_width_for_terminal(self.terminal_width.into()) == 0 {
            (
                "enabled_width_limited",
                "info rail: enabled; hidden at current width",
            )
        } else {
            ("shown", "info rail: shown")
        };
        self.last_notice = Some(notice.to_owned());
        self.push_event("info_rail", mode);
    }

    pub(crate) fn toggle_info_rail_detail(&mut self) {
        self.info_rail_detail = !self.info_rail_detail;
        let mode = if self.info_rail_detail {
            "detail"
        } else {
            "compact"
        };
        self.last_notice = Some(format!("info rail: {mode}"));
        self.push_event("info_rail", mode);
    }

    pub(crate) fn permission_card_lines(&self) -> Vec<String> {
        vec![
            format!("mode: {}", self.runtime.permission_mode),
            "Shift-Tab cycle + save".to_owned(),
            if self.runtime.is_busy {
                "busy: locked during run".to_owned()
            } else {
                "scope: saved default".to_owned()
            },
        ]
    }

    pub fn is_setup_mode(&self) -> bool {
        self.setup_state.is_some()
    }

    pub fn is_config_mode(&self) -> bool {
        self.config_state.is_some()
    }

    pub fn has_modal(&self) -> bool {
        self.modal_state.is_some()
    }
}
