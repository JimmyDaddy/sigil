//! Application-neutral semantic theme primitives.

use crate::CoreError;

/// A terminal-independent RGB color used by framework consumers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl ThemeColor {
    pub const fn rgb(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }
}

/// Semantic roles shared by framework consumers without prescribing a widget palette.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeRole {
    Background,
    Foreground,
    Muted,
    Accent,
    Danger,
    Success,
}

/// Bounded semantic theme selected by a host or application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticTheme {
    name: String,
    version: u16,
    background: ThemeColor,
    foreground: ThemeColor,
    muted: ThemeColor,
    accent: ThemeColor,
    danger: ThemeColor,
    success: ThemeColor,
}

impl SemanticTheme {
    pub fn new(
        name: impl Into<String>,
        version: u16,
        background: ThemeColor,
        foreground: ThemeColor,
        muted: ThemeColor,
        accent: ThemeColor,
        danger: ThemeColor,
        success: ThemeColor,
    ) -> Result<Self, CoreError> {
        let name = name.into();
        if name.is_empty() || name.len() > 128 || version == 0 {
            return Err(CoreError::InvalidValue(
                "theme identity must be bounded and versioned",
            ));
        }
        Ok(Self {
            name,
            version,
            background,
            foreground,
            muted,
            accent,
            danger,
            success,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn version(&self) -> u16 {
        self.version
    }

    pub const fn color(&self, role: ThemeRole) -> ThemeColor {
        match role {
            ThemeRole::Background => self.background,
            ThemeRole::Foreground => self.foreground,
            ThemeRole::Muted => self.muted,
            ThemeRole::Accent => self.accent,
            ThemeRole::Danger => self.danger,
            ThemeRole::Success => self.success,
        }
    }
}

impl Default for SemanticTheme {
    fn default() -> Self {
        Self {
            name: "default".to_owned(),
            version: 1,
            background: ThemeColor::rgb(0, 0, 0),
            foreground: ThemeColor::rgb(240, 240, 240),
            muted: ThemeColor::rgb(140, 140, 140),
            accent: ThemeColor::rgb(90, 170, 255),
            danger: ThemeColor::rgb(240, 90, 90),
            success: ThemeColor::rgb(90, 210, 130),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_theme_is_versioned_and_role_based() {
        let theme = SemanticTheme::default();
        assert_eq!(theme.name(), "default");
        assert_eq!(theme.version(), 1);
        assert_eq!(
            theme.color(ThemeRole::Accent),
            ThemeColor::rgb(90, 170, 255)
        );
        assert!(
            SemanticTheme::new(
                "",
                1,
                theme.background,
                theme.foreground,
                theme.muted,
                theme.accent,
                theme.danger,
                theme.success,
            )
            .is_err()
        );
    }
}
