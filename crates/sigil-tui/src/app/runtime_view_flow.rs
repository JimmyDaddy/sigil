use sigil_kernel::{RootConfig, TerminalKeyboardEnhancement};
use sigil_runtime::support::SupportBuildInfo;

use super::{AppState, ComposerMode, formatting::sidebar_width_for_terminal};

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

    pub fn set_terminal_size(&mut self, width: u16, height: u16) -> bool {
        let next_width = width.max(3);
        let next_height = height.max(8);
        let height_changed = self.terminal_height != next_height;
        let width_changed = self.terminal_width != next_width;
        self.terminal_width = next_width;
        self.terminal_height = next_height;
        self.clamp_input_cursor();
        if width_changed {
            self.rebuild_timeline_render_store();
            self.rerender_active_agent_child_transcript();
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
        desired.min(self.terminal_height.saturating_sub(2).max(4))
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
