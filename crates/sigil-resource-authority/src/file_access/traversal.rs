//! Cross-platform traversal keeps every file open relative to an admitted directory handle.

use std::{path::Path, sync::Arc};

use sigil_kernel::managed_file_access::{ManagedFileAccessErrorV1, ManagedFileExecutionContextV1};

use super::{
    MAX_DIRECTORY_SCAN_DEPTH, MAX_DIRECTORY_SCAN_ENTRIES, PlannedFileAccessV1, relative_io_error,
    streaming,
};

/// Observe an existing leaf through the same non-following component walk used by execution.
/// An absent component is a create target; an alias is never treated as an absent component.
pub(super) fn plan_snapshot(
    root: &Path,
    logical: &str,
    root_handle: &std::fs::File,
    capture_content: bool,
) -> Result<
    (
        Option<sigil_kernel::resource::CanonicalHash>,
        Option<sigil_kernel::resource::CanonicalHash>,
    ),
    ManagedFileAccessErrorV1,
> {
    let mut file = root_handle.try_clone().map_err(relative_io_error)?;
    let mut path = root.to_path_buf();
    for name in logical
        .split('/')
        .filter(|name| !name.is_empty() && *name != ".")
    {
        path.push(name);
        file = match open_child(&file, name, &path) {
            Ok(file) => file,
            Err(ManagedFileAccessErrorV1::NotFound) => return Ok((None, None)),
            Err(error) => return Err(error),
        };
    }
    #[cfg(windows)]
    let identity = crate::identity::canonical_identity_from_handle(&path, &file)
        .map_err(|error| ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string()))?;
    #[cfg(not(windows))]
    let identity = crate::identity::canonical_identity_from_metadata(
        &path,
        &file.metadata().map_err(relative_io_error)?,
    );
    let digest = if capture_content && identity.is_regular_file {
        Some(super::hash_file(file, None)?)
    } else {
        None
    };
    Ok((Some(identity.digest), digest))
}

pub(super) fn open_plan_file(
    plan: &PlannedFileAccessV1,
) -> Result<(std::fs::File, Vec<std::fs::File>), ManagedFileAccessErrorV1> {
    #[cfg(unix)]
    {
        Ok((super::open_relative_path(plan, libc::O_RDONLY)?, Vec::new()))
    }
    #[cfg(windows)]
    {
        let handle = super::windows_open_plan(plan, super::WindowsOpenKind::Any, false)?;
        Ok((handle.file, handle._ancestors))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = plan;
        Err(ManagedFileAccessErrorV1::OperationNotPermitted)
    }
}

pub(super) fn open_child(
    directory: &std::fs::File,
    name: &str,
    path: &Path,
) -> Result<std::fs::File, ManagedFileAccessErrorV1> {
    #[cfg(unix)]
    {
        let file = super::open_at(directory, name, libc::O_RDONLY, 0).map_err(relative_io_error)?;
        let identity = crate::identity::canonical_identity_from_metadata(
            path,
            &file.metadata().map_err(relative_io_error)?,
        );
        if identity.is_symlink
            || (!identity.is_directory && !identity.is_regular_file)
            || (identity.is_regular_file && identity.link_count > 1)
        {
            return Err(ManagedFileAccessErrorV1::AliasCollision);
        }
        Ok(file)
    }
    #[cfg(windows)]
    {
        let _ = path;
        super::windows_open_relative_component(directory, name, super::WindowsOpenKind::Any, false)
            .map_err(relative_io_error)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (directory, name, path);
        Err(ManagedFileAccessErrorV1::OperationNotPermitted)
    }
}

/// Classifies a name without requiring read access to its contents. Reads reopen through
/// `open_child`, so this metadata observation never permits following an alias.
fn child_is_directory(
    directory: &std::fs::File,
    name: &str,
    path: &Path,
) -> Result<bool, ManagedFileAccessErrorV1> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let _ = path;
        let name =
            std::ffi::CString::new(name).map_err(|_| ManagedFileAccessErrorV1::AliasCollision)?;
        let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the parent descriptor and name remain live; fstatat initializes the entire
        // stat structure on success, and AT_SYMLINK_NOFOLLOW never follows the leaf.
        let result = unsafe {
            libc::fstatat(
                directory.as_raw_fd(),
                name.as_ptr(),
                metadata.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            return Err(relative_io_error(std::io::Error::last_os_error()));
        }
        // SAFETY: the successful fstatat call initialized metadata.
        let metadata = unsafe { metadata.assume_init() };
        let kind = metadata.st_mode & libc::S_IFMT;
        if kind == libc::S_IFDIR {
            Ok(true)
        } else if kind == libc::S_IFREG && metadata.st_nlink == 1 {
            Ok(false)
        } else {
            Err(ManagedFileAccessErrorV1::AliasCollision)
        }
    }
    #[cfg(windows)]
    {
        let file = super::windows_open_relative_component(
            directory,
            name,
            super::WindowsOpenKind::Metadata,
            false,
        )
        .map_err(relative_io_error)?;
        let identity =
            crate::identity::canonical_identity_from_handle(path, &file).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
        if identity.is_directory {
            Ok(true)
        } else if identity.is_regular_file {
            Ok(false)
        } else {
            Err(ManagedFileAccessErrorV1::AliasCollision)
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (directory, name, path);
        Err(ManagedFileAccessErrorV1::OperationNotPermitted)
    }
}

pub(super) fn names(
    directory: &std::fs::File,
    path: &Path,
    max_entries: usize,
    context: &ManagedFileExecutionContextV1,
) -> Result<Vec<String>, ManagedFileAccessErrorV1> {
    #[cfg(unix)]
    {
        let _ = path;
        super::directory_entry_names(directory, max_entries, context)
    }
    #[cfg(windows)]
    {
        let _ = directory;
        std::fs::read_dir(path)
            .map_err(relative_io_error)?
            .take(max_entries)
            .map(|entry| {
                streaming::check_context(context)?;
                entry
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .map_err(relative_io_error)
            })
            .collect()
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (directory, path, max_entries, context);
        Err(ManagedFileAccessErrorV1::OperationNotPermitted)
    }
}

#[derive(Default)]
pub(super) struct WalkBudget {
    scanned: usize,
    pub(super) truncated: bool,
}

#[derive(Clone, Copy)]
struct WalkOptions<'a> {
    depth: usize,
    recursive: bool,
    max_depth: usize,
    respect_gitignore: bool,
    hidden: bool,
    context: &'a ManagedFileExecutionContextV1,
}

fn walk(
    directory: &std::fs::File,
    root: &Path,
    prefix: &str,
    options: &WalkOptions<'_>,
    inherited_ignores: &[Arc<ignore::gitignore::Gitignore>],
    budget: &mut WalkBudget,
    visit: &mut impl FnMut(&std::fs::File, &str, &str, bool) -> Result<(), ManagedFileAccessErrorV1>,
) -> Result<(), ManagedFileAccessErrorV1> {
    streaming::check_context(options.context)?;
    let path = root.join(prefix);
    let mut ignores = inherited_ignores.to_vec();
    if options.respect_gitignore
        && let Some(local) = read_directory_ignore(directory, &path, options.context)?
    {
        ignores.push(local);
    }
    let remaining = MAX_DIRECTORY_SCAN_ENTRIES.saturating_sub(budget.scanned);
    let mut names = names(
        directory,
        &path,
        remaining.saturating_add(1),
        options.context,
    )?;
    if names.len() > remaining {
        names.truncate(remaining);
        budget.truncated = true;
    }
    budget.scanned = budget.scanned.saturating_add(names.len());
    names.sort();
    for name in names {
        streaming::check_context(options.context)?;
        if !options.hidden && name.starts_with('.') {
            continue;
        }
        let relative = if prefix.is_empty() || prefix == "." {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let entry_path = root.join(&relative);
        let is_directory = match child_is_directory(directory, &name, &entry_path) {
            Ok(is_directory) => is_directory,
            Err(ManagedFileAccessErrorV1::AliasCollision | ManagedFileAccessErrorV1::NotFound) => {
                continue;
            }
            Err(ManagedFileAccessErrorV1::PermissionDenied) => {
                budget.truncated = true;
                continue;
            }
            Err(error) => return Err(error),
        };
        let ignored = ignores
            .iter()
            .rev()
            .find_map(|ignore| {
                match ignore.matched_path_or_any_parents(&entry_path, is_directory) {
                    ignore::Match::None => None,
                    matched => Some(matched.is_ignore()),
                }
            })
            .unwrap_or(false);
        if ignored {
            continue;
        }
        visit(directory, &name, &relative, is_directory)?;
        if options.recursive && is_directory {
            if options.depth < options.max_depth {
                let file = match open_child(directory, &name, &entry_path) {
                    Ok(file) => file,
                    Err(
                        ManagedFileAccessErrorV1::AliasCollision
                        | ManagedFileAccessErrorV1::NotFound,
                    ) => continue,
                    Err(ManagedFileAccessErrorV1::PermissionDenied) => {
                        budget.truncated = true;
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                walk(
                    &file,
                    root,
                    &relative,
                    &WalkOptions {
                        depth: options.depth + 1,
                        ..*options
                    },
                    &ignores,
                    budget,
                    visit,
                )?;
            } else {
                budget.truncated = true;
            }
        }
    }
    Ok(())
}

/// Read only this directory's ignore policy, from its admitted parent handle. Each matcher owns
/// its actual directory base; `add_line`'s source filename alone does not scope nested patterns.
fn read_directory_ignore(
    directory: &std::fs::File,
    path: &Path,
    context: &ManagedFileExecutionContextV1,
) -> Result<Option<Arc<ignore::gitignore::Gitignore>>, ManagedFileAccessErrorV1> {
    streaming::check_context(context)?;
    let ignore_path = path.join(".gitignore");
    match child_is_directory(directory, ".gitignore", &ignore_path) {
        Ok(false) => {}
        Ok(true)
        | Err(ManagedFileAccessErrorV1::NotFound | ManagedFileAccessErrorV1::AliasCollision) => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    }
    let mut file = open_child(directory, ".gitignore", &ignore_path)?;
    let (text, truncated) = super::read_text_with_budget(&mut file, 1024 * 1024)?;
    streaming::check_context(context)?;
    if truncated {
        return Err(ManagedFileAccessErrorV1::ResourceLimit(
            "ignore file exceeds the content limit".to_owned(),
        ));
    }
    let mut builder = ignore::gitignore::GitignoreBuilder::new(path);
    for line in text.lines() {
        builder
            .add_line(Some(ignore_path.clone()), line)
            .map_err(|error| ManagedFileAccessErrorV1::InvalidInput(error.to_string()))?;
    }
    builder
        .build()
        .map(Arc::new)
        .map(Some)
        .map_err(|error| ManagedFileAccessErrorV1::InvalidInput(error.to_string()))
}

/// A scoped grep/list starts with only its real ancestors' policies, without scanning sibling
/// trees or opening unrelated file contents to discover ignore files.
fn ancestor_ignores(
    plan: &PlannedFileAccessV1,
    context: &ManagedFileExecutionContextV1,
) -> Result<Vec<Arc<ignore::gitignore::Gitignore>>, ManagedFileAccessErrorV1> {
    let components: Vec<_> = plan
        .logical_path
        .split('/')
        .filter(|name| !name.is_empty() && *name != ".")
        .collect();
    let mut ignores = Vec::new();
    if components.is_empty() {
        return Ok(ignores);
    }
    #[cfg(any(unix, windows))]
    let mut directory = plan.root_handle.try_clone().map_err(relative_io_error)?;
    #[cfg(not(any(unix, windows)))]
    let mut directory = open_plan_file(plan)?.0;
    let mut path = plan.root.clone();
    for (index, name) in components.iter().enumerate() {
        streaming::check_context(context)?;
        if index >= MAX_DIRECTORY_SCAN_DEPTH {
            return Err(ManagedFileAccessErrorV1::ResourceLimit(
                "ignore ancestor depth exceeded".to_owned(),
            ));
        }
        if let Some(ignore) = read_directory_ignore(&directory, &path, context)? {
            ignores.push(ignore);
        }
        if index + 1 < components.len() {
            path.push(name);
            directory = open_child(&directory, name, &path)?;
        }
    }
    Ok(ignores)
}

pub(super) fn collect_plan_entries(
    plan: &PlannedFileAccessV1,
    recursive: bool,
    max_depth: usize,
    entries: &mut Vec<String>,
    respect_gitignore: bool,
    budget: &mut WalkBudget,
    context: &ManagedFileExecutionContextV1,
) -> Result<(), ManagedFileAccessErrorV1> {
    let (directory, _ancestors) = open_plan_file(plan)?;
    if !directory.metadata().map_err(relative_io_error)?.is_dir() {
        return Err(ManagedFileAccessErrorV1::InvalidInput(
            "list path must be a directory".to_owned(),
        ));
    }
    let prefix = if plan.logical_path == "." {
        ""
    } else {
        &plan.logical_path
    };
    let ignores = if respect_gitignore {
        ancestor_ignores(plan, context)?
    } else {
        Vec::new()
    };
    walk(
        &directory,
        &plan.root,
        prefix,
        &WalkOptions {
            depth: 0,
            recursive,
            max_depth,
            respect_gitignore,
            hidden: !respect_gitignore,
            context,
        },
        &ignores,
        budget,
        &mut |_, _, relative, _| {
            entries.push(relative.to_owned());
            Ok(())
        },
    )
}

pub(super) fn collect_plan_grep(
    plan: &PlannedFileAccessV1,
    regex: &regex::Regex,
    stream: &mut streaming::GrepStream,
    context: &ManagedFileExecutionContextV1,
) -> Result<(), ManagedFileAccessErrorV1> {
    let (target, _ancestors) = open_plan_file(plan)?;
    if !target.metadata().map_err(relative_io_error)?.is_dir() {
        return stream.scan(target, &plan.logical_path, regex, context);
    }
    let ignores = ancestor_ignores(plan, context)?;
    let mut budget = WalkBudget::default();
    let prefix = if plan.logical_path == "." {
        ""
    } else {
        &plan.logical_path
    };
    walk(
        &target,
        &plan.root,
        prefix,
        &WalkOptions {
            depth: 0,
            recursive: true,
            max_depth: MAX_DIRECTORY_SCAN_DEPTH,
            respect_gitignore: true,
            hidden: false,
            context,
        },
        &ignores,
        &mut budget,
        &mut |directory, name, relative, is_directory| {
            if !is_directory {
                let file = match open_child(directory, name, &plan.root.join(relative)) {
                    Ok(file) => file,
                    Err(
                        ManagedFileAccessErrorV1::AliasCollision
                        | ManagedFileAccessErrorV1::NotFound,
                    ) => return Ok(()),
                    Err(ManagedFileAccessErrorV1::PermissionDenied) => {
                        stream.truncated = true;
                        return Ok(());
                    }
                    Err(error) => return Err(error),
                };
                stream.scan(file, relative, regex, context)?;
            }
            Ok(())
        },
    )?;
    stream.truncated |= budget.truncated;
    Ok(())
}
