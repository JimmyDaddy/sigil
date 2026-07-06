use anyhow::{Result, bail};
use sigil_kernel::{RootConfig, SyntaxThemeId};

use super::ConfigDraft;
use crate::ui::theme::{COLOR_TOKEN_GROUPS, COLOR_TOKEN_NAMES, ColorTokenGroup};

impl ConfigDraft {
    pub(crate) fn selected_appearance_color_token(&self) -> &'static str {
        COLOR_TOKEN_NAMES[self
            .appearance_color_token_index
            .min(COLOR_TOKEN_NAMES.len() - 1)]
    }

    pub(crate) fn selected_appearance_color_group(&self) -> ColorTokenGroup {
        COLOR_TOKEN_GROUPS[self
            .appearance_color_group_index
            .min(COLOR_TOKEN_GROUPS.len() - 1)]
    }

    pub(crate) fn cycle_appearance_syntax_theme(&mut self) {
        self.appearance_syntax_theme = self.appearance_syntax_theme.next();
    }

    pub(crate) fn cycle_appearance_usage_cost_currency(&mut self) {
        self.appearance_usage_cost_currency = self.appearance_usage_cost_currency.next();
    }

    pub(crate) fn resolved_appearance_syntax_theme(&self) -> SyntaxThemeId {
        self.appearance_syntax_theme
            .resolved_for_theme(self.appearance_theme)
    }

    pub(crate) fn cycle_appearance_color_group(&mut self, forward: bool) {
        let len = COLOR_TOKEN_GROUPS.len();
        if forward {
            self.appearance_color_group_index = (self.appearance_color_group_index + 1) % len;
        } else if self.appearance_color_group_index == 0 {
            self.appearance_color_group_index = len - 1;
        } else {
            self.appearance_color_group_index -= 1;
        }
        self.appearance_color_token_index = COLOR_TOKEN_NAMES
            .iter()
            .position(|token| {
                self.selected_appearance_color_group()
                    .tokens
                    .contains(token)
            })
            .unwrap_or(0);
    }

    pub(crate) fn cycle_appearance_color_token(&mut self, forward: bool) {
        let group = self.selected_appearance_color_group();
        let tokens = group.tokens;
        let current_group_index = tokens
            .iter()
            .position(|token| *token == self.selected_appearance_color_token())
            .unwrap_or(0);
        let next_group_index = if forward {
            (current_group_index + 1) % tokens.len()
        } else if current_group_index == 0 {
            tokens.len() - 1
        } else {
            current_group_index - 1
        };
        let next_token = tokens[next_group_index];
        self.appearance_color_token_index = COLOR_TOKEN_NAMES
            .iter()
            .position(|token| *token == next_token)
            .unwrap_or(0);
    }

    #[cfg(test)]
    pub(crate) fn cycle_all_appearance_color_tokens(&mut self, forward: bool) {
        let len = COLOR_TOKEN_NAMES.len();
        if forward {
            self.appearance_color_token_index = (self.appearance_color_token_index + 1) % len;
        } else if self.appearance_color_token_index == 0 {
            self.appearance_color_token_index = len - 1;
        } else {
            self.appearance_color_token_index -= 1;
        }
        self.appearance_color_group_index =
            appearance_color_group_index_for_token(self.selected_appearance_color_token())
                .unwrap_or(0);
    }

    pub(crate) fn selected_appearance_color_override(&self) -> Option<&str> {
        self.base_root_config
            .appearance
            .colors
            .get(self.selected_appearance_color_token())
    }

    pub(crate) fn set_selected_appearance_color_override(&mut self, value: String) -> Result<bool> {
        let token = self.selected_appearance_color_token();
        let value = value.trim();
        if value.is_empty() {
            return Ok(self.reset_selected_appearance_color_override());
        }
        let normalized = normalize_hex_color_override(value)?;
        let changed =
            self.base_root_config.appearance.colors.get(token) != Some(normalized.as_str());
        if changed {
            self.base_root_config
                .appearance
                .colors
                .insert(token.to_owned(), normalized);
        }
        Ok(changed)
    }

    pub(crate) fn reset_selected_appearance_color_override(&mut self) -> bool {
        let token = self.selected_appearance_color_token();
        self.base_root_config
            .appearance
            .colors
            .remove(token)
            .is_some()
    }

    #[allow(dead_code)]
    pub(crate) fn reset_all_appearance_color_overrides(&mut self) -> bool {
        if self.base_root_config.appearance.colors.is_empty() {
            return false;
        }
        self.base_root_config.appearance.colors.clear();
        true
    }

    pub(crate) fn reset_selected_appearance_color_group_overrides(&mut self) -> usize {
        let tokens = self.selected_appearance_color_group().tokens;
        let mut removed = 0usize;
        for token in tokens {
            if self
                .base_root_config
                .appearance
                .colors
                .remove(token)
                .is_some()
            {
                removed += 1;
            }
        }
        removed
    }

    #[allow(dead_code)]
    pub(crate) fn selected_appearance_color_group_override_count(&self) -> usize {
        self.selected_appearance_color_group()
            .tokens
            .iter()
            .filter(|token| self.base_root_config.appearance.colors.get(token).is_some())
            .count()
    }
}

pub(super) fn first_appearance_color_token_index(root_config: &RootConfig) -> usize {
    COLOR_TOKEN_NAMES
        .iter()
        .position(|token| root_config.appearance.colors.get(token).is_some())
        .unwrap_or(0)
}

pub(super) fn first_appearance_color_group_index(root_config: &RootConfig) -> usize {
    COLOR_TOKEN_NAMES
        .iter()
        .find(|token| root_config.appearance.colors.get(token).is_some())
        .and_then(|token| appearance_color_group_index_for_token(token))
        .unwrap_or(0)
}

fn appearance_color_group_index_for_token(token: &str) -> Option<usize> {
    COLOR_TOKEN_GROUPS
        .iter()
        .position(|group| group.tokens.contains(&token))
}

fn normalize_hex_color_override(value: &str) -> Result<String> {
    let value = value.trim();
    if value.len() != 7
        || !value.starts_with('#')
        || !value[1..]
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        bail!("color override must be #RRGGBB");
    }
    Ok(format!("#{}", value[1..].to_ascii_uppercase()))
}
