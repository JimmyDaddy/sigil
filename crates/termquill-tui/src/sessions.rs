use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct SessionHistoryEntry {
    pub path: PathBuf,
    pub label: String,
    pub modified_epoch_secs: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionViewMode {
    Provider,
    Audit,
}

impl SessionViewMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Audit => "audit",
        }
    }
}
