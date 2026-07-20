use std::{
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use tauri::Theme;
use thiserror::Error;

const APPEARANCE_SCHEMA_VERSION: u16 = 1;
const MAX_APPEARANCE_FILE_BYTES: u64 = 4 * 1024;
pub(crate) const DESKTOP_APPEARANCE_EVENT_NAME: &str = "sigil-appearance-changed";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemePreference {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub(crate) const fn native_theme(self) -> Option<Theme> {
        match self {
            Self::System => None,
            Self::Light => Some(Theme::Light),
            Self::Dark => Some(Theme::Dark),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResolvedTheme {
    Light,
    Dark,
}

impl From<Theme> for ResolvedTheme {
    fn from(theme: Theme) -> Self {
        match theme {
            Theme::Light => Self::Light,
            Theme::Dark => Self::Dark,
            _ => Self::Dark,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AppearanceSnapshot {
    pub(crate) preference: ThemePreference,
    pub(crate) resolved_theme: ResolvedTheme,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AppearanceFile {
    schema_version: u16,
    theme_preference: ThemePreference,
}

pub(crate) struct AppearanceStore {
    path: PathBuf,
    preference: ThemePreference,
}

impl AppearanceStore {
    pub(crate) fn load(path: PathBuf) -> Self {
        let preference = read_preference(&path).unwrap_or_default();
        Self { path, preference }
    }

    pub(crate) const fn preference(&self) -> ThemePreference {
        self.preference
    }

    pub(crate) fn set(&mut self, preference: ThemePreference) -> Result<(), AppearanceStoreError> {
        let bytes = serde_json::to_vec_pretty(&AppearanceFile {
            schema_version: APPEARANCE_SCHEMA_VERSION,
            theme_preference: preference,
        })
        .map_err(|_| AppearanceStoreError::Unavailable)?;
        if bytes.len() as u64 > MAX_APPEARANCE_FILE_BYTES {
            return Err(AppearanceStoreError::InvalidFile);
        }
        persist_atomically(&self.path, &bytes)?;
        self.preference = preference;
        Ok(())
    }
}

pub(crate) fn initialization_script(preference: ThemePreference) -> String {
    format!(
        "Object.defineProperty(window, '__SIGIL_THEME_PREFERENCE__', {{ value: '{}', writable: false, configurable: false }});",
        preference.as_str()
    )
}

fn read_preference(path: &Path) -> Result<ThemePreference, AppearanceStoreError> {
    let metadata = std::fs::metadata(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => AppearanceStoreError::Missing,
        _ => AppearanceStoreError::Unavailable,
    })?;
    if !metadata.is_file() || metadata.len() > MAX_APPEARANCE_FILE_BYTES {
        return Err(AppearanceStoreError::InvalidFile);
    }
    let bytes = std::fs::read(path).map_err(|_| AppearanceStoreError::Unavailable)?;
    let file: AppearanceFile =
        serde_json::from_slice(&bytes).map_err(|_| AppearanceStoreError::InvalidFile)?;
    if file.schema_version != APPEARANCE_SCHEMA_VERSION {
        return Err(AppearanceStoreError::InvalidFile);
    }
    Ok(file.theme_preference)
}

fn persist_atomically(path: &Path, bytes: &[u8]) -> Result<(), AppearanceStoreError> {
    let parent = path.parent().ok_or(AppearanceStoreError::Unavailable)?;
    std::fs::create_dir_all(parent).map_err(|_| AppearanceStoreError::Unavailable)?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|_| AppearanceStoreError::Unavailable)?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file_mut().sync_all())
        .map_err(|_| AppearanceStoreError::Unavailable)?;
    temporary
        .persist(path)
        .map_err(|_| AppearanceStoreError::Unavailable)?;
    Ok(())
}

#[derive(Debug, Error)]
pub(crate) enum AppearanceStoreError {
    #[error("appearance preference is missing")]
    Missing,
    #[error("appearance preference is unavailable")]
    Unavailable,
    #[error("appearance preference is invalid")]
    InvalidFile,
}

#[cfg(test)]
#[path = "tests/appearance_tests.rs"]
mod tests;
