use std::{path::PathBuf, sync::Arc};

use sigil_desktop::DesktopWorkspaceManager;
use tokio::sync::Mutex;

#[derive(Clone)]
pub(crate) struct DesktopAppState {
    pub(crate) manager: Arc<Mutex<DesktopWorkspaceManager>>,
    pub(crate) sigil_binary: PathBuf,
}

impl DesktopAppState {
    pub(crate) fn new(sigil_binary: PathBuf) -> Self {
        Self {
            manager: Arc::new(Mutex::new(DesktopWorkspaceManager::default())),
            sigil_binary,
        }
    }
}
