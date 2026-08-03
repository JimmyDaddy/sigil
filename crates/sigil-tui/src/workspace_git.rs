use std::{path::Path, process::Command};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceGitStatus {
    pub branch: String,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub changed_entries: usize,
    pub staged_entries: usize,
    pub unstaged_entries: usize,
    pub untracked_entries: usize,
    pub conflicted_entries: usize,
}

impl WorkspaceGitStatus {
    pub(crate) fn compact_label(&self) -> String {
        let mut parts = vec![self.branch.clone(), self.change_label()];
        if self.ahead > 0 {
            parts.push(format!("ahead {}", self.ahead));
        }
        if self.behind > 0 {
            parts.push(format!("behind {}", self.behind));
        }
        parts.join(" · ")
    }

    pub(crate) fn change_label(&self) -> String {
        match self.changed_entries {
            0 => "clean".to_owned(),
            1 => "1 change".to_owned(),
            count => format!("{count} changes"),
        }
    }
}

pub(crate) fn inspect_workspace_git_status(workspace_root: &Path) -> Option<WorkspaceGitStatus> {
    let mut command = Command::new("git");
    for name in [
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_PREFIX",
        "GIT_WORK_TREE",
    ] {
        command.env_remove(name);
    }
    let output = command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("-C")
        .arg(workspace_root)
        .args([
            "status",
            "--porcelain=v1",
            "--branch",
            "--untracked-files=normal",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_workspace_git_status(&String::from_utf8(output.stdout).ok()?)
}

pub(crate) fn parse_workspace_git_status(status: &str) -> Option<WorkspaceGitStatus> {
    let mut lines = status.lines();
    let header = lines.next()?.strip_prefix("## ")?;
    let (branch, upstream, ahead, behind) = parse_branch(header);
    let mut status = WorkspaceGitStatus {
        branch,
        upstream,
        ahead,
        behind,
        changed_entries: 0,
        staged_entries: 0,
        unstaged_entries: 0,
        untracked_entries: 0,
        conflicted_entries: 0,
    };

    for line in lines {
        let code = line.as_bytes();
        if code.len() < 2 {
            continue;
        }
        let index = code[0];
        let worktree = code[1];
        status.changed_entries = status.changed_entries.saturating_add(1);
        if index == b'?' && worktree == b'?' {
            status.untracked_entries = status.untracked_entries.saturating_add(1);
        } else if is_conflicted(index, worktree) {
            status.conflicted_entries = status.conflicted_entries.saturating_add(1);
        } else {
            if index != b' ' {
                status.staged_entries = status.staged_entries.saturating_add(1);
            }
            if worktree != b' ' {
                status.unstaged_entries = status.unstaged_entries.saturating_add(1);
            }
        }
    }

    Some(status)
}

fn is_conflicted(index: u8, worktree: u8) -> bool {
    matches!(
        (index, worktree),
        (b'D', b'D')
            | (b'A', b'U')
            | (b'U', b'D')
            | (b'U', b'A')
            | (b'D', b'U')
            | (b'A', b'A')
            | (b'U', b'U')
    )
}

fn parse_branch(header: &str) -> (String, Option<String>, usize, usize) {
    let header = header.trim();
    let (reference, tracking) = header
        .split_once(" [")
        .map_or((header, None), |(reference, tracking)| {
            (reference, Some(tracking.trim_end_matches(']')))
        });
    let (branch, upstream) = if let Some(branch) = reference.strip_prefix("No commits yet on ") {
        (branch, None)
    } else if let Some(branch) = reference.strip_prefix("Initial commit on ") {
        (branch, None)
    } else if reference == "HEAD (no branch)" || reference.starts_with("HEAD (detached") {
        ("detached HEAD", None)
    } else if let Some((branch, upstream)) = reference.split_once("...") {
        (branch, Some(upstream))
    } else {
        (reference, None)
    };
    let mut ahead = 0;
    let mut behind = 0;
    if let Some(tracking) = tracking {
        for item in tracking.split(',').map(str::trim) {
            if let Some(value) = item.strip_prefix("ahead ") {
                ahead = value.parse().unwrap_or(0);
            } else if let Some(value) = item.strip_prefix("behind ") {
                behind = value.parse().unwrap_or(0);
            }
        }
    }
    (
        bounded_reference(branch),
        upstream.map(bounded_reference),
        ahead,
        behind,
    )
}

fn bounded_reference(reference: &str) -> String {
    let normalized = reference.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut characters = normalized.chars();
    let prefix = characters.by_ref().take(80).collect::<String>();
    if characters.next().is_some() {
        format!("{prefix}…")
    } else if prefix.is_empty() {
        "unknown".to_owned()
    } else {
        prefix
    }
}

#[cfg(test)]
#[path = "tests/workspace_git_tests.rs"]
mod tests;
