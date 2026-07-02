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

pub(super) fn canonical_workspace_root(workspace_root: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(workspace_root).with_context(|| {
        format!(
            "failed to resolve workspace root {}",
            workspace_root.display()
        )
    })
}

pub(super) fn absolute_path_from(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

pub(super) fn lexically_normalize_path(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) => bail!("platform path prefixes are not supported"),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(component.as_os_str());
                }
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        Ok(PathBuf::from("."))
    } else {
        Ok(normalized)
    }
}

pub(super) fn resolve_existing_prefix(absolute_path: &Path) -> Result<PathBuf> {
    let mut resolved = PathBuf::new();
    for (index, component) in absolute_path.components().enumerate() {
        let candidate = if resolved.as_os_str().is_empty() {
            PathBuf::from(component.as_os_str())
        } else {
            resolved.join(component.as_os_str())
        };
        match std::fs::symlink_metadata(&candidate) {
            Ok(_) => {
                resolved = std::fs::canonicalize(&candidate)
                    .with_context(|| format!("failed to resolve {}", candidate.display()))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut missing_path = candidate;
                for remaining in absolute_path.components().skip(index + 1) {
                    missing_path.push(remaining.as_os_str());
                }
                return lexically_normalize_path(&missing_path);
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", candidate.display()));
            }
        }
    }
    Ok(resolved)
}
