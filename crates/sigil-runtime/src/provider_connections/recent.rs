use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sigil_kernel::{ModelRef, RootConfig, atomic_publish_private_file};

use super::{load_provider_connections, resolve_model_route};

const RECENT_MODEL_LIMIT: usize = 20;
const RECENT_MODEL_FILE_MAX_BYTES: u64 = 32 * 1024;
const RECENT_MODEL_STATE_VERSION: u32 = 1;

fn recent_store_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecentModelsWire {
    version: u32,
    models: Vec<ModelRef>,
}

#[must_use]
pub fn recent_models_path(state_root: &Path) -> PathBuf {
    state_root
        .join("provider-models")
        .join("v1")
        .join("recent.json")
}

pub fn load_recent_model_refs(state_root: &Path, root_config: &RootConfig) -> Vec<ModelRef> {
    let _guard = match recent_store_lock().lock() {
        Ok(guard) => guard,
        Err(_) => return Vec::new(),
    };
    load_recent_model_refs_locked(state_root, root_config)
}

pub fn record_recent_model_ref(
    state_root: &Path,
    root_config: &RootConfig,
    model_ref: &ModelRef,
) -> Result<()> {
    resolve_model_route(root_config, model_ref).map_err(anyhow::Error::new)?;
    let _guard = recent_store_lock()
        .lock()
        .map_err(|_| anyhow::anyhow!("recent model store lock poisoned"))?;
    let mut models = load_recent_model_refs_locked(state_root, root_config);
    models.retain(|candidate| candidate != model_ref);
    models.insert(0, model_ref.clone());
    models.truncate(RECENT_MODEL_LIMIT);
    publish_recent_models(state_root, &models)
}

fn load_recent_model_refs_locked(state_root: &Path, root_config: &RootConfig) -> Vec<ModelRef> {
    let path = recent_models_path(state_root);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(_) => return Vec::new(),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > RECENT_MODEL_FILE_MAX_BYTES
    {
        return Vec::new();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Vec::new();
        }
    }
    let wire = fs::read(&path)
        .ok()
        .filter(|bytes| bytes.len() as u64 <= RECENT_MODEL_FILE_MAX_BYTES)
        .and_then(|bytes| serde_json::from_slice::<RecentModelsWire>(&bytes).ok());
    let Some(wire) = wire.filter(|wire| wire.version == RECENT_MODEL_STATE_VERSION) else {
        return Vec::new();
    };
    let configured = load_provider_connections(root_config);
    let mut models = Vec::new();
    for model_ref in wire.models {
        if models.len() >= RECENT_MODEL_LIMIT {
            break;
        }
        if configured
            .connections
            .contains_key(&model_ref.connection_id)
            && !models.contains(&model_ref)
        {
            models.push(model_ref);
        }
    }
    models
}

fn publish_recent_models(state_root: &Path, models: &[ModelRef]) -> Result<()> {
    let path = recent_models_path(state_root);
    secure_recent_tree(state_root)?;
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        anyhow::ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "recent model state target must be a regular non-symlink file"
        );
    }
    let bytes = serde_json::to_vec(&RecentModelsWire {
        version: RECENT_MODEL_STATE_VERSION,
        models: models.to_vec(),
    })
    .context("failed to encode recent model state")?;
    anyhow::ensure!(
        bytes.len() as u64 <= RECENT_MODEL_FILE_MAX_BYTES,
        "recent model state exceeds its size limit"
    );
    atomic_publish_private_file(&path, &bytes)
}

fn secure_recent_tree(state_root: &Path) -> Result<()> {
    if !state_root.exists() {
        fs::create_dir_all(state_root)
            .with_context(|| format!("failed to create {}", state_root.display()))?;
    }
    let mut current = state_root.to_path_buf();
    for component in ["provider-models", "v1"] {
        let metadata = fs::symlink_metadata(&current)
            .with_context(|| format!("failed to inspect {}", current.display()))?;
        anyhow::ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "recent model state path is not a private directory"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&current, fs::Permissions::from_mode(0o700))
                .with_context(|| format!("failed to secure {}", current.display()))?;
        }
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => anyhow::ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "recent model state path is not a private directory"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current)
                    .with_context(|| format!("failed to create {}", current.display()))?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&current, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to secure {}", current.display()))?;
    }
    Ok(())
}
