//! Explicit capability vocabulary for degraded and authority-backed agent runs.
//!
//! The capability is intentionally separate from provider identity. A provider may still be
//! reachable while the local workspace or durable session authority is unavailable.

use std::path::Path;

/// Whether a run may inspect or mutate a concrete workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceCapability {
    Available,
    Unavailable,
}

impl WorkspaceCapability {
    #[must_use]
    pub fn from_path(path: &Path) -> Self {
        if path.as_os_str().is_empty() {
            Self::Unavailable
        } else {
            Self::Available
        }
    }

    #[must_use]
    pub const fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }
}

/// Whether session facts survive a process restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPersistenceCapability {
    Durable,
    Ephemeral,
}

impl SessionPersistenceCapability {
    #[must_use]
    pub const fn is_durable(self) -> bool {
        matches!(self, Self::Durable)
    }
}
