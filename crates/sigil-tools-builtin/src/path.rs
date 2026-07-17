use std::{
    ffi::OsString,
    fs,
    io::ErrorKind,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use sigil_kernel::{ToolSubject, ToolSubjectScope};

#[derive(Debug, Clone)]
pub(crate) struct ResolvedToolPath {
    pub(crate) original: String,
    pub(crate) normalized: String,
    pub(crate) canonical: PathBuf,
    pub(crate) scope: ToolSubjectScope,
}

#[derive(Debug, Clone)]
pub(crate) struct DeleteFileTarget {
    pub(crate) path: PathBuf,
    pub(crate) display_path: String,
}

pub(crate) fn resolve_workspace_path(workspace_root: &Path, requested: &str) -> Result<PathBuf> {
    Ok(resolve_tool_path(workspace_root, requested)?.canonical)
}

pub(crate) fn tool_path_subject(workspace_root: &Path, requested: &str) -> Result<ToolSubject> {
    let resolved = resolve_tool_path(workspace_root, requested)?;
    Ok(ToolSubject::path_with_scope(
        resolved.original,
        resolved.normalized,
        Some(resolved.canonical),
        resolved.scope,
    ))
}

pub(crate) fn resolve_tool_path(
    workspace_root: &Path,
    requested: &str,
) -> Result<ResolvedToolPath> {
    let workspace_root = canonical_workspace_root(workspace_root)?;
    resolve_tool_path_from_base(&workspace_root, &workspace_root, requested)
}

pub(crate) fn resolve_delete_file_target(
    workspace_root: &Path,
    requested: &str,
) -> Result<DeleteFileTarget> {
    let workspace_root = canonical_workspace_root(workspace_root)?;
    let resolved = resolve_tool_path_from_base(&workspace_root, &workspace_root, requested)?;
    if resolved.scope != ToolSubjectScope::Workspace {
        bail!("delete_file path is outside workspace: {requested}");
    }
    let requested_path = Path::new(requested);
    let path = if requested_path.is_absolute() {
        lexically_normalize_path(requested_path)?
    } else {
        lexically_normalize_path(&workspace_root.join(requested_path))?
    };
    Ok(DeleteFileTarget {
        path,
        display_path: requested.to_owned(),
    })
}

pub(crate) fn validate_delete_file_target(path: &Path, display_path: &str) -> Result<fs::Metadata> {
    let symlink_metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if symlink_metadata.file_type().is_symlink() {
        bail!("delete_file does not support symlink paths: {display_path}");
    }
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to inspect {}", path.display()))?;
    if !metadata.is_file() {
        bail!("delete_file only supports regular files: {display_path}");
    }
    Ok(metadata)
}

pub(crate) fn resolve_tool_path_from_base(
    workspace_root: &Path,
    base_dir: &Path,
    requested: &str,
) -> Result<ResolvedToolPath> {
    let requested_path = Path::new(requested);
    let lexical_target = if requested_path.is_absolute() {
        lexically_normalize_path(requested_path)?
    } else {
        lexically_normalize_path(&base_dir.join(requested_path))?
    };
    let canonical = resolve_existing_prefix(&lexical_target)?;
    let scope = if canonical.starts_with(workspace_root) {
        ToolSubjectScope::Workspace
    } else {
        ToolSubjectScope::External
    };
    let normalized = match scope {
        ToolSubjectScope::Workspace => {
            let relative = relativize(workspace_root, &canonical)?;
            if relative.is_empty() {
                ".".to_owned()
            } else {
                relative
            }
        }
        ToolSubjectScope::External => canonical.to_string_lossy().to_string(),
        ToolSubjectScope::Unknown => canonical.to_string_lossy().to_string(),
    };
    Ok(ResolvedToolPath {
        original: requested.to_owned(),
        normalized,
        canonical,
        scope,
    })
}

pub(crate) fn canonical_workspace_root(workspace_root: &Path) -> Result<PathBuf> {
    fs::canonicalize(workspace_root).with_context(|| {
        format!(
            "failed to resolve workspace root {}",
            workspace_root.display()
        )
    })
}

pub(crate) fn absolute_path_from(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

pub(crate) fn lexically_normalize_path(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                if !normalized.as_os_str().is_empty() {
                    bail!("platform path prefix must be the first component");
                }
                normalized.push(prefix.as_os_str());
            }
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

pub(crate) fn resolve_existing_prefix(absolute_path: &Path) -> Result<PathBuf> {
    let mut candidate = absolute_path.to_path_buf();
    let mut missing_suffix = Vec::<OsString>::new();
    loop {
        match fs::symlink_metadata(&candidate) {
            Ok(_) => {
                let mut resolved = fs::canonicalize(&candidate)
                    .with_context(|| format!("failed to resolve {}", candidate.display()))?;
                if !missing_suffix.is_empty()
                    && !fs::metadata(&resolved)
                        .with_context(|| format!("failed to inspect {}", resolved.display()))?
                        .is_dir()
                {
                    bail!(
                        "existing path prefix is not a directory: {}",
                        candidate.display()
                    );
                }
                for component in missing_suffix.iter().rev() {
                    resolved.push(component);
                }
                return lexically_normalize_path(&resolved);
            }
            Err(error)
                if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) =>
            {
                let Some(file_name) = candidate.file_name().map(ToOwned::to_owned) else {
                    return lexically_normalize_path(absolute_path);
                };
                missing_suffix.push(file_name);
                if !candidate.pop() {
                    return lexically_normalize_path(absolute_path);
                }
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", candidate.display()));
            }
        }
    }
}

pub(crate) fn relativize(workspace_root: &Path, path: &Path) -> Result<String> {
    Ok(path
        .strip_prefix(workspace_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/"))
}
