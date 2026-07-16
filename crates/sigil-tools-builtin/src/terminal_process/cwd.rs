use super::*;

#[derive(Debug, Clone)]
pub(super) struct ResolvedTerminalCwd {
    pub(super) relative: PathBuf,
    pub(super) absolute: PathBuf,
}

pub(super) fn resolve_terminal_cwd(
    workspace_root: &Path,
    requested: Option<&Path>,
) -> Result<ResolvedTerminalCwd> {
    let requested = requested.unwrap_or_else(|| Path::new("."));
    if requested.as_os_str().is_empty() {
        bail!("terminal cwd cannot be empty");
    }
    let candidate = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        workspace_root.join(requested)
    };
    let lexical = lexically_normalize_path(&candidate)?;
    let canonical = canonical_workspace_root(&lexical)?;
    if !canonical.starts_with(workspace_root) {
        bail!("terminal cwd is outside workspace: {}", requested.display());
    }
    let relative = if canonical == workspace_root {
        PathBuf::from(".")
    } else {
        canonical
            .strip_prefix(workspace_root)
            .unwrap_or(&canonical)
            .to_path_buf()
    };
    Ok(ResolvedTerminalCwd {
        relative,
        absolute: canonical,
    })
}
