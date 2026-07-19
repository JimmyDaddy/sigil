use std::{path::PathBuf, sync::Arc};

use sigil_desktop::DesktopWorkspaceManager;
use tokio::sync::Mutex;

use crate::recent::RecentWorkspaceStore;

#[derive(Clone)]
pub(crate) struct DesktopAppState {
    pub(crate) manager: Arc<Mutex<DesktopWorkspaceManager>>,
    pub(crate) recent_workspaces: Arc<Mutex<RecentWorkspaceStore>>,
    pub(crate) sigil_binary: PathBuf,
}

impl DesktopAppState {
    pub(crate) fn new(sigil_binary: PathBuf, recent_workspaces_path: PathBuf) -> Self {
        Self {
            manager: Arc::new(Mutex::new(DesktopWorkspaceManager::default())),
            recent_workspaces: Arc::new(Mutex::new(RecentWorkspaceStore::new(
                recent_workspaces_path,
            ))),
            sigil_binary,
        }
    }
}
