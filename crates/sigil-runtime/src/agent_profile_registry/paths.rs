use std::{
    fs,
    path::{Path, PathBuf},
};

use sigil_kernel::AgentProfileSource;

#[cfg(test)]
pub(super) fn configured_dir(workspace_root: &Path, configured: &str) -> PathBuf {
    let path = Path::new(configured);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    }
}

pub(super) fn workspace_path(workspace_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    }
}

pub(super) fn path_stays_in_workspace(canonical_workspace_root: &Path, path: &Path) -> bool {
    path.canonicalize()
        .map(|canonical| canonical.starts_with(canonical_workspace_root))
        .unwrap_or(false)
}

pub(super) fn sorted_dir_entries(dir: &Path, warnings: &mut Vec<String>) -> Vec<fs::DirEntry> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!(
                "failed to read workspace agent discovery directory {}: {error}",
                dir.display()
            ));
            return Vec::new();
        }
    };
    let mut entries = entries.filter_map(|entry| entry.ok()).collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    entries
}

pub(super) fn display_path(workspace_root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(workspace_root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf())
}

pub(super) fn agent_profile_source_label(source: &AgentProfileSource) -> &'static str {
    match source {
        AgentProfileSource::Workspace => "workspace",
        AgentProfileSource::User => "user",
        AgentProfileSource::Plugin { .. } => "plugin",
        AgentProfileSource::Compatibility { .. } => "compatibility",
        AgentProfileSource::System => "system",
        AgentProfileSource::Unknown => "unknown",
    }
}
