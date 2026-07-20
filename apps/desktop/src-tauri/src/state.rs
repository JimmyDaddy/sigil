use std::{
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex},
};

use sigil_desktop::DesktopWorkspaceManager;
use tokio::sync::Mutex;

use crate::appearance::AppearanceStore;
use crate::recent::RecentWorkspaceStore;
use crate::run_streams::DesktopRunStreamOwner;

#[derive(Clone)]
pub(crate) struct DesktopAppState {
    pub(crate) manager: Arc<Mutex<DesktopWorkspaceManager>>,
    pub(crate) recent_workspaces: Arc<Mutex<RecentWorkspaceStore>>,
    pub(crate) appearance: Arc<StdMutex<AppearanceStore>>,
    pub(crate) run_streams: DesktopRunStreamOwner,
    pub(crate) sigil_binary: PathBuf,
}

impl DesktopAppState {
    pub(crate) fn new(
        sigil_binary: PathBuf,
        recent_workspaces_path: PathBuf,
        appearance: AppearanceStore,
    ) -> Self {
        Self {
            manager: Arc::new(Mutex::new(DesktopWorkspaceManager::default())),
            recent_workspaces: Arc::new(Mutex::new(RecentWorkspaceStore::new(
                recent_workspaces_path,
            ))),
            appearance: Arc::new(StdMutex::new(appearance)),
            run_streams: DesktopRunStreamOwner::default(),
            sigil_binary,
        }
    }
}
