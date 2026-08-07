use std::path::Path;

use sigil_kernel::{CommandPermissionConfig, ConfigUpdateLockGuard, RootConfig};

/// Stable error for persisting one command-family allow rule into `permission.commands.allow`.
#[derive(Debug, thiserror::Error)]
pub enum CommandPermissionPersistError {
    #[error("config update transaction lock failed")]
    TransactionLock {
        #[source]
        source: anyhow::Error,
    },
    #[error("failed to load persisted config at {path}")]
    Load {
        path: String,
        #[source]
        source: anyhow::Error,
    },
    #[error("command pattern conflicts with an existing permission.commands rule: {message}")]
    Conflict { message: String },
    #[error("failed to persist config at {path}")]
    Save {
        path: String,
        #[source]
        source: anyhow::Error,
    },
}

/// Appends one derived command-family allow pattern (e.g. `cargo test*`) to
/// `permission.commands.allow` under the cross-process config update lock.
///
/// The pattern is whitespace-normalized, deduplicated against the existing allow list, and
/// validated for cross-group conflicts (a pattern already present in `ask`/`deny` is rejected).
/// The persisted rule takes effect for subsequent runs; the already-assembled run options are
/// not mutated.
pub fn append_command_allow_pattern(
    config_path: &Path,
    pattern: &str,
) -> Result<(), CommandPermissionPersistError> {
    let lock = ConfigUpdateLockGuard::acquire(config_path)
        .map_err(|source| CommandPermissionPersistError::TransactionLock { source })?;
    let mut config = RootConfig::load_persisted(config_path).map_err(|source| {
        CommandPermissionPersistError::Load {
            path: config_path.display().to_string(),
            source,
        }
    })?;
    let normalized = pattern.split_whitespace().collect::<Vec<_>>().join(" ");
    if config
        .permission
        .commands
        .allow
        .iter()
        .any(|existing| existing == &normalized)
    {
        return Ok(());
    }
    let mut addition = CommandPermissionConfig::default();
    addition.allow.push(normalized);
    config
        .permission
        .commands
        .extend_from(&addition)
        .map_err(|error| CommandPermissionPersistError::Conflict {
            message: error.to_string(),
        })?;
    config
        .save_with_update_lock(config_path, &lock)
        .map_err(|source| CommandPermissionPersistError::Save {
            path: config_path.display().to_string(),
            source,
        })
}

#[cfg(test)]
#[path = "tests/command_permission_tests.rs"]
mod tests;
