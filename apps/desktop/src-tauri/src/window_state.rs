use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const WINDOW_STATE_SCHEMA_VERSION: u16 = 1;
const MAX_WINDOW_STATE_FILE_BYTES: u64 = 4 * 1024;
const MIN_WINDOW_WIDTH: u32 = 1_100;
const MIN_WINDOW_HEIGHT: u32 = 720;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WindowGeometry {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) maximized: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DisplayBounds {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) scale_factor: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct InitialWindowGeometry {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) width: f64,
    pub(crate) height: f64,
    pub(crate) maximized: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WindowStateFile {
    schema_version: u16,
    geometry: WindowGeometry,
}

pub(crate) struct WindowStateStore {
    path: PathBuf,
    geometry: Option<WindowGeometry>,
}

impl WindowStateStore {
    pub(crate) fn load(path: PathBuf) -> Self {
        Self {
            geometry: read_geometry(&path).ok(),
            path,
        }
    }

    pub(crate) fn initial_geometry(
        &self,
        displays: &[DisplayBounds],
    ) -> Option<InitialWindowGeometry> {
        restore_geometry(self.geometry?, displays)
    }

    pub(crate) fn set(&mut self, geometry: WindowGeometry) -> Result<(), WindowStateStoreError> {
        validate_geometry(geometry)?;
        let bytes = serde_json::to_vec_pretty(&WindowStateFile {
            schema_version: WINDOW_STATE_SCHEMA_VERSION,
            geometry,
        })
        .map_err(|_| WindowStateStoreError::Unavailable)?;
        if bytes.len() as u64 > MAX_WINDOW_STATE_FILE_BYTES {
            return Err(WindowStateStoreError::InvalidFile);
        }
        persist_atomically(&self.path, &bytes)?;
        self.geometry = Some(geometry);
        Ok(())
    }
}

pub(crate) struct WindowStateOwner(pub(crate) Mutex<WindowStateStore>);

impl WindowStateOwner {
    pub(crate) fn load(path: PathBuf) -> Self {
        Self(Mutex::new(WindowStateStore::load(path)))
    }

    pub(crate) fn initial_geometry(
        &self,
        displays: &[DisplayBounds],
    ) -> Option<InitialWindowGeometry> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .initial_geometry(displays)
    }

    pub(crate) fn persist(&self, geometry: WindowGeometry) -> Result<(), WindowStateStoreError> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set(geometry)
    }
}

fn read_geometry(path: &Path) -> Result<WindowGeometry, WindowStateStoreError> {
    let metadata = std::fs::metadata(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => WindowStateStoreError::Missing,
        _ => WindowStateStoreError::Unavailable,
    })?;
    if !metadata.is_file() || metadata.len() > MAX_WINDOW_STATE_FILE_BYTES {
        return Err(WindowStateStoreError::InvalidFile);
    }
    let bytes = std::fs::read(path).map_err(|_| WindowStateStoreError::Unavailable)?;
    let file: WindowStateFile =
        serde_json::from_slice(&bytes).map_err(|_| WindowStateStoreError::InvalidFile)?;
    if file.schema_version != WINDOW_STATE_SCHEMA_VERSION {
        return Err(WindowStateStoreError::InvalidFile);
    }
    validate_geometry(file.geometry)?;
    Ok(file.geometry)
}

fn validate_geometry(geometry: WindowGeometry) -> Result<(), WindowStateStoreError> {
    if geometry.width < MIN_WINDOW_WIDTH
        || geometry.height < MIN_WINDOW_HEIGHT
        || geometry.width > 32_768
        || geometry.height > 32_768
        || geometry.x.unsigned_abs() > 1_000_000
        || geometry.y.unsigned_abs() > 1_000_000
    {
        return Err(WindowStateStoreError::InvalidFile);
    }
    Ok(())
}

fn restore_geometry(
    geometry: WindowGeometry,
    displays: &[DisplayBounds],
) -> Option<InitialWindowGeometry> {
    let valid_displays = displays
        .iter()
        .filter(|display| {
            display.width > 0
                && display.height > 0
                && display.scale_factor.is_finite()
                && display.scale_factor > 0.0
        })
        .collect::<Vec<_>>();
    let best_overlap = valid_displays
        .iter()
        .map(|display| intersection_area(geometry, **display))
        .max()
        .unwrap_or_default();
    let display = if best_overlap == 0 {
        *valid_displays.first()?
    } else {
        valid_displays
            .iter()
            .copied()
            .find(|display| intersection_area(geometry, **display) == best_overlap)?
    };
    let minimum_width = (f64::from(MIN_WINDOW_WIDTH) * display.scale_factor).round() as u32;
    let minimum_height = (f64::from(MIN_WINDOW_HEIGHT) * display.scale_factor).round() as u32;
    let width = geometry
        .width
        .max(minimum_width)
        .min(display.width.max(minimum_width));
    let height = geometry
        .height
        .max(minimum_height)
        .min(display.height.max(minimum_height));
    let maximum_x = i64::from(display.x) + i64::from(display.width.saturating_sub(width));
    let maximum_y = i64::from(display.y) + i64::from(display.height.saturating_sub(height));
    let x = i64::from(geometry.x).clamp(i64::from(display.x), maximum_x) as i32;
    let y = i64::from(geometry.y).clamp(i64::from(display.y), maximum_y) as i32;
    Some(InitialWindowGeometry {
        x: f64::from(x) / display.scale_factor,
        y: f64::from(y) / display.scale_factor,
        width: f64::from(width) / display.scale_factor,
        height: f64::from(height) / display.scale_factor,
        maximized: geometry.maximized,
    })
}

fn intersection_area(window: WindowGeometry, display: DisplayBounds) -> u64 {
    let left = i64::from(window.x).max(i64::from(display.x));
    let top = i64::from(window.y).max(i64::from(display.y));
    let right = (i64::from(window.x) + i64::from(window.width))
        .min(i64::from(display.x) + i64::from(display.width));
    let bottom = (i64::from(window.y) + i64::from(window.height))
        .min(i64::from(display.y) + i64::from(display.height));
    u64::try_from((right - left).max(0)).unwrap_or_default()
        * u64::try_from((bottom - top).max(0)).unwrap_or_default()
}

fn persist_atomically(path: &Path, bytes: &[u8]) -> Result<(), WindowStateStoreError> {
    let parent = path.parent().ok_or(WindowStateStoreError::Unavailable)?;
    std::fs::create_dir_all(parent).map_err(|_| WindowStateStoreError::Unavailable)?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|_| WindowStateStoreError::Unavailable)?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file_mut().sync_all())
        .map_err(|_| WindowStateStoreError::Unavailable)?;
    temporary
        .persist(path)
        .map_err(|_| WindowStateStoreError::Unavailable)?;
    Ok(())
}

#[derive(Debug, Error)]
pub(crate) enum WindowStateStoreError {
    #[error("window state is missing")]
    Missing,
    #[error("window state is unavailable")]
    Unavailable,
    #[error("window state is invalid")]
    InvalidFile,
}

#[cfg(test)]
#[path = "tests/window_state_tests.rs"]
mod tests;
