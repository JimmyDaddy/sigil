mod builtin;
#[allow(dead_code)]
pub(crate) mod contrast;
mod overrides;
mod palette;
pub(crate) mod styles;

use anyhow::Result;
use ratatui::style::Color;
use sigil_kernel::{AppearanceConfig, ThemeId};

use crate::app::AppState;
#[cfg(test)]
use crate::app::RunPhase;

#[allow(unused_imports)]
pub(crate) use overrides::COLOR_TOKEN_NAMES;
pub(crate) use palette::ThemePalette;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Theme {
    pub(crate) id: ThemeId,
    pub(crate) palette: ThemePalette,
}

impl Theme {
    pub(crate) fn try_from_config(appearance: &AppearanceConfig) -> Result<Self> {
        let mut palette = builtin::palette_for(appearance.theme);
        overrides::apply_overrides(&mut palette, &appearance.colors)?;
        Ok(Self {
            id: appearance.theme,
            palette,
        })
    }

    pub(crate) fn from_config_lossy(appearance: &AppearanceConfig) -> Self {
        Self::try_from_config(appearance).unwrap_or_else(|_| Self::default())
    }

    pub(crate) fn builtin(id: ThemeId) -> Self {
        Self {
            id,
            palette: builtin::palette_for(id),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::builtin(ThemeId::SigilDark)
    }
}

pub(crate) fn default_palette() -> ThemePalette {
    builtin::palette_for(ThemeId::SigilDark)
}

pub(crate) fn resolve_for_app(app: &AppState) -> Theme {
    if let Some(appearance) = app.config_preview_appearance() {
        return Theme::from_config_lossy(&appearance);
    }
    app.root_config_snapshot()
        .map(|config| Theme::from_config_lossy(&config.appearance))
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) fn shell_bg() -> Color {
    default_palette().surface_base
}

#[allow(dead_code)]
pub(crate) fn rail_bg() -> Color {
    default_palette().surface_rail
}

#[cfg(test)]
pub(crate) fn composer_bg() -> Color {
    default_palette().surface_panel
}

#[allow(dead_code)]
pub(crate) fn composer_input_bg() -> Color {
    default_palette().surface_input
}

#[allow(dead_code)]
pub(crate) fn agent_panel_bg() -> Color {
    default_palette().surface_agent_panel
}

#[allow(dead_code)]
pub(crate) fn status_band_bg() -> Color {
    default_palette().surface_panel_alt
}

#[allow(dead_code)]
pub(crate) fn dock_edge() -> Color {
    default_palette().border_subtle
}

#[allow(dead_code)]
pub(crate) fn selector_bg() -> Color {
    default_palette().surface_overlay
}

#[allow(dead_code)]
pub(crate) fn selector_shadow_bg() -> Color {
    default_palette().surface_overlay_shadow
}

#[allow(dead_code)]
pub(crate) fn selector_accent() -> Color {
    default_palette().selection_bg
}

#[cfg(test)]
pub(crate) fn user_message_bg() -> Color {
    default_palette().surface_user_message
}

#[allow(dead_code)]
pub(crate) fn ink() -> Color {
    default_palette().text_primary
}

#[allow(dead_code)]
pub(crate) fn muted() -> Color {
    default_palette().text_secondary
}

pub(crate) fn dim() -> Color {
    default_palette().text_muted
}

#[allow(dead_code)]
pub(crate) fn accent_teal() -> Color {
    default_palette().accent_secondary
}

#[allow(dead_code)]
pub(crate) fn accent_blue() -> Color {
    default_palette().accent_info
}

#[allow(dead_code)]
pub(crate) fn accent_gold() -> Color {
    default_palette().accent_warning
}

#[allow(dead_code)]
pub(crate) fn accent_lime() -> Color {
    default_palette().accent_primary
}

#[allow(dead_code)]
pub(crate) fn accent_rose() -> Color {
    default_palette().accent_danger
}

pub(crate) fn badge_bg() -> Color {
    default_palette().surface_badge
}

#[allow(dead_code)]
pub(crate) fn config_panel_bg() -> Color {
    default_palette().config_bg
}

#[allow(dead_code)]
pub(crate) fn config_border() -> Color {
    default_palette().config_border
}

#[allow(dead_code)]
pub(crate) fn config_primary() -> Color {
    default_palette().config_primary
}

#[allow(dead_code)]
pub(crate) fn config_detail() -> Color {
    default_palette().config_detail
}

#[allow(dead_code)]
pub(crate) fn config_warning() -> Color {
    default_palette().config_warning
}

#[allow(dead_code)]
pub(crate) fn config_danger() -> Color {
    default_palette().config_danger
}

#[allow(dead_code)]
pub(crate) fn config_tab_bg() -> Color {
    default_palette().config_tab_bg
}

#[allow(dead_code)]
pub(crate) fn config_section_bg() -> Color {
    default_palette().config_section_bg
}

#[allow(dead_code)]
pub(crate) fn config_selected_bg() -> Color {
    default_palette().config_selected_bg
}

#[cfg(test)]
pub(crate) fn phase_accent(phase: &RunPhase) -> Color {
    default_palette().phase_accent(phase)
}

#[cfg(test)]
#[path = "../tests/theme_tests.rs"]
mod tests;
