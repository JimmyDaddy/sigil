//! Request-local repository enumeration with a budget before filtering and sorting.

use std::{
    fs,
    io::{self, BufRead},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Result;
use ignore::gitignore::{Gitignore, GitignoreBuilder};

struct DirectoryRules {
    parent: Option<Arc<Self>>,
    ignore: Gitignore,
    gitignore: Gitignore,
    exclude: Gitignore,
}

impl DirectoryRules {
    fn load(directory: &Path, parent: Option<Arc<Self>>) -> Arc<Self> {
        Arc::new(Self {
            parent,
            ignore: load_ignore(directory, &directory.join(".ignore")),
            gitignore: load_ignore(directory, &directory.join(".gitignore")),
            exclude: git_exclude_path(directory)
                .map_or_else(Gitignore::empty, |path| load_ignore(directory, &path)),
        })
    }

    fn ignored(&self, path: &Path, is_directory: bool, global: &Gitignore) -> bool {
        // Match WalkBuilder's .ignore > .gitignore > .git/info/exclude > global precedence.
        // Within each class the nearest matching directory wins, including negations. As with
        // require_git(false), parent policies also apply outside and across nested repositories.
        for select in [
            (|rules: &Self| &rules.ignore) as fn(&Self) -> &Gitignore,
            |rules: &Self| &rules.gitignore,
            |rules: &Self| &rules.exclude,
        ] {
            let mut current = Some(self);
            while let Some(rules) = current {
                let matched = select(rules).matched(path, is_directory);
                if !matched.is_none() {
                    return matched.is_ignore();
                }
                current = rules.parent.as_deref();
            }
        }
        global.matched(path, is_directory).is_ignore()
    }
}

fn git_exclude_path(directory: &Path) -> Option<PathBuf> {
    let git_path = directory.join(".git");
    let metadata = fs::metadata(&git_path).ok()?;
    if metadata.is_dir() {
        return Some(git_path.join("info/exclude"));
    }
    if !metadata.is_file() {
        return None;
    }
    // Linked worktrees store their per-worktree gitdir in .git and share ignore policy through
    // that gitdir's commondir. Resolve both relative pointers from the file that contains them.
    let git_pointer = first_line(&git_path)?;
    let git_dir = directory.join(git_pointer.strip_prefix("gitdir: ")?);
    let common_dir = first_line(&git_dir.join("commondir"))
        .map_or_else(|| git_dir.clone(), |common| git_dir.join(common));
    Some(common_dir.join("info/exclude"))
}

fn first_line(path: &Path) -> Option<String> {
    io::BufReader::new(fs::File::open(path).ok()?)
        .lines()
        .next()?
        .ok()
}

fn load_ignore(directory: &Path, path: &Path) -> Gitignore {
    let mut builder = GitignoreBuilder::new(directory);
    // Preserve valid rules from partially malformed files, as ignore::WalkBuilder does.
    let _ = builder.add(path);
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

struct RepositoryEntry {
    path: PathBuf,
    file_type: fs::FileType,
}

fn read_directory(
    directory: &Path,
) -> io::Result<impl Iterator<Item = io::Result<RepositoryEntry>> + use<>> {
    Ok(fs::read_dir(directory)?.map(|entry| {
        let entry = entry?;
        Ok(RepositoryEntry {
            path: entry.path(),
            file_type: entry.file_type()?,
        })
    }))
}

/// Visits regular repository files within a shared raw directory-entry budget.
///
/// `workspace_root` is the caller's canonical workspace. Ignore policies include parent `.ignore`,
/// `.gitignore`, Git `info/exclude` (including linked-worktree common directories), and the global
/// Git ignore file, with the same precedence as
/// `WalkBuilder` with `hidden(false)` and `require_git(false)`. Symbolic links are not followed.
/// `skip` receives workspace-relative paths and prunes both files and directories. Returning false
/// from `visit` stops collection, for example when the caller's file budget is exhausted.
///
/// Every raw entry, including ignored entries and enumeration errors, consumes the budget before
/// filtering. Each directory's bounded prefix is sorted before visiting; an exhausted budget does
/// not promise the globally lexicographically first files. There is no depth limit. The return
/// value counts raw entries consumed. This observational helper grants no filesystem authority.
///
/// # Errors
/// Returns errors from `visit`. Unreadable individual directories and entries are skipped.
pub fn visit_repository_files(
    workspace_root: &Path,
    max_entries: usize,
    skip: impl Fn(&Path) -> bool,
    visit: impl FnMut(&Path) -> Result<bool>,
) -> Result<usize> {
    let (global, _) = Gitignore::global();
    visit_repository_files_with_reader(
        workspace_root,
        max_entries,
        skip,
        visit,
        &global,
        read_directory,
    )
}

fn visit_repository_files_with_reader<I>(
    workspace_root: &Path,
    max_entries: usize,
    skip: impl Fn(&Path) -> bool,
    mut visit: impl FnMut(&Path) -> Result<bool>,
    global: &Gitignore,
    mut read: impl FnMut(&Path) -> io::Result<I>,
) -> Result<usize>
where
    I: Iterator<Item = io::Result<RepositoryEntry>>,
{
    if max_entries == 0 {
        return Ok(0);
    }
    let mut inherited = None;
    let ancestors = workspace_root.ancestors().skip(1).collect::<Vec<_>>();
    for ancestor in ancestors.into_iter().rev() {
        inherited = Some(DirectoryRules::load(ancestor, inherited));
    }
    let mut pending = vec![(workspace_root.to_path_buf(), true, inherited)];
    let mut consumed = 0usize;
    while let Some((path, is_directory, rules)) = pending.pop() {
        if !is_directory {
            if !visit(&path)? {
                break;
            }
            continue;
        }
        let remaining = max_entries.saturating_sub(consumed);
        if remaining == 0 {
            continue;
        }
        let rules = DirectoryRules::load(&path, rules);
        let Ok(entries) = read(&path) else {
            continue;
        };
        // Take before collecting: neither sorting nor ignored entries can hide unbounded reads.
        let entries = entries.take(remaining).collect::<Vec<_>>();
        consumed += entries.len();
        let mut entries = entries.into_iter().flatten().collect::<Vec<_>>();
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        for entry in entries.into_iter().rev() {
            if entry.file_type.is_symlink()
                || (!entry.file_type.is_file() && !entry.file_type.is_dir())
            {
                continue;
            }
            let Ok(relative) = entry.path.strip_prefix(workspace_root) else {
                continue;
            };
            let is_directory = entry.file_type.is_dir();
            if skip(relative) || rules.ignored(&entry.path, is_directory, global) {
                continue;
            }
            pending.push((entry.path, is_directory, Some(Arc::clone(&rules))));
        }
    }
    Ok(consumed)
}

#[cfg(test)]
#[path = "tests/repository_walk_tests.rs"]
mod tests;
